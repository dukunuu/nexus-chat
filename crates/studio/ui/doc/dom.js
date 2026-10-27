// Runs <-> DOM, and the mapping between the browser's selection and plain-text
// offsets inside an editable part. Text always goes in through text nodes, never
// as HTML.
import { normalize, plainText } from './inline.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

/** Fill a part with rich runs. `deco` carries comment highlights and tracked changes. */
export function renderRuns(part, runs, deco = {}) {
  part.replaceChildren();
  const plain = plainText(runs), marks = deco.marks || [], dels = deco.del || [], inserts = [...(deco.ins || [])];
  const cuts = new Set([0, plain.length]);
  let at = 0;
  for (const run of runs) { cuts.add(at); at += run.t.length; cuts.add(at); }
  for (const range of [...marks, ...dels]) { cuts.add(Math.max(0, range.start)); cuts.add(Math.min(plain.length, range.end)); }
  const points = [...cuts].sort((a, b) => a - b);
  const runAt = position => {
    let pos = 0;
    for (const run of runs) { if (position < pos + run.t.length) return run; pos += run.t.length; }
    return null;
  };
  const insertAt = position => {
    for (const ins of inserts.filter(i => i.at === position)) {
      const node = el('ins', 'sug-ins', ins.text);
      node.contentEditable = 'false'; node.dataset.sug = ins.id;
      part.append(node);
    }
  };
  for (let k = 0; k < points.length; k++) {
    insertAt(points[k]);
    const start = points[k], end = points[k + 1];
    if (end === undefined || end <= start) continue;
    const run = runAt(start);
    if (!run) continue;
    if (run.f != null) { part.append(footnote(run)); continue; }
    let node = document.createTextNode(plain.slice(start, end));
    for (const range of dels) if (range.start <= start && range.end >= end) { const d = el('del', 'sug-del'); d.dataset.sug = range.id; d.append(node); node = d; }
    for (const range of marks) if (range.start <= start && range.end >= end) {
      const m = el('mark', `cmt${range.hot ? ' hot' : ''}${range.resolved ? ' done' : ''}`); m.dataset.thread = range.id; m.append(node); node = m;
    }
    for (const [key, tag] of [['c', 'code'], ['s', 's'], ['i', 'em'], ['b', 'strong']]) if (run[key]) { const w = el(tag); w.append(node); node = w; }
    if (run.a != null) { const a = el('a', 'link'); a.setAttribute('href', run.a); a.title = run.a; a.append(node); node = a; }
    part.append(node);
  }
  insertAt(plain.length);
  if (plain.endsWith('\n')) part.append(endBreak());
}
/** A line break that only gives a trailing newline its height. */
export function endBreak() {
  const br = el('br');
  br.dataset.end = '1';
  return br;
}
function footnote(run) {
  const sup = el('sup', 'fn', run.t);
  sup.contentEditable = 'false'; sup.dataset.fn = run.f;
  return sup;
}

/** Fill a plain-text part (code, captions, the language label). */
export function renderPlain(part, text, deco = {}) {
  part.replaceChildren();
  const del = deco.del || [], inserts = deco.ins || [];
  if (!del.length && !inserts.length) part.append(document.createTextNode(text));
  else renderRuns(part, text ? [{ t: text }] : [], deco);
  if (text.endsWith('\n')) part.append(endBreak());
}

/** Text nodes of a part that belong to the document (tracked insertions do not). */
function* textNodes(root) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: node => node.parentElement.closest('ins.sug-ins') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
  });
  for (let n = walker.nextNode(); n; n = walker.nextNode()) yield n;
}

/** The runs a part currently shows. Browser leftovers (b, i, spans) are read as their marks. */
export function readRuns(root) {
  const out = [];
  const walk = (node, marks) => {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) { if (child.data) out.push({ ...marks, t: child.data.replace(/ /g, ' ').replace(/​/g, '') }); continue; }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      const tag = child.tagName;
      if (tag === 'INS' && child.classList.contains('sug-ins')) continue;
      if (tag === 'BR') continue;
      if (tag === 'SUP' && child.dataset.fn != null) { out.push({ ...marks, t: child.textContent, f: child.dataset.fn }); continue; }
      const next = { ...marks };
      if (tag === 'STRONG' || tag === 'B') next.b = true;
      else if (tag === 'EM' || tag === 'I') next.i = true;
      else if (tag === 'S' || tag === 'STRIKE') next.s = true;
      else if (tag === 'CODE') next.c = true;
      else if (tag === 'A' && child.getAttribute('href')) next.a = child.getAttribute('href');
      else if (child.style?.fontWeight >= 600) next.b = true;
      walk(child, next);
    }
  };
  walk(root, {});
  return normalize(out);
}
export const readPlain = root => [...textNodes(root)].map(n => n.data.replace(/​/g, '')).join('');

