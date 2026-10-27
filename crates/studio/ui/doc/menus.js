// Popup menus on the page: the "/" insert menu, the block menu, the drag handle
// beside a block, and the controls that add table rows and columns. Everything
// is a button in the overlay layer, never inside the editable surface.
import * as M from './model.js';
import * as E from './edits.js';
import { partElement, readRuns, renderRuns, setCaret } from './dom.js';
import { deleteRange, plainText } from './inline.js';
import { glyph } from './glyphs.js';
import { TONE_LABELS } from './blocks.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const TURN_TARGETS = { text: 'text', title: 'title', h1: 'h1', h2: 'h2', h3: 'h3', quote: 'quote', code: 'code', bullets: 'bullets', numbers: 'numbers', checklist: 'checklist', callout: 'callout:info' };

/** Keep a popup inside the window, below or above its anchor. */
export function placePopup(node, rect, { prefer = 'below', align = 'left' } = {}) {
  node.style.left = '0px'; node.style.top = '0px';
  const { width, height } = node.getBoundingClientRect();
  const margin = 8, W = innerWidth, H = innerHeight;
  let top = prefer === 'above' ? rect.top - height - 6 : rect.bottom + 6;
  if (top + height > H - margin) top = Math.max(margin, rect.top - height - 6);
  if (top < margin) top = Math.min(margin, H - height - margin);
  let left = align === 'center' ? rect.left + rect.width / 2 - width / 2 : align === 'right' ? rect.right - width : rect.left;
  left = Math.max(margin, Math.min(left, W - width - margin));
  node.style.left = `${Math.round(left)}px`; node.style.top = `${Math.round(top)}px`;
}

