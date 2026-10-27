// The editing surface. One contenteditable holds every block, so selections,
// arrow keys and spell checking work across blocks the way the browser does it.
// The browser handles typing inside a part; everything structural (Enter,
// Backspace at the start of a block, Tab, deleting across blocks, formatting,
// pasting) is intercepted and turned into document operations (edits.js), which
// are applied locally and sent as one undo step.
import { parseInline, plainText, serializeRuns, markState, setMark, insertText, inlineShortcut } from './inline.js';
import * as M from './model.js';
import * as E from './edits.js';
import {
  endBreak as endBreakNode, readRuns, readPlain, renderRuns, setCaret, pointAt, partElement, blockElement, locationOf,
  findPart, placeCaret, insertAtCaret,
} from './dom.js';
import { renderBlock } from './blocks.js';
import { htmlToMarkdown, looksLikeMarkdown } from './paste.js';

const DELIMITERS = new Set(['*', '_', '~', '`']);
const keyOf = (block, part) => `${block}\u0000${part}`;

export function editorController(api) {
  const { state, surface } = api;
  const records = new Map();
  const dirty = new Map();
  let timer = 0, composing = false, plainNext = null, selectedBlock = null;

  // -- rendering -------------------------------------------------------------------

  const lonelyId = () => {
    const blocks = state.doc.blocks;
    if (blocks.length > 2 || !blocks.every(b => b.kind === 'title' || (b.kind === 'paragraph' && !b.text))) return null;
    return blocks.find(b => b.kind === 'paragraph')?.id ?? null;
  };
  const context = () => ({ doc: state.doc, assets: state.assets, decos: state.decos, lonely: lonelyId() });
  const signature = (b, ctx = context()) => JSON.stringify([
    b, ctx.decos[b.id] ?? null, b.kind === 'toc' ? M.outline(ctx.doc).map(e => [e.id, e.level, e.text]) : 0,
    b.kind === 'image' ? ctx.assets[b.src] ?? 0 : 0, ctx.lonely === b.id,
  ]);

  /** Bring the page in line with `state.doc`, replacing only blocks that changed. */
  function reconcile() {
    const saved = captureSelection();
    const keepSelected = selectedBlock;
    const ctx = context(), blocks = state.doc.blocks;
    surface.querySelectorAll('[data-ghost]').forEach(node => node.remove());
    let cursor = surface.firstElementChild;
    const seen = new Set();
    for (const b of blocks) {
      seen.add(b.id);
      const sig = signature(b, ctx);
      let record = records.get(b.id);
      if (!record || record.sig !== sig) {
        const node = renderBlock(b, ctx);
        if (record) {
          if (record.el === cursor) cursor = cursor.nextElementSibling;
          record.el.remove();
        }
        record = { el: node, sig };
        records.set(b.id, record);
      }
      if (record.el === cursor) cursor = cursor.nextElementSibling;
      else surface.insertBefore(record.el, cursor);
    }
    for (const [id, record] of records) if (!seen.has(id)) { record.el.remove(); records.delete(id); }
    if (!blocks.length) renderGhosts();
    surface.classList.toggle('empty', !blocks.length);
    restoreSelection(saved);
    if (keepSelected && !surface.querySelector(`[data-block="${CSS.escape(keepSelected)}"]`)) selectedBlock = null;
    else if (keepSelected) surface.querySelector(`[data-block="${CSS.escape(keepSelected)}"]`).classList.add('selected');
    api.afterRender();
  }

  function renderGhosts() {
    for (const [kind, placeholder] of [['title', 'Untitled'], ['paragraph', 'Start writing, or press / for blocks']]) {
      const wrap = document.createElement('div');
      wrap.className = `blk blk-${kind} ghost`;
      wrap.dataset.ghost = kind;
      const node = document.createElement(kind === 'title' ? 'h1' : 'p');
      node.className = 'part';
      node.dataset.ts = kind === 'title' ? 'title' : 'body';
      node.dataset.ph = placeholder;
      wrap.append(node);
      surface.append(wrap);
    }
  }

  /** The record of a block, to be told its DOM already shows a change. */
  function accept(id) {
    const record = records.get(id), b = M.block(state.doc, id);
    if (record && b) record.sig = signature(b);
  }

  // -- selection -------------------------------------------------------------------

  /** Selection endpoints on the surface itself (select all) become block edges. */
  function normalized(range) {
    const point = (node, offset, start) => {
      if (node !== surface) return [node, offset];
      const child = surface.children[Math.min(offset, surface.children.length - 1)];
      if (!child) return [node, offset];
      const after = !start && offset >= surface.children.length;
      const edge = [...child.querySelectorAll('[data-part]')];
      const target = after ? edge.at(-1) : edge[0];
      if (!target) return [child, after ? child.childNodes.length : 0];
      return after ? [target, target.childNodes.length] : [target, 0];
    };
    return [...point(range.startContainer, range.startOffset, true), ...point(range.endContainer, range.endOffset, false)];
  }

  function selection() {
    const s = getSelection();
    if (!s.rangeCount) return null;
    const range = s.getRangeAt(0);
    if (!surface.contains(range.startContainer) || !surface.contains(range.endContainer)) return null;
    const [sn, so, en, eo] = normalized(range);
    const from = locationOf(sn, so, 'before'), to = locationOf(en, eo, 'after');
    if (!from || !to) return null;
    const [a, b] = M.indexOf(state.doc, from.block) <= M.indexOf(state.doc, to.block) ? [from, to] : [to, from];
    return { from: a, to: b, collapsed: range.collapsed, range };
  }
  const sameParts = sel => sel.from.block === sel.to.block && sel.from.part === sel.to.part;

  function captureSelection() {
    const sel = selection();
    return sel && (document.activeElement === surface || surface.contains(document.activeElement)) ? { from: sel.from, to: sel.to, collapsed: sel.collapsed } : null;
  }
  function restoreSelection(saved) {
    if (!saved) return;
    const current = getSelection();
    if (current.rangeCount && surface.contains(current.anchorNode) && surface.contains(current.focusNode) && current.anchorNode.isConnected) return;
    place(saved.collapsed ? saved.from : saved);
  }
  /** Put the caret (a location) or a selection ({from, to}) back. */
  function place(target) {
    if (!target) return;
    if (target.from) {
      const a = findPart(surface, target.from), b = findPart(surface, target.to);
      if (!a || !b) return;
      const p = pointAt(a, target.from.offset), q = pointAt(b, target.to.offset), range = document.createRange();
      range.setStart(p.node, p.offset); range.setEnd(q.node, q.offset);
      const s = getSelection(); s.removeAllRanges(); s.addRange(range);
      return;
    }
    if (target.part == null) {
      const node = surface.querySelector(`[data-block="${CSS.escape(target.block)}"]`);
      if (node) node.scrollIntoView({ block: 'nearest' });
      return;
    }
    if (placeCaret(surface, target)) keepCaretVisible();
  }
  /** A caret placed from code does not scroll itself into view the way typing does. */
  function keepCaretVisible() {
    const s = getSelection();
    if (!s.rangeCount) return;
    let rect = s.getRangeAt(0).getBoundingClientRect();
    if (!rect.height) {
      const part = partElement(s.anchorNode);
      if (!part) return;
      rect = part.getBoundingClientRect();
    }
    const view = api.page.getBoundingClientRect();
    if (rect.bottom > view.bottom - 90) api.page.scrollBy(0, rect.bottom - view.bottom + 140);
    else if (rect.top < view.top + 70) api.page.scrollBy(0, rect.top - view.top - 120);
  }

  // -- typing ----------------------------------------------------------------------

  /** Read a part's DOM back into the doc. Returns whether the stored text changed. */
  function syncPart(partEl) {
    const blk = blockElement(partEl)?.dataset.block, path = partEl.dataset.part;
    const b = blk && M.block(state.doc, blk);
    if (!b) return false;
    if (partEl.dataset.kind !== 'plain') {
      // The browser may leave a lone <br> in an emptied part; nothing else.
      if (!partEl.textContent && partEl.firstChild) partEl.replaceChildren();
    }
    const md = partEl.dataset.kind === 'plain' ? readPlain(partEl) : serializeRuns(readRuns(partEl), { cell: path.startsWith('cell:') });
    endBreak(partEl, md);
    if (md === M.getPart(b, path)) return false;
    const next = M.setPart(b, path, md);
    state.doc.blocks[M.indexOf(state.doc, blk)] = next;
    accept(blk);
    dirty.set(keyOf(blk, path), { block: blk, part: path });
    schedule();
    return true;
  }
  /** A trailing newline shows only if something follows it. */
  function endBreak(partEl, text) {
    const last = partEl.lastChild;
    const has = last?.nodeType === Node.ELEMENT_NODE && last.tagName === 'BR' && last.dataset.end;
    if (text.endsWith('\n') && !has) partEl.append(endBreakNode());
    else if (!text.endsWith('\n') && has) last.remove();
  }
  function schedule() {
    api.renderSave();
    clearTimeout(timer);
    timer = setTimeout(() => api.apply([], { typing: true }), 700);
  }
  /** The operations for text typed since the last batch; clears the pending state. */
  function collectDirty() {
    clearTimeout(timer);
    const byBlock = new Map();
    for (const { block: id, part } of dirty.values()) {
      const b = M.block(state.doc, id);
      if (!b) continue;
      byBlock.set(id, { ...byBlock.get(id), ...M.partPatch(b, part) });
    }
    dirty.clear();
    const ops = [...byBlock].map(([id, patch]) => ({ op: 'update_block', id, patch }));
    const title = state.doc.blocks.find(b => b.kind === 'title');
    if (title && byBlock.has(title.id)) {
      const text = plainText(parseInline(title.text));
      if (text !== state.doc.title) ops.push({ op: 'set_title', title: text });
    }
    return ops;
  }
  const hasDirty = () => dirty.size > 0;
  function discardDirty(blockId) {
    for (const [key, entry] of dirty) if (entry.block === blockId) dirty.delete(key);
  }
  function discardAll() { clearTimeout(timer); dirty.clear(); }
  /** Re-read dirty parts from the page after the backend's copy replaced the doc. */
  function overlayDirty() {
    for (const { block: id, part } of dirty.values()) {
      const el = findPart(surface, { block: id, part });
      const b = M.block(state.doc, id);
      if (!el || !b) continue;
      const md = el.dataset.kind === 'plain' ? readPlain(el) : serializeRuns(readRuns(el), { cell: part.startsWith('cell:') });
      state.doc.blocks[M.indexOf(state.doc, id)] = M.setPart(b, part, md);
      accept(id);
    }
  }
  const forget = id => records.delete(id);

  // -- running an edit ---------------------------------------------------------------

  /** Apply the result of an edits.js function; `select` is where the caret goes after. */
  function perform(result, extra = {}) {
    if (!result) return false;
    api.apply(result.ops, { select: result.select, ...extra });
    return true;
  }
  /** Run steps in order, each on the doc the previous left, as one batch. */
  function chain(...steps) {
    let doc = state.doc, ops = [], select = null;
    for (const step of steps) {
      const result = step(doc, select);
      if (!result) return null;
      const next = result.ops.length ? M.applyOps(doc, result.ops) : doc;
      if (!next) return null;
      doc = next; ops = ops.concat(result.ops); select = result.select ?? select;
    }
    return { ops, select };
  }
  /** The selection's contents removed, as the first step of a chain. */
  const removeSelection = sel => (doc) => sel.collapsed ? { ops: [], select: sel.from } : E.deleteSelection(doc, sel.from, sel.to);

  // -- marks -------------------------------------------------------------------------

  /** The parts a selection covers, with the covered range in each. */
  function partsBetween(sel = selection()) {
    if (!sel) return [];
    const first = M.indexOf(state.doc, sel.from.block), last = M.indexOf(state.doc, sel.to.block), out = [];
    for (let i = first; i <= last; i++) {
      const b = state.doc.blocks[i], paths = M.partsOf(b).map(p => p.path);
      const lo = i === first && sel.from.part ? paths.indexOf(sel.from.part) : 0;
      const hi = i === last && sel.to.part ? paths.indexOf(sel.to.part) : paths.length - 1;
      for (let k = Math.max(0, lo); k <= hi; k++) {
        const path = paths[k], length = M.partPlain(b, path).length;
        const start = i === first && path === sel.from.part ? sel.from.offset : 0;
        const end = i === last && path === sel.to.part ? sel.to.offset : length;
        out.push({ block: b.id, part: path, start, end, rich: M.isRich(b, path) });
      }
    }
    return out.filter(p => p.end > p.start || (sel.collapsed && p.block === sel.from.block));
  }

  /** What the selection's text carries: for the toolbar. */
  function marksOfSelection() {
    const sel = selection();
    if (!sel || sel.collapsed) return null;
    const targets = partsBetween(sel).filter(t => t.rich && t.end > t.start);
    if (!targets.length) return null;
    const result = {};
    for (const mark of ['b', 'i', 's', 'c', 'a']) {
      const states = targets.map(t => markState(parseInline(M.getPart(M.block(state.doc, t.block), t.part)), t.start, t.end, mark));
      result[mark] = states.every(s => s === 'all') ? 'all' : states.some(s => s !== 'none') ? 'some' : 'none';
    }
    const hrefs = new Set(targets.flatMap(t => parseInline(M.getPart(M.block(state.doc, t.block), t.part)).filter(r => r.a != null).map(r => r.a)));
    result.href = hrefs.size === 1 ? [...hrefs][0] : '';
    return result;
  }

  function toggleMark(mark, href) {
    const sel = selection();
    if (!sel || sel.collapsed) return false;
    const targets = partsBetween(sel).filter(t => t.rich && t.end > t.start);
    if (!targets.length) return false;
    const on = mark === 'a' ? href != null : !targets.every(t => markState(parseInline(M.getPart(M.block(state.doc, t.block), t.part)), t.start, t.end, mark) === 'all');
    let doc = state.doc;
    const ops = [];
    for (const t of targets) {
      const b = M.block(doc, t.block), runs = parseInline(M.getPart(b, t.part));
      const md = serializeRuns(setMark(runs, t.start, t.end, mark, mark === 'a' ? href : on), { cell: t.part.startsWith('cell:') });
      const next = M.setPart(b, t.part, md);
      doc = { ...doc, blocks: doc.blocks.map(x => x.id === next.id ? next : x) };
      ops.push({ op: 'update_block', id: b.id, patch: M.partPatch(next, t.part) });
    }
    api.apply(ops, { select: { from: sel.from, to: sel.to } });
    return true;
  }

  // -- keyboard ----------------------------------------------------------------------

  function onKeydown(e) {
    if (e.isComposing || composing || e.defaultPrevented) return;
    if (selectedBlock) {
      const id = selectedBlock;
      if ((e.key === 'Backspace' || e.key === 'Delete') && !e.metaKey && !e.ctrlKey) { e.preventDefault(); clearBlockSelection(); perform(E.deleteBlock(state.doc, id)); return; }
      if (!e.metaKey && !e.ctrlKey && !e.shiftKey) clearBlockSelection();
    }
    if (api.menus.handleKey(e)) return;
    const mod = e.metaKey || e.ctrlKey, key = e.key.toLowerCase();
    // Select all inside the language label or a caption selects that text, not the whole page.
    const nested = getSelection().anchorNode && partElement(getSelection().anchorNode)?.closest('[contenteditable="plaintext-only"]');
    if (mod && !e.altKey && !e.shiftKey && key === 'a' && nested) {
      e.preventDefault();
      const range = document.createRange();
      range.selectNodeContents(nested);
      const s = getSelection();
      s.removeAllRanges(); s.addRange(range);
      return;
    }
    if (mod && !e.altKey && !e.shiftKey && key === 'z') { e.preventDefault(); api.undo(); return; }
    if (mod && e.shiftKey && key === 'z') { e.preventDefault(); api.redo(); return; }
    if (mod && !e.altKey && !e.shiftKey && key === 'b') { e.preventDefault(); toggleMark('b'); return; }
    if (mod && !e.altKey && !e.shiftKey && key === 'i') { e.preventDefault(); toggleMark('i'); return; }
    if (mod && !e.altKey && !e.shiftKey && key === 'e') { e.preventDefault(); toggleMark('c'); return; }
    if (mod && e.shiftKey && key === 'x') { e.preventDefault(); toggleMark('s'); return; }
    if (mod && !e.altKey && !e.shiftKey && key === 'k') { e.preventDefault(); api.toolbar.openLink(); return; }
    if (mod && e.altKey && e.code === 'KeyM') { e.preventDefault(); api.comments.begin(); return; }
    if (mod) return;
    if (!surface.contains(document.activeElement) && document.activeElement !== surface) return;
    if (document.activeElement?.closest('[data-part][contenteditable="plaintext-only"]') && e.key !== 'Enter' && e.key !== 'Escape') return;
    const sel = selection();
    if (!sel) return;
    if (e.key === 'Enter' && !e.altKey) { onEnter(e, sel); return; }
    if (e.key === 'Backspace' || e.key === 'Delete') { onDelete(e, sel); return; }
    if (e.key === 'Tab') { onTab(e, sel); return; }
    if (e.key === 'Escape') { plainNext = null; }
  }

  function onEnter(e, sel) {
    e.preventDefault();
    const b = M.block(state.doc, sel.from.block);
    if (!b || !sel.from.part) return;
    const plain = sel.from.part === 'lang' ? null : M.partPlain(b, sel.from.part);
    if (sel.collapsed && b.kind === 'code' && sel.from.part === 'text') { newline(); return; }
    if (sel.collapsed && b.kind === 'code' && sel.from.part === 'lang') { place({ block: b.id, part: 'text', offset: 0 }); return; }
    if (sel.collapsed && sel.from.part === 'caption') {
      perform(E.insertBlock(state.doc, b.id, { kind: 'paragraph', text: '' }, { replaceEmpty: false }));
      return;
    }
    if (e.shiftKey && M.isRich(b, sel.from.part)) { newline(); return; }
    if (sel.collapsed && b.kind === 'table') { perform(E.tableDown(state.doc, sel.from)); return; }
    if (sel.collapsed && b.kind === 'paragraph' && sel.from.offset === plain.length) {
      const fence = M.fenceOnEnter(plain);
      if (fence) {
        perform({ ops: M.replaceOps(state.doc, b.id, [{ id: b.id, ...fence.make }]), select: { block: b.id, part: 'text', offset: 0 } });
        return;
      }
    }
    const result = chain(removeSelection(sel), (doc, at) => E.enter(doc, at ?? sel.from));
    if (!sel.collapsed || !result) { if (!sel.collapsed) perform(chain(removeSelection(sel))); return; }
    perform(result);
  }

  /** A line break inside the part: written as text so the browser does not decide. */
  function newline() {
    insertAtCaret('\n');
    const part = partElement(getSelection().anchorNode);
    if (part) { syncPart(part); }
  }

  function onDelete(e, sel) {
    const backward = e.key === 'Backspace';
    if (!sel.collapsed) {
      if (sameParts(sel)) return;
      e.preventDefault();
      perform(chain(removeSelection(sel)));
      return;
    }
    const at = sel.from;
    if (!at.part) { e.preventDefault(); return; }
    const b = M.block(state.doc, at.block), length = M.partPlain(b, at.part).length;
    if (backward && at.offset === 0 && !e.altKey) {
      e.preventDefault();
      const result = at.part === 'lang' || at.part === 'caption' ? null : E.backspaceAtStart(state.doc, at);
      if (result) perform(result);
      else if (b.kind === 'table') { /* a cell's start is the table's own edge */ }
      return;
    }
    if (!backward && at.offset === length) {
      e.preventDefault();
      perform(E.deleteAtEnd(state.doc, at));
    }
  }

  function onTab(e, sel) {
    e.preventDefault();
    const b = M.block(state.doc, sel.from.block);
    if (!b || !sel.from.part) return;
    if (b.kind === 'list') perform(e.shiftKey ? E.outdent(state.doc, sel.from) : E.indent(state.doc, sel.from));
    else if (b.kind === 'table') perform(E.tableStep(state.doc, sel.from, e.shiftKey));
  }

  // -- input -------------------------------------------------------------------------

  function onBeforeInput(e) {
    if (composing) return;
    const type = e.inputType;
    if (type === 'historyUndo') { e.preventDefault(); api.undo(); return; }
    if (type === 'historyRedo') { e.preventDefault(); api.redo(); return; }
    const sel = selection();
    if (!sel) {
      if (type === 'insertText' || type === 'insertFromPaste') { const ghost = ghostOf(); if (ghost) { e.preventDefault(); materialize(ghost, e.data ?? ''); } }
      return;
    }
    if (/^format(Bold|Italic|StrikeThrough|Underline)/.test(type)) {
      e.preventDefault();
      if (type === 'formatBold') toggleMark('b'); else if (type === 'formatItalic') toggleMark('i'); else if (type === 'formatStrikeThrough') toggleMark('s');
      return;
    }
    if (type === 'insertParagraph' || type === 'insertLineBreak') { e.preventDefault(); return; }
    if (!sel.collapsed && !sameParts(sel) && (type === 'insertText' || type === 'insertReplacementText' || type === 'insertCompositionText')) {
      e.preventDefault();
      const result = chain(removeSelection(sel));
      if (result?.select) {
        api.apply(result.ops, { select: result.select });
        if (e.data) insertAtCaret(e.data);
        const part = partElement(getSelection().anchorNode);
        if (part) syncPart(part);
      }
      return;
    }
    if (type.startsWith('delete') && !sel.collapsed && !sameParts(sel)) {
      e.preventDefault();
      perform(chain(removeSelection(sel)));
      return;
    }
    if (type === 'insertText' && plainNext && sel.collapsed && e.data) {
      e.preventDefault();
      typePlain(sel, e.data);
      return;
    }
    plainNext = null;
  }

  /** Typing straight after a Markdown shortcut continues in plain style. */
  function typePlain(sel, text) {
    plainNext = null;
    const part = findPart(surface, sel.from);
    if (!part) return;
    const runs = insertText(readRuns(part), sel.from.offset, text, {});
    renderRuns(part, runs);
    setCaret(part, sel.from.offset + text.length);
    syncPart(part);
  }

  function onInput(e) {
    if (composing && e.inputType !== 'insertCompositionText') return;
    const sel = selection();
    if (!sel) return;
    const part = partElement(sel.range.startContainer);
    if (!part) return;
    syncPart(part);
    if (!sel.collapsed || part.dataset.kind === 'plain') { api.menus.onInput(sel, part, e); return; }
    const b = M.block(state.doc, sel.from.block);
    if (!b) return;
    if (e.inputType === 'insertText' && e.data) {
      if (b.kind === 'paragraph' && sel.from.part === 'text' && applyBlockShortcut(b, sel, plainText(readRuns(part)))) return;
      if (b.kind !== 'code' && DELIMITERS.has(e.data)) {
        const runs = readRuns(part), done = inlineShortcut(runs, sel.from.offset);
        if (done) {
          renderRuns(part, done.runs);
          setCaret(part, done.caret);
          plainNext = { block: b.id, offset: done.caret };
          syncPart(part);
        }
      }
    }
    api.menus.onInput(sel, part, e);
  }

  function applyBlockShortcut(b, sel, plain) {
    if (sel.from.offset !== plain.length) return false;
    const hit = M.blockShortcut(plain);
    if (!hit) return false;
    discardDirty(b.id);
    const text = serializeRuns([{ t: hit.rest }]);
    let blocks;
    if (hit.whole) {
      const taken = M.idsOf(state.doc);
      const next = M.newId(taken);
      blocks = [{ id: b.id, kind: 'divider' }, { id: next, kind: 'paragraph', text: '' }];
      api.apply(M.replaceOps(state.doc, b.id, blocks), { select: { block: next, part: 'text', offset: 0 } });
      return true;
    }
    const made = { id: b.id, ...structuredClone(hit.make) };
    if (made.kind === 'list') made.items[0].text = text;
    else if (made.kind !== 'code') made.text = text;
    const target = made.kind === 'list' ? `item:1` : 'text';
    api.apply(M.replaceOps(state.doc, b.id, [made]), { select: { block: b.id, part: target, offset: hit.rest.length } });
    return true;
  }

  // -- the empty doc --------------------------------------------------------------------

  const ghostOf = () => {
    const node = getSelection().anchorNode;
    return (node?.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest?.('[data-ghost]') ?? null;
  };
  /** Typing into the placeholder makes the first real block. */
  function materialize(ghost, text) {
    const taken = new Set(), id = M.newId(taken);
    taken.add(id);
    const next = M.newId(taken);
    const md = serializeRuns([{ t: text }]);
    const blocks = ghost.dataset.ghost === 'title'
      ? [{ id, kind: 'title', text: md }, { id: next, kind: 'paragraph', text: '' }]
      : [{ id, kind: 'paragraph', text: md }];
    api.apply([{ op: 'insert_blocks', blocks }], { select: { block: id, part: 'text', offset: text.length } });
  }

  // -- clipboard and files ------------------------------------------------------------------

  async function onPaste(e) {
    e.preventDefault();
    const data = e.clipboardData;
    const images = [...(data.files || [])].filter(f => f.type.startsWith('image/'));
    const sel = selection();
    const ghost = !sel && ghostOf();
    if (images.length) { await api.insertImages(images, sel?.to.block ?? null); return; }
    const html = data.getData('text/html'), text = data.getData('text/plain');
    if (!sel) { if (ghost && text) materialize(ghost, text.split('\n')[0]); return; }
    const inPlain = sel.from.part && !M.isRich(M.block(state.doc, sel.from.block), sel.from.part);
    if (inPlain) { insertAtCaret(text.replace(/\r\n?/g, '\n')); const part = partElement(getSelection().anchorNode); if (part) syncPart(part); return; }
    const markdown = html ? htmlToMarkdown(html) : null;
    pasteContent(sel, markdown ?? text, markdown == null);
  }

  /** Paste: one line goes into the text; several blocks become blocks. */
  function pasteContent(sel, source, literal) {
    const text = source.replace(/\r\n?/g, '\n').replace(/^\n+|\n+$/g, '');
    if (!text) return;
    const blocky = looksLikeMarkdown(text);
    const multi = literal ? text.includes('\n') || blocky : text.includes('\n\n') || blocky;
    const removal = chain(removeSelection(sel));
    if (!removal) return;
    const at = removal.select ?? sel.from;
    if (!at.part) return;
    if (!multi) {
      const runs = literal ? [{ t: text.replace(/\n/g, ' ') }] : parseInline(text.replace(/\n/g, ' '));
      const current = M.applyOps(state.doc, removal.ops) ?? state.doc, owner = M.block(current, at.block);
      const merged = serializeRuns(insertRunsAt(parseInline(M.getPart(owner, at.part)), at.offset, runs), { cell: at.part.startsWith('cell:') });
      const length = plainText(runs).length;
      api.apply([...removal.ops, ...E.setPartOps(current, at.block, at.part, merged)], { select: { block: at.block, part: at.part, offset: at.offset + length } });
      return;
    }
    const doc = M.applyOps(state.doc, removal.ops) ?? state.doc;
    const owner = M.block(doc, at.block);
    const ops = [...removal.ops];
    const empty = M.TEXT_KINDS.has(owner.kind) && M.partPlain(owner, 'text') === '';
    if (owner.kind === 'list' || !M.TEXT_KINDS.has(owner.kind)) {
      ops.push(...pasteBlocksOps(literal && !blocky ? paragraphs(text) : null, text, owner.id));
    } else if (empty) {
      ops.push(...pasteBlocksOps(literal && !blocky ? paragraphs(text) : null, text, owner.id), { op: 'delete_blocks', ids: [owner.id] });
    } else {
      const [left, right] = E.splitPart(owner, 'text', at.offset);
      ops.push({ op: 'update_block', id: owner.id, patch: { text: left } });
      if (right) ops.push({ op: 'insert_blocks', after: owner.id, blocks: [{ id: M.newId(M.idsOf(doc)), kind: 'paragraph', text: right }] });
      ops.push(...pasteBlocksOps(literal && !blocky ? paragraphs(text) : null, text, owner.id));
    }
    api.apply(ops, { focusNew: true });
  }
  /** Plain text keeps its paragraphs: blank-line separated, or one per line when there are none. */
  const paragraphs = text => text.split(/\n{2,}/.test(text) ? /\n{2,}/ : /\n/).map(chunk => ({ kind: 'paragraph', text: serializeRuns([{ t: chunk.trim() }]) })).filter(p => p.text);
  function pasteBlocksOps(blocks, markdown, after) {
    if (blocks) {
      const taken = M.idsOf(state.doc);
      return [{ op: 'insert_blocks', after, blocks: blocks.map(b => { const id = M.newId(taken); taken.add(id); return { id, ...b }; }) }];
    }
    return [{ op: 'insert_markdown', after, markdown }];
  }
  function insertRunsAt(runs, at, extra) {
    return [...parseSlice(runs, 0, at), ...extra, ...parseSlice(runs, at, plainText(runs).length)];
  }
  function parseSlice(runs, start, end) {
    const out = [];
    let pos = 0;
    for (const run of runs) {
      const from = Math.max(start, pos), to = Math.min(end, pos + run.t.length);
      if (to > from) out.push({ ...run, t: run.t.slice(from - pos, to - pos) });
      pos += run.t.length;
    }
    return out;
  }

  function onCut(e) {
    const sel = selection();
    if (!sel || sel.collapsed || sameParts(sel)) return;
    e.preventDefault();
    e.clipboardData.setData('text/plain', getSelection().toString());
    perform(chain(removeSelection(sel)));
  }

  async function onDrop(e) {
    e.preventDefault();
    const images = [...(e.dataTransfer?.files || [])].filter(f => f.type.startsWith('image/'));
    if (!images.length) return;
    const target = document.elementFromPoint(e.clientX, e.clientY)?.closest?.('[data-block]');
    await api.insertImages(images, target?.dataset.block ?? null);
  }

  // -- pointer -------------------------------------------------------------------------

  /** Blocks with no text (images, charts, rules) are selected whole, then Delete removes them. */
  function selectBlock(id) {
    clearBlockSelection();
    selectedBlock = id;
    surface.querySelector(`[data-block="${CSS.escape(id)}"]`)?.classList.add('selected');
    getSelection().removeAllRanges();
    surface.focus({ preventScroll: true });
  }
  function clearBlockSelection() {
    selectedBlock = null;
    surface.querySelectorAll('.blk.selected').forEach(n => n.classList.remove('selected'));
  }
  /** A click below the last block puts the caret at the end of the document. */
  function focusEnd() {
    const last = state.doc.blocks.at(-1);
    if (!last) { surface.querySelector('.ghost.blk-paragraph .part')?.focus(); return; }
    const end = E.endOf(last);
    if (end && !M.ISLAND_KINDS.has(last.kind)) { surface.focus({ preventScroll: true }); place(end); return; }
    perform(E.insertBlock(state.doc, last.id, { kind: 'paragraph', text: '' }, { replaceEmpty: false }));
  }

  function onClick(e) {
    const island = e.target.closest?.('.blk-image figure, .blk-chart figure, .blk-divider, .blk-page-break, .blk-toc');
    if (island && !e.target.closest('figcaption, a[data-goto]')) { selectBlock(island.closest('[data-block]').dataset.block); return; }
    clearBlockSelection();
    const check = e.target.closest?.('[data-check]');
    if (check) {
      const blk = check.closest('[data-block]').dataset.block;
      perform(E.toggleCheck(state.doc, blk, check.dataset.check));
      return;
    }
    const link = e.target.closest?.('a[data-goto]');
    if (link) { e.preventDefault(); api.scrollToBlock(link.dataset.goto); return; }
    if (e.target.closest?.('a.link')) e.preventDefault();
    const icon = e.target.closest?.('.callout-icon');
    if (icon) api.menus.openBlockMenu(icon.closest('[data-block]').dataset.block, icon.getBoundingClientRect());
  }

  // -- wiring ----------------------------------------------------------------------------

  surface.addEventListener('keydown', onKeydown);
  surface.addEventListener('beforeinput', onBeforeInput);
  surface.addEventListener('input', onInput);
  surface.addEventListener('paste', onPaste);
  surface.addEventListener('cut', onCut);
  surface.addEventListener('drop', onDrop);
  surface.addEventListener('dragover', e => e.preventDefault());
  surface.addEventListener('click', onClick);
  surface.addEventListener('compositionstart', () => { composing = true; });
  surface.addEventListener('compositionend', () => {
    composing = false;
    const sel = selection();
    const part = sel && partElement(sel.range.startContainer);
    if (part) syncPart(part);
  });
  surface.addEventListener('blur', () => { if (dirty.size) api.apply([], { typing: true }); }, true);

  return {
    reconcile, selection, captureSelection, restoreSelection, place, perform, chain, collectDirty, hasDirty, overlayDirty, discardDirty, discardAll,
    focusEnd, selectBlock, toggleMark, setLink: href => toggleMark('a', href), marksOfSelection, partsBetween, syncPart, forget, accept, removeSelection, syncPartAtCaret() {
      const sel = selection();
      const part = sel && partElement(sel.range.startContainer);
      if (part) syncPart(part);
    },
    clearPlainNext() { plainNext = null; },
    get plainNext() { return plainNext; },
    records,
  };
}