/** Plain-text offset of a DOM point inside `root`. */
export function offsetIn(root, node, offset) {
  const end = document.createRange();
  end.selectNodeContents(root);
  try { end.setEnd(node, offset); } catch { return 0; }
  let total = 0;
  for (const text of textNodes(root)) {
    if (text === node) return total + offset;
    if (end.comparePoint(text, 0) > 0) break;
    if (end.comparePoint(text, text.length) <= 0) total += text.length;
    else break;
  }
  return total;
}

/** The DOM point for a plain-text offset, preferring the end of the earlier text. */
export function pointAt(root, offset) {
  let seen = 0, last = null;
  for (const text of textNodes(root)) {
    if (seen + text.length >= offset) return { node: text, offset: offset - seen };
    seen += text.length; last = text;
  }
  return last ? { node: last, offset: last.length } : { node: root, offset: 0 };
}

export function setCaret(root, offset) {
  const { node, offset: at } = pointAt(root, offset);
  const range = document.createRange();
  range.setStart(node, at); range.collapse(true);
  const selection = getSelection();
  selection.removeAllRanges(); selection.addRange(range);
}
export function setRange(root, start, end) {
  const a = pointAt(root, start), b = pointAt(root, end), range = document.createRange();
  range.setStart(a.node, a.offset); range.setEnd(b.node, b.offset);
  const selection = getSelection();
  selection.removeAllRanges(); selection.addRange(range);
}

export const partElement = node => (node?.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest?.('[data-part]') ?? null;
export const blockElement = node => (node?.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest?.('[data-block]') ?? null;

/** A location ({block, part, offset}) for a DOM point; a block edge for points outside any part. */
export function locationOf(node, offset, side = 'before') {
  const part = partElement(node);
  const blk = blockElement(part || node);
  if (!blk) return null;
  if (part) return { block: blk.dataset.block, part: part.dataset.part, offset: offsetIn(part, node, offset) };
  return { block: blk.dataset.block, part: null, side };
}

/** The part element for a location inside `surface`. */
export function findPart(surface, loc) {
  if (!loc?.part) return null;
  for (const blk of surface.querySelectorAll(`[data-block="${CSS.escape(loc.block)}"]`)) {
    const part = blk.querySelector(`[data-part="${CSS.escape(loc.part)}"]`);
    if (part) return part;
  }
  return null;
}
export function placeCaret(surface, loc) {
  const part = findPart(surface, loc);
  if (!part) return false;
  const text = part.dataset.kind === 'plain' ? readPlain(part) : plainText(readRuns(part));
  setCaret(part, Math.min(loc.offset ?? 0, text.length));
  part.focus?.({ preventScroll: true });
  return true;
}

/** The current selection as two locations plus whether it is collapsed, or null. */
export function selectionLocations(surface) {
  const selection = getSelection();
  if (!selection.rangeCount) return null;
  const range = selection.getRangeAt(0);
  if (!surface.contains(range.startContainer) || !surface.contains(range.endContainer)) return null;
  const from = locationOf(range.startContainer, range.startOffset, 'before');
  const to = locationOf(range.endContainer, range.endOffset, 'after');
  return from && to ? { from, to, collapsed: range.collapsed, range } : null;
}

/** Insert plain text at the caret inside a part (used where the browser must not decide). */
export function insertAtCaret(text) {
  const selection = getSelection();
  if (!selection.rangeCount) return;
  const range = selection.getRangeAt(0);
  range.deleteContents();
  const node = document.createTextNode(text);
  range.insertNode(node);
  range.setStartAfter(node); range.collapse(true);
  selection.removeAllRanges(); selection.addRange(range);
}