export function menusController(api) {
  const { state, surface, overlay } = api;
  let slash = null, block = null;

  // -- the "/" menu ------------------------------------------------------------------

  function closeSlash() { slash?.node.remove(); slash = null; }
  function openSlash(sel, partEl, start) {
    closeSlash();
    const node = el('div', 'popup menu slash');
    node.setAttribute('role', 'listbox');
    node.setAttribute('aria-label', 'Insert a block');
    overlay.append(node);
    slash = { node, block: sel.from.block, part: sel.from.part, start, items: M.INSERT_ITEMS, index: 0, query: '' };
    renderSlash();
    anchorSlash(partEl);
  }
  function anchorSlash(partEl) {
    const range = getSelection().rangeCount ? getSelection().getRangeAt(0).cloneRange() : null;
    let rect = range?.getBoundingClientRect();
    if (!rect || (!rect.width && !rect.height && !rect.top)) rect = partEl.getBoundingClientRect();
    placePopup(slash.node, rect, { prefer: 'below' });
  }
  function renderSlash() {
    slash.node.replaceChildren();
    if (!slash.items.length) { slash.node.append(el('div', 'menu-empty', 'No matching blocks')); return; }
    slash.items.forEach((item, i) => {
      const row = el('button', `menu-row${i === slash.index ? ' active' : ''}`);
      row.type = 'button';
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', String(i === slash.index));
      row.tabIndex = -1;
      const copy = el('span', 'menu-copy');
      copy.append(el('span', 'menu-label', item.label), el('span', 'menu-hint', item.hint));
      row.append(copy);
      row.addEventListener('mousedown', e => e.preventDefault());
      row.addEventListener('mousemove', () => { if (slash.index !== i) { slash.index = i; renderSlash(); } });
      row.addEventListener('click', () => chooseSlash(item));
      slash.node.append(row);
    });
    slash.node.querySelector('.active')?.scrollIntoView({ block: 'nearest' });
  }
  function chooseSlash(item) {
    const { block: id, part, start } = slash;
    const partEl = surface.querySelector(`[data-block="${CSS.escape(id)}"] [data-part="${CSS.escape(part)}"]`);
    closeSlash();
    if (!partEl) return;
    const caret = M.getPart(M.block(state.doc, id), part) ? caretOf(partEl) : start;
    // The "/query" text goes first, as part of the same undo step.
    const runs = deleteRange(readRuns(partEl), start, Math.max(start, caret));
    renderRuns(partEl, runs);
    setCaret(partEl, start);
    api.editor.syncPart(partEl);
    const doc = state.doc, b = M.block(doc, id);
    const target = TURN_TARGETS[item.id];
    if (item.file) { api.chooseImage(id); return; }
    if (target && M.TEXT_KINDS.has(b.kind)) { api.editor.perform(E.turnInto(doc, id, target, start)); return; }
    api.editor.perform(E.insertBlock(doc, id, item.make()));
  }
  const caretOf = partEl => {
    const s = getSelection();
    return s.rangeCount ? api.offsetIn(partEl, s.anchorNode, s.anchorOffset) : 0;
  };

  /** Called after every input in the surface. */
  function onInput(sel, partEl, event) {
    if (slash) {
      const plain = plainText(readRuns(partEl)), caret = sel.from.offset;
      const query = plain.slice(slash.start + 1, caret);
      if (sel.from.block !== slash.block || sel.from.part !== slash.part || plain[slash.start] !== '/' || caret <= slash.start || /\s\s|\n/.test(query)) { closeSlash(); return; }
      slash.query = query;
      slash.items = M.filterInsert(query);
      slash.index = 0;
      if (!slash.items.length && query.length > 2) { closeSlash(); return; }
      renderSlash(); anchorSlash(partEl);
      return;
    }
    if (event?.inputType !== 'insertText' || event.data !== '/') return;
    const b = M.block(state.doc, sel.from.block);
    if (!b || b.kind === 'code' || !M.isRich(b, sel.from.part) || sel.from.part.startsWith('cell:')) return;
    const plain = plainText(readRuns(partEl)), at = sel.from.offset - 1;
    if (at === 0 || /\s/.test(plain[at - 1])) openSlash(sel, partEl, at);
  }

  // -- the block menu -------------------------------------------------------------------

  function closeBlockMenu() { block?.node.remove(); block = null; handle.classList.remove('open'); }
  function openBlockMenu(id, rect) {
    closeBlockMenu(); closeSlash();
    const b = M.block(state.doc, id);
    if (!b) return;
    const node = el('div', 'popup menu block-menu');
    node.setAttribute('role', 'menu');
    node.setAttribute('aria-label', `${M.blockLabel(b)} options`);
    const group = (title, items, grid = false) => {
      if (title) node.append(el('div', 'menu-group', title));
      const wrap = el('div', grid ? 'menu-items grid' : 'menu-items');
      for (const [label, run, active, icon] of items) {
        const row = el('button', `menu-row compact${active ? ' on' : ''}`);
        row.type = 'button'; row.setAttribute('role', 'menuitem');
        if (icon) row.append(glyph(icon));
        row.append(el('span', 'menu-label', label));
        if (active) row.append(glyph('check'));
        row.addEventListener('click', () => { closeBlockMenu(); run(); });
        wrap.append(row);
      }
      node.append(wrap);
    };
    const current = M.styleItemOf(b);
    if (M.TEXT_KINDS.has(b.kind) || b.kind === 'code' || b.kind === 'list') {
      const kinds = [['Text', 'text', 'paragraph'], ['Heading 1', 'h1'], ['Heading 2', 'h2'], ['Heading 3', 'h3'], ['Quote', 'quote', 'quote'], ['Callout', 'callout:info'], ['Code', 'code', 'code'], ['Bulleted list', 'bullets', 'list'], ['Numbered list', 'numbers'], ['Checklist', 'checklist']];
      const isOn = target => target === current || (b.kind === 'list' && (target === 'numbers' ? b.ordered : target === 'bullets' ? !b.ordered && !b.items.some(i => i.checked != null) : b.items.some(i => i.checked != null) && target === 'checklist')) || (b.kind === 'callout' && target.startsWith('callout'));
      group('Turn into', kinds.map(([label, target]) => [label, () => api.editor.perform(E.turnInto(state.doc, id, target)), isOn(target)]), true);
    }
    if (b.kind === 'callout') group('Tone', Object.entries(TONE_LABELS).map(([tone, label]) => [label, () => api.apply([{ op: 'update_block', id, patch: { tone } }]), b.tone === tone, tone]));
    if (b.kind === 'image') group('Width', ['inline', 'wide', 'full'].map(w => [w[0].toUpperCase() + w.slice(1), () => api.apply([{ op: 'update_block', id, patch: { width: w } }]), (b.width || 'full') === w]));
    const index = M.indexOf(state.doc, id);
    const actions = [
      ['Duplicate', () => api.editor.perform(E.duplicateBlock(state.doc, id)), false, 'duplicate'],
      ['Copy link to block', () => api.copyBlockLink(id), false, 'link'],
    ];
    if (index > 0) actions.push(['Move up', () => api.editor.perform(E.moveBlock(state.doc, id, state.doc.blocks[index - 1].id)), false, 'forward']);
    if (index < state.doc.blocks.length - 1) actions.push(['Move down', () => api.editor.perform(E.moveBlock(state.doc, id, state.doc.blocks[index + 2]?.id ?? null)), false, 'back']);
    actions.push(['Delete', () => api.editor.perform(E.deleteBlock(state.doc, id)), false, 'trash']);
    node.append(el('div', 'menu-rule'));
    group(null, actions);
    overlay.append(node);
    block = { node, id };
    handle.classList.add('open');
    placePopup(node, rect, { prefer: 'below' });
    node.querySelector('button')?.focus({ preventScroll: true });
    node.addEventListener('keydown', e => {
      const items = [...node.querySelectorAll('button')], at = items.indexOf(document.activeElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); items[(at + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus(); }
      else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeBlockMenu(); api.editor.place(api.lastCaret); surface.focus({ preventScroll: true }); }
    });
  }

  // -- the handle and table controls -----------------------------------------------------

  const handle = el('button', 'block-handle', '⋮⋮');
  handle.type = 'button';
  handle.hidden = true;
  handle.setAttribute('aria-label', 'Block options. Drag to move.');
  handle.title = 'Drag to move · click for options';
  const indicator = el('div', 'drop-line');
  indicator.hidden = true;
  // Table controls: a grip on the hovered row's left edge and on the hovered
  // column's top edge (click for that row's or column's menu), and a bar along
  // the bottom and the right edge that adds a row or a column.
  const tableButton = (className, label, content) => {
    const node = el('button', className);
    node.type = 'button'; node.hidden = true;
    node.setAttribute('aria-label', label); node.title = label;
    if (content) node.append(content);
    node.addEventListener('mousedown', e => e.preventDefault());
    return node;
  };
  const rowGrip = tableButton('table-grip row', 'Row options', el('span', 'grip-dots'));
  const columnGrip = tableButton('table-grip column', 'Column options', el('span', 'grip-dots'));
  const addRow = tableButton('table-addbar row', 'Add a row', glyph('plus'));
  const addColumn = tableButton('table-addbar column', 'Add a column', glyph('plus'));
  const tableControls = [rowGrip, columnGrip, addRow, addColumn];
  overlay.append(handle, indicator, ...tableControls);
  let hovered = null, hideTimer = 0, dragging = null, cell = null, tableMenu = null;

  function blockAt(y) {
    for (const child of surface.children) {
      if (child.dataset.ghost) continue;
      const rect = child.getBoundingClientRect();
      if (y >= rect.top - 4 && y <= rect.bottom + 4) return child;
    }
    return null;
  }
  function showHandle(node) {
    if (!node || dragging) return;
    hovered = node;
    const rect = node.getBoundingClientRect();
    const first = node.querySelector('.part, hr, figure, .rule, .toc-title');
    const lineTop = first ? first.getBoundingClientRect().top : rect.top;
    const surfaceRect = surface.getBoundingClientRect();
    const view = api.page.getBoundingClientRect();
    if (rect.bottom < view.top + 8 || rect.top > view.bottom - 8) { handle.hidden = true; hideTableControls(); return; }
    handle.hidden = false;
    handle.style.left = `${Math.round(surfaceRect.left - 30)}px`;
    handle.style.top = `${Math.round(Math.max(rect.top, Math.min(lineTop, rect.bottom - 24)) + 2)}px`;
    placeTableControls(node);
  }
  function hideTableControls() { if (tableMenu) return; for (const node of tableControls) node.hidden = true; markCells(null); }
  function hideHandle() { if (dragging || block || tableMenu) return; handle.hidden = true; hideTableControls(); hovered = null; }

  /** The [row, column] of the table cell under a node, or null. */
  function cellOf(node) {
    const part = node?.closest?.('td, th')?.querySelector('[data-part^="cell:"]');
    return part ? part.dataset.part.slice(5).split(':').map(Number) : null;
  }
  function placeTableControls(node) {
    const table = node?.classList.contains('blk-table') ? node.querySelector('table') : null;
    if (!table) { hideTableControls(); return; }
    const t = table.getBoundingClientRect();
    addRow.hidden = addColumn.hidden = false;
    Object.assign(addRow.style, { left: `${Math.round(t.left)}px`, top: `${Math.round(t.bottom + 4)}px`, width: `${Math.round(t.width)}px` });
    Object.assign(addColumn.style, { left: `${Math.round(t.right + 4)}px`, top: `${Math.round(t.top)}px`, height: `${Math.round(t.height)}px` });
    const rows = table.rows, at = cell && cell[0] < rows.length && cell[1] < rows[cell[0]].cells.length ? cell : null;
    rowGrip.hidden = columnGrip.hidden = !at;
    if (!at) return;
    const r = rows[at[0]].getBoundingClientRect(), c = rows[0].cells[at[1]].getBoundingClientRect();
    Object.assign(rowGrip.style, { left: `${Math.round(t.left - 7)}px`, top: `${Math.round(r.top + r.height / 2 - 11)}px` });
    Object.assign(columnGrip.style, { left: `${Math.round(c.left + c.width / 2 - 11)}px`, top: `${Math.round(t.top - 7)}px` });
  }
  /** Tint the row or column a grip or menu is about to act on. */
  function markCells(target) {
    for (const old of surface.querySelectorAll('.tbl-pick')) old.classList.remove('tbl-pick');
    if (!target) return;
    const table = surface.querySelector(`[data-block="${CSS.escape(target.id)}"] table`);
    if (!table) return;
    const cells = target.row != null ? [...(table.rows[target.row]?.cells ?? [])] : [...table.rows].map(row => row.cells[target.column]).filter(Boolean);
    for (const c of cells) c.classList.add('tbl-pick');
  }

  // -- the table menu: row grip, column grip, or right-click in a cell ----------------------------

  function closeTableMenu() { tableMenu?.node.remove(); tableMenu = null; markCells(null); }
  function openTableMenu(id, at, rect, scope) {
    closeTableMenu(); closeBlockMenu(); closeSlash();
    const b = M.block(state.doc, id);
    if (!b || b.kind !== 'table') return;
    const [r, c] = at, rowCount = b.rows.length, columnCount = b.rows[0]?.length ?? 0;
    const node = el('div', 'popup menu table-menu');
    node.setAttribute('role', 'menu');
    node.setAttribute('aria-label', scope === 'row' ? `Row ${r + 1}` : scope === 'column' ? `Column ${c + 1}` : 'Table');
    const run = result => { closeTableMenu(); if (result) api.editor.perform(result); };
    const patch = fields => { closeTableMenu(); api.apply([{ op: 'update_block', id, patch: fields }]); };
    const items = [];
    if (scope !== 'column') {
      items.push(['Insert row above', () => run(E.tableAddRow(state.doc, id, r - 1)), 'arrow-up']);
      items.push(['Insert row below', () => run(E.tableAddRow(state.doc, id, r)), 'arrow-down']);
    }
    if (scope !== 'row') {
      items.push(['Insert column left', () => run(E.tableAddColumn(state.doc, id, c - 1)), 'arrow-left']);
      items.push(['Insert column right', () => run(E.tableAddColumn(state.doc, id, c)), 'arrow-right']);
    }
    items.push(null);
    if (scope !== 'column' && r === 0) items.push([b.header ? 'Plain first row' : 'Header row', () => patch({ header: !b.header }), 'table', b.header]);
    if (scope === 'column') {
      const align = b.align?.[c] || 'left';
      for (const side of ['left', 'center', 'right']) {
        items.push([`Align ${side}`, () => patch({ align: Array.from({ length: columnCount }, (_, i) => (i === c ? side : b.align?.[i] || 'left')) }), `align-${side}`, align === side]);
      }
    }
    if (scope !== 'column') items.push(['Delete row', () => run(E.tableDeleteRow(state.doc, id, r)), 'trash', false, rowCount < 2, true]);
    if (scope !== 'row') items.push(['Delete column', () => run(E.tableDeleteColumn(state.doc, id, c)), 'trash', false, columnCount < 2, true]);
    for (const item of items) {
      if (!item) { if (node.lastChild && !node.lastChild.classList.contains('menu-rule')) node.append(el('div', 'menu-rule')); continue; }
      const [label, action, icon, on, disabled, danger] = item;
      const row = el('button', `menu-row compact${on ? ' on' : ''}${danger ? ' danger' : ''}`);
      row.type = 'button'; row.setAttribute('role', 'menuitem'); row.disabled = Boolean(disabled);
      row.append(glyph(icon), el('span', 'menu-label', label));
      if (on) row.append(glyph('check'));
      row.addEventListener('mousedown', e => e.preventDefault());
      row.addEventListener('click', action);
      node.append(row);
    }
    if (node.lastChild?.classList.contains('menu-rule')) node.lastChild.remove();
    overlay.append(node);
    tableMenu = { node, id };
    markCells(scope === 'row' ? { id, row: r } : scope === 'column' ? { id, column: c } : null);
    placePopup(node, rect, { prefer: 'below' });
    node.querySelector('button:not(:disabled)')?.focus({ preventScroll: true });
    node.addEventListener('keydown', e => {
      const buttons = [...node.querySelectorAll('button:not(:disabled)')], i = buttons.indexOf(document.activeElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); buttons[(i + (e.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length].focus(); }
      else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeTableMenu(); surface.focus({ preventScroll: true }); }
    });
  }
  api.page.addEventListener('pointermove', e => {
    if (dragging || e.target.closest('.popup')) return;
    clearTimeout(hideTimer);
    const onControl = e.target.closest('.popup, .block-handle, .table-grip, .table-addbar');
    const node = onControl ? hovered : blockAt(e.clientY);
    if (!onControl && !tableMenu) cell = node?.classList.contains('blk-table') ? cellOf(e.target) ?? cell : null;
    if (node) showHandle(node);
  });
  api.page.addEventListener('pointerleave', () => { hideTimer = setTimeout(hideHandle, 400); });
  api.page.addEventListener('scroll', () => { if (hovered && !dragging) showHandle(hovered); closeSlash(); }, true);
  surface.addEventListener('keydown', e => { if (!e.metaKey && !e.ctrlKey && e.key.length === 1) hideHandle(); });
  addRow.addEventListener('click', () => { const id = hovered?.dataset.block; if (id) api.editor.perform(E.tableAddRow(state.doc, id, M.block(state.doc, id).rows.length - 1)); });
  addColumn.addEventListener('click', () => { const id = hovered?.dataset.block; if (id) api.editor.perform(E.tableAddColumn(state.doc, id, M.block(state.doc, id).rows[0].length - 1)); });
  rowGrip.addEventListener('click', () => { const id = hovered?.dataset.block; if (id && cell) openTableMenu(id, cell, rowGrip.getBoundingClientRect(), 'row'); });
  columnGrip.addEventListener('click', () => { const id = hovered?.dataset.block; if (id && cell) openTableMenu(id, cell, columnGrip.getBoundingClientRect(), 'column'); });
  for (const [grip, scope] of [[rowGrip, 'row'], [columnGrip, 'column']]) {
    grip.addEventListener('pointerenter', () => { const id = hovered?.dataset.block; if (id && cell && !tableMenu) markCells(scope === 'row' ? { id, row: cell[0] } : { id, column: cell[1] }); });
    grip.addEventListener('pointerleave', () => { if (!tableMenu) markCells(null); });
  }
  // Right-click in a cell: the table menu, without the browser selecting a word first.
  surface.addEventListener('mousedown', e => { if (e.button === 2 && cellOf(e.target)) e.preventDefault(); });
  surface.addEventListener('contextmenu', e => {
    const at = cellOf(e.target), id = e.target.closest('[data-block]')?.dataset.block;
    if (!at || !id) return;
    e.preventDefault();
    cell = at;
    openTableMenu(id, at, { left: e.clientX, right: e.clientX, top: e.clientY, bottom: e.clientY, width: 0, height: 0 }, 'cell');
  });

  let press = null;
  handle.addEventListener('pointerdown', e => {
    if (e.button !== 0 || !hovered) return;
    press = { x: e.clientX, y: e.clientY, id: hovered.dataset.block, node: hovered, moved: false };
    handle.setPointerCapture(e.pointerId);
    api.lastCaret = api.editor.selection()?.from ?? null;
    e.preventDefault();
  });
  handle.addEventListener('pointermove', e => {
    if (!press) return;
    if (!press.moved && Math.hypot(e.clientX - press.x, e.clientY - press.y) < 5) return;
    if (!press.moved) { press.moved = true; dragging = press; press.node.classList.add('dragging'); document.body.classList.add('dragging-block'); }
    const slot = dropSlot(e.clientY);
    indicator.hidden = false;
    const rect = surface.getBoundingClientRect();
    indicator.style.left = `${Math.round(rect.left)}px`; indicator.style.width = `${Math.round(rect.width)}px`;
    indicator.style.top = `${Math.round(slot.y)}px`;
    press.slot = slot;
    handle.style.top = `${Math.round(e.clientY - 12)}px`;
    const page = api.page.getBoundingClientRect();
    if (e.clientY < page.top + 60) api.page.scrollBy(0, -14); else if (e.clientY > page.bottom - 60) api.page.scrollBy(0, 14);
  });
  const endPress = e => {
    if (!press) return;
    const done = press; press = null; dragging = null;
    handle.releasePointerCapture?.(e.pointerId);
    indicator.hidden = true; done.node.classList.remove('dragging'); document.body.classList.remove('dragging-block');
    if (done.moved && done.slot && e.type === 'pointerup') {
      const ids = state.doc.blocks.map(b => b.id), from = ids.indexOf(done.id);
      if (done.slot.index !== from && done.slot.index !== from + 1) api.editor.perform(E.moveBlock(state.doc, done.id, state.doc.blocks[done.slot.index]?.id ?? null));
    } else if (!done.moved && e.type === 'pointerup') openBlockMenu(done.id, handle.getBoundingClientRect());
    showHandle(done.node.isConnected ? done.node : null);
  };
  handle.addEventListener('pointerup', endPress);
  handle.addEventListener('pointercancel', endPress);
  handle.addEventListener('keydown', e => {
    if ((e.key === 'Enter' || e.key === ' ') && hovered) { e.preventDefault(); openBlockMenu(hovered.dataset.block, handle.getBoundingClientRect()); }
  });
  /** The boundary between blocks nearest to `y`: its index in the doc and where to draw the line. */
  function dropSlot(y) {
    const kids = [...surface.children].filter(k => !k.dataset.ghost);
    let best = { index: kids.length, y: kids.at(-1)?.getBoundingClientRect().bottom ?? 0, distance: Infinity };
    kids.forEach((kid, i) => {
      const rect = kid.getBoundingClientRect();
      for (const [edge, index] of [[rect.top, i], [rect.bottom, i + 1]]) {
        const distance = Math.abs(edge - y);
        if (distance < best.distance) best = { index, y: edge, distance };
      }
    });
    return best;
  }

  document.addEventListener('pointerdown', e => {
    if (slash && !e.target.closest('.slash')) closeSlash();
    if (block && !e.target.closest('.block-menu') && !e.target.closest('.block-handle')) closeBlockMenu();
    if (tableMenu && !e.target.closest('.table-menu') && !e.target.closest('.table-grip')) closeTableMenu();
  }, { signal: api.signal });

  /** Keys while a menu is open. Returns true when consumed. */
  function handleKey(e) {
    if (!slash) return false;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (slash.items.length) { slash.index = (slash.index + (e.key === 'ArrowDown' ? 1 : -1) + slash.items.length) % slash.items.length; renderSlash(); }
      return true;
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault();
      if (slash.items[slash.index]) chooseSlash(slash.items[slash.index]); else closeSlash();
      return true;
    }
    if (e.key === 'Escape') { e.preventDefault(); closeSlash(); return true; }
    return false;
  }

  return {
    onInput, handleKey, openBlockMenu, closeAll() { closeSlash(); closeBlockMenu(); closeTableMenu(); },
    get open() { return Boolean(slash || block || tableMenu); },
    reposition() { if (hovered?.isConnected) showHandle(hovered); else hideHandle(); },
    partElement,
  };
}
