// The floating toolbar over selected text, and the link popover. Formatting
// lives here, not in a permanent row: the page stays quiet until you select.
import * as M from './model.js';
import * as E from './edits.js';
import { glyph } from './glyphs.js';
import { placePopup } from './menus.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const STYLE_TARGET = { title: 'title', h1: 'h1', h2: 'h2', h3: 'h3', text: 'text', quote: 'quote', code: 'code' };

/** `example.com` becomes https; anything that is not http(s) or mailto is refused. */
export function normalizeLink(value) {
  const text = value.trim();
  if (!text) return null;
  if (/^(https?:\/\/|mailto:)\S+$/i.test(text)) return text;
  if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(text)) return `mailto:${text}`;
  if (/^[^\s:/]+\.[^\s:]{2,}(\/\S*)?$/.test(text)) return `https://${text}`;
  return null;
}

export function toolbarController(api) {
  const { state, surface, overlay } = api;
  const bar = el('div', 'popup sel-toolbar');
  bar.setAttribute('role', 'toolbar');
  bar.setAttribute('aria-label', 'Text formatting');
  bar.hidden = true;
  const buttons = {};
  let styleMenu = null, linkPop = null, frame = 0;

  const style = el('button', 'tb-style');
  style.type = 'button';
  style.setAttribute('aria-haspopup', 'menu');
  style.setAttribute('aria-label', 'Text style');
  style.append(el('span', 'tb-style-label', 'Text'), glyph('down'));
  bar.append(style, el('span', 'tb-sep'));

  const add = (key, icon, label, run) => {
    const b = el('button', 'tb-button');
    b.type = 'button';
    b.setAttribute('aria-label', label);
    b.title = label;
    b.append(glyph(icon));
    b.addEventListener('click', run);
    bar.append(b);
    buttons[key] = b;
    return b;
  };
  add('b', 'bold', 'Bold (⌘B)', () => api.editor.toggleMark('b'));
  add('i', 'italic', 'Italic (⌘I)', () => api.editor.toggleMark('i'));
  add('s', 'strike', 'Strikethrough (⇧⌘X)', () => api.editor.toggleMark('s'));
  add('c', 'code', 'Code (⌘E)', () => api.editor.toggleMark('c'));
  add('a', 'link', 'Link (⌘K)', () => openLink());
  bar.append(el('span', 'tb-sep'));
  add('comment', 'comment', 'Comment (⌥⌘M)', () => api.comments.begin());
  bar.addEventListener('mousedown', e => { if (!e.target.closest('input')) e.preventDefault(); });
  overlay.append(bar);

  // -- the style menu --------------------------------------------------------------------

  function closeStyle() { styleMenu?.remove(); styleMenu = null; style.setAttribute('aria-expanded', 'false'); }
  style.addEventListener('click', () => {
    if (styleMenu) { closeStyle(); return; }
    styleMenu = el('div', 'popup menu style-menu');
    styleMenu.setAttribute('role', 'menu');
    const current = currentStyle();
    for (const id of M.STYLE_ITEMS) {
      const row = el('button', `menu-row compact${id === current ? ' on' : ''}`);
      row.type = 'button';
      row.setAttribute('role', 'menuitem');
      row.append(el('span', `menu-label style-${id}`, M.styleLabel(id)));
      if (id === current) row.append(glyph('check'));
      row.addEventListener('mousedown', e => e.preventDefault());
      row.addEventListener('click', () => { closeStyle(); applyStyle(id); });
      styleMenu.append(row);
    }
    overlay.append(styleMenu);
    style.setAttribute('aria-expanded', 'true');
    placePopup(styleMenu, style.getBoundingClientRect());
  });
  function selectedBlocks() {
    const sel = api.editor.selection();
    if (!sel) return [];
    const a = M.indexOf(state.doc, sel.from.block), z = M.indexOf(state.doc, sel.to.block);
    return state.doc.blocks.slice(a, z + 1);
  }
  function currentStyle() {
    const kinds = new Set(selectedBlocks().map(b => M.styleItemOf(b)));
    return kinds.size === 1 ? [...kinds][0] : null;
  }
  function applyStyle(id) {
    const target = STYLE_TARGET[id];
    const blocks = selectedBlocks().filter(b => M.TEXT_KINDS.has(b.kind) || b.kind === 'code');
    if (!blocks.length) return;
    const result = api.editor.chain(...blocks.map(b => doc => E.turnInto(doc, b.id, target)));
    if (result) api.editor.perform(result);
  }

  // -- showing it --------------------------------------------------------------------------

  function update() {
    frame = 0;
    const sel = api.editor.selection();
    const plain = sel && document.activeElement && document.activeElement.closest?.('[contenteditable="plaintext-only"]');
    const text = sel && !sel.collapsed ? getSelection().toString() : '';
    const marks = sel && !sel.collapsed ? api.editor.marksOfSelection() : null;
    if (!sel || sel.collapsed || !text.trim() || plain || !marks || linkPop || api.menus.open) {
      if (!linkPop) { bar.hidden = true; closeStyle(); }
      return;
    }
    bar.hidden = false;
    for (const key of ['b', 'i', 's', 'c', 'a']) {
      buttons[key].classList.toggle('on', marks[key] === 'all');
      buttons[key].setAttribute('aria-pressed', String(marks[key] === 'all'));
    }
    const current = currentStyle();
    style.querySelector('.tb-style-label').textContent = current ? M.styleLabel(current) : selectedBlocks().length > 1 ? 'Mixed' : M.blockLabel(selectedBlocks()[0] || { kind: 'paragraph' });
    style.disabled = !selectedBlocks().some(b => M.TEXT_KINDS.has(b.kind) || b.kind === 'code');
    const rect = getSelection().getRangeAt(0).getBoundingClientRect();
    placePopup(bar, rect, { prefer: 'above', align: 'center' });
  }
  const schedule = () => { if (!frame) frame = requestAnimationFrame(update); };
  document.addEventListener('selectionchange', schedule, { signal: api.signal });
  api.page.addEventListener('scroll', schedule, true);
  addEventListener('resize', schedule, { signal: api.signal });
  document.addEventListener('pointerdown', e => {
    if (styleMenu && !e.target.closest('.style-menu') && !e.target.closest('.tb-style')) closeStyle();
    if (linkPop && !e.target.closest('.link-pop')) closeLink(false);
  }, { signal: api.signal });

  // -- links -----------------------------------------------------------------------------------

  function closeLink(restore = true) {
    if (!linkPop) return;
    const { saved } = linkPop;
    linkPop.node.remove(); linkPop = null;
    if (restore) { surface.focus({ preventScroll: true }); api.editor.place(saved); }
    schedule();
  }
  function openLink() {
    const sel = api.editor.selection();
    if (!sel || sel.collapsed) { api.toast('Select the text to link first.'); return; }
    const marks = api.editor.marksOfSelection();
    if (!marks) return;
    const saved = { from: sel.from, to: sel.to };
    const rect = getSelection().getRangeAt(0).getBoundingClientRect();
    closeStyle();
    const node = el('form', 'popup link-pop');
    node.setAttribute('aria-label', 'Link');
    const input = el('input');
    input.type = 'text'; input.placeholder = 'Paste or type a link'; input.value = marks.href || '';
    input.setAttribute('aria-label', 'Link address');
    input.autocomplete = 'off'; input.spellcheck = false;
    const hint = el('div', 'link-hint');
    const apply = el('button', 'primary', marks.href ? 'Update' : 'Apply');
    apply.type = 'submit';
    const row = el('div', 'link-actions');
    row.append(apply);
    if (marks.a !== 'none') {
      const remove = el('button', 'link-remove', 'Remove');
      remove.type = 'button';
      remove.addEventListener('click', () => finishLink(null));
      row.append(remove);
    }
    node.append(input, hint, row);
    node.addEventListener('submit', e => {
      e.preventDefault();
      const href = normalizeLink(input.value);
      if (!href) { hint.textContent = 'Use an http(s) or mailto address.'; input.focus(); return; }
      finishLink(href);
    });
    node.addEventListener('keydown', e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeLink(); } });
    overlay.append(node);
    linkPop = { node, saved };
    bar.hidden = true;
    placePopup(node, rect, { prefer: 'above', align: 'center' });
    input.focus();
    input.select();
  }
  /** Close the popover and set (or, with null, remove) the link on the saved selection. */
  function finishLink(href) {
    const { saved } = linkPop;
    closeLink(false);
    surface.focus({ preventScroll: true });
    api.editor.place(saved);
    api.editor.setLink(href);
  }

  return { openLink, hide() { bar.hidden = true; closeStyle(); }, update: schedule, closeLink };
}
