// Structural edits as pure functions: given the doc and a caret, they return the
// operations to send and where the caret goes. The editor applies the result
// locally and sends it as one batch, so each action is one undo step.
//
// A location is { block, part, offset }: `part` names the editable part
// (see model.js) and `offset` counts UTF-16 units of that part's plain text.
// A location with `part: null` and a `side` ('before' or 'after') is the edge of
// a block that has no text of its own.
import { parseInline, plainText, serializeRuns, sliceRuns, insertText, deleteRange as deleteRunRange } from './inline.js';
import {
  TEXT_KINDS, INSERT_ITEMS, block as findBlock, clone, getPart, setPart, partPlain, partPatch, partsOf, indexOf, idsOf,
  itemAt, itemPaths, isRich, newId, insertAtOps, replaceOps, moveOps,
} from './model.js';

const cellOptions = (path, trim = true) => ({ cell: path.startsWith('cell:'), trim });
const runsOf = (b, path) => isRich(b, path) ? parseInline(getPart(b, path)) : [{ t: getPart(b, path) }];
const writeRuns = (b, path, runs, trim = true) => isRich(b, path) ? serializeRuns(runs, cellOptions(path, trim)) : plainText(runs);
const make = (kind, extra = {}) => ({ id: '', ...INSERT_ITEMS.find(i => i.make && i.make().kind === kind).make(), ...extra });
const paragraph = (id, text = '') => ({ id, kind: 'paragraph', text });
const update = (id, patch) => ({ op: 'update_block', id, patch });
const lengthOf = (b, path) => partPlain(b, path).length;

/** The text of a part split at an offset, each half as stored Markdown. */
export function splitPart(b, path, offset, trim = true) {
  const runs = runsOf(b, path), total = plainText(runs).length;
  return [writeRuns(b, path, sliceRuns(runs, 0, offset), trim), writeRuns(b, path, sliceRuns(runs, offset, total), trim)];
}

/** Tidy list items: no empty `children`, no `checked: undefined`. */
export function tidyItems(items) {
  return items.map(item => {
    const { children, checked, ...rest } = item;
    const out = { ...rest };
    if (checked != null) out.checked = checked;
    if (children?.length) out.children = tidyItems(children);
    return out;
  });
}
const parentList = (items, steps) => steps.length ? (itemAt(items, steps.join('.')).children ||= []) : items;
const stepsOf = path => path.slice(5).split('.').map(Number);
const itemPath = steps => `item:${steps.join('.')}`;

/** Every list item in reading order with its path steps. */
function flatItems(items) {
  return itemPaths(items).map(p => ({ path: p, item: itemAt(items, p) }));
}

// -- Enter -------------------------------------------------------------------------

/** Enter in a text block or list item. Null when the browser should handle it. */
export function enter(doc, loc) {
  const b = findBlock(doc, loc.block);
  if (!b) return null;
  if (b.kind === 'list') return enterInList(doc, b, loc);
  if (!TEXT_KINDS.has(b.kind)) return null;
  const taken = idsOf(doc), at = indexOf(doc, b.id), empty = partPlain(b, 'text') === '';
  if (empty && (b.kind === 'quote' || b.kind === 'callout' || b.kind === 'heading' || b.kind === 'title')) {
    // Enter on an empty quote, callout or heading leaves it.
    return { ops: replaceOps(doc, b.id, [paragraph(b.id)]), select: { block: b.id, part: 'text', offset: 0 } };
  }
  if (loc.offset === 0 && !empty) {
    const id = newId(taken);
    return { ops: insertAtOps(doc, at, [paragraph(id)]), select: { block: b.id, part: 'text', offset: 0 } };
  }
  const [left, right] = splitPart(b, 'text', loc.offset), id = newId(taken);
  const keep = b.kind === 'quote' || b.kind === 'callout';
  const next = keep ? { ...clone(b), id, text: right } : paragraph(id, right);
  return {
    ops: [update(b.id, { text: left }), { op: 'insert_blocks', after: b.id, blocks: [next] }],
    select: { block: id, part: 'text', offset: 0 },
  };
}

function enterInList(doc, b, loc) {
  const steps = stepsOf(loc.part), item = itemAt(b.items, steps.join('.'));
  const taken = idsOf(doc);
  if (!item) return null;
  if (partPlain(b, loc.part) === '') {
    return steps.length > 1 ? outdent(doc, loc) : leaveList(doc, b, loc.part, item.text, taken);
  }
  const items = clone(b.items), list = parentList(items, steps.slice(0, -1)), at = steps.at(-1) - 1;
  const [left, right] = splitPart(b, loc.part, loc.offset);
  const current = list[at], fresh = { text: right };
  if (current.checked != null) fresh.checked = false;
  if (current.children?.length) fresh.children = current.children;
  current.text = left; delete current.children;
  list.splice(at + 1, 0, fresh);
  const next = [...steps.slice(0, -1), steps.at(-1) + 1];
  return { ops: [update(b.id, { items: tidyItems(items) })], select: { block: b.id, part: itemPath(next), offset: 0 } };
}

/** List items in reading order with their depth, so a range can be cut and the tree rebuilt. */
function flatten(items, depth = 0, out = []) {
  for (const item of items) {
    const { children, ...rest } = item;
    out.push({ depth, ...rest });
    flatten(children || [], depth + 1, out);
  }
  return out;
}
/** The nested items for a flat run; a child whose parent is gone moves up a level. */
function rebuild(flat) {
  const root = [], stack = [root];
  let previous = -1;
  for (const { depth, ...item } of flat) {
    const d = Math.max(0, Math.min(depth, previous + 1)), node = { ...item, children: [] };
    stack[d].push(node);
    stack.length = d + 1;
    stack[d + 1] = node.children;
    previous = d;
  }
  return tidyItems(root);
}
const readingIndex = (b, path) => itemPaths(b.items).indexOf(path.slice(5));

/** Take an item out of its list as a paragraph holding `text`; the rest of the list stays around it. */
function leaveList(doc, b, path, text, taken) {
  const flat = flatten(b.items), at = readingIndex(b, path);
  const before = rebuild(flat.slice(0, at)), after = rebuild(flat.slice(at + 1));
  const id = newId(taken);
  taken.add(id);
  const blocks = [paragraph(id, text)];
  if (after.length) blocks.push({ ...clone(b), id: newId(taken), items: after });
  const ops = before.length
    ? [update(b.id, { items: before }), { op: 'insert_blocks', after: b.id, blocks }]
    : replaceOps(doc, b.id, blocks);
  return { ops, select: { block: id, part: 'text', offset: 0 } };
}

// -- Backspace and Delete ----------------------------------------------------------

function joined(b, path, other, otherPath) {
  const runs = [...runsOf(b, path), ...runsOf(other, otherPath)];
  return { text: writeRuns(b, path, runs), at: lengthOf(b, path) };
}

/** Backspace with the caret at the start of a part. Null when there is nothing to do. */
export function backspaceAtStart(doc, loc) {
  const b = findBlock(doc, loc.block);
  if (!b) return null;
  const at = indexOf(doc, b.id), top = { block: b.id, part: 'text', offset: 0 };
  if (b.kind === 'title' || b.kind === 'heading' || b.kind === 'quote' || b.kind === 'callout') {
    return { ops: replaceOps(doc, b.id, [paragraph(b.id, b.text)]), select: top };
  }
  if (b.kind === 'code') {
    if (loc.part !== 'text') return null;
    return { ops: replaceOps(doc, b.id, [paragraph(b.id, serializeRuns([{ t: b.text }]))]), select: top };
  }
  if (b.kind === 'list') {
    const steps = stepsOf(loc.part);
    if (steps.length > 1) return outdent(doc, loc);
    const item = itemAt(b.items, steps.join('.'));
    return leaveList(doc, b, loc.part, item.text, idsOf(doc));
  }
  if (b.kind !== 'paragraph' || at === 0) return null;
  const prev = doc.blocks[at - 1];
  if (TEXT_KINDS.has(prev.kind)) {
    const { text, at: offset } = joined(prev, 'text', b, 'text');
    return { ops: [update(prev.id, { text }), { op: 'delete_blocks', ids: [b.id] }], select: { block: prev.id, part: 'text', offset } };
  }
  if (prev.kind === 'list') {
    const paths = itemPaths(prev.items), last = paths.at(-1), part = `item:${last}`;
    const { text, at: offset } = joined(prev, part, b, 'text');
    const next = setPart(prev, part, text);
    return { ops: [update(prev.id, { items: next.items }), { op: 'delete_blocks', ids: [b.id] }], select: { block: prev.id, part, offset } };
  }
  if (prev.kind === 'divider' || prev.kind === 'page_break' || prev.kind === 'toc') {
    return { ops: [{ op: 'delete_blocks', ids: [prev.id] }], select: top };
  }
  return { ops: [], select: endOf(prev) };
}

/** The caret location at the end of a block's last text part, or null. */
export function endOf(b) {
  const parts = partsOf(b).filter(p => p.path !== 'lang' && p.path !== 'alt');
  const last = parts.at(-1);
  return last ? { block: b.id, part: last.path, offset: lengthOf(b, last.path) } : null;
}
export function startOf(b) {
  const first = partsOf(b).find(p => p.path !== 'lang');
  return first ? { block: b.id, part: first.path, offset: 0 } : null;
}

/** Delete with the caret at the end of a text block: the next text block joins this one. */
export function deleteAtEnd(doc, loc) {
  const b = findBlock(doc, loc.block), at = indexOf(doc, loc.block);
  if (!b || !TEXT_KINDS.has(b.kind) || at === doc.blocks.length - 1) return null;
  const next = doc.blocks[at + 1];
  if (!TEXT_KINDS.has(next.kind)) return null;
  const { text, at: offset } = joined(b, 'text', next, 'text');
  return { ops: [update(b.id, { text }), { op: 'delete_blocks', ids: [next.id] }], select: { block: b.id, part: 'text', offset } };
}

// -- nesting -----------------------------------------------------------------------

export const MAX_DEPTH = 4;

/** Tab: make the item a child of the item above it. */
export function indent(doc, loc) {
  const b = findBlock(doc, loc.block);
  if (b?.kind !== 'list') return null;
  const steps = stepsOf(loc.part), at = steps.at(-1) - 1;
  if (at === 0 || steps.length >= MAX_DEPTH) return null;
  const items = clone(b.items), list = parentList(items, steps.slice(0, -1));
  const [item] = list.splice(at, 1), above = list[at - 1];
  (above.children ||= []).push(item);
  const next = [...steps.slice(0, -1), at, above.children.length];
  return { ops: [update(b.id, { items: tidyItems(items) })], select: { block: b.id, part: itemPath(next), offset: loc.offset } };
}

/** Shift+Tab: make the item a sibling after its parent; the siblings below become its children. */
export function outdent(doc, loc) {
  const b = findBlock(doc, loc.block);
  if (b?.kind !== 'list') return null;
  const steps = stepsOf(loc.part);
  if (steps.length < 2) return null;
  const items = clone(b.items), parentSteps = steps.slice(0, -1);
  const parent = itemAt(items, parentSteps.join('.')), siblings = parent.children, at = steps.at(-1) - 1;
  const [item] = siblings.splice(at, 1), following = siblings.splice(at);
  item.children = [...(item.children || []), ...following];
  const grand = parentList(items, parentSteps.slice(0, -1));
  grand.splice(parentSteps.at(-1), 0, item);
  const next = [...parentSteps.slice(0, -1), parentSteps.at(-1) + 1];
  return { ops: [update(b.id, { items: tidyItems(items) })], select: { block: b.id, part: itemPath(next), offset: loc.offset } };
}

/** Click on a checkbox. */
export function toggleCheck(doc, id, path) {
  const b = findBlock(doc, id), item = itemAt(b.items, path.slice(5));
  const items = clone(b.items);
  const target = itemAt(items, path.slice(5));
  target.checked = !item.checked;
  return { ops: [update(id, { items })], select: null };
}

// -- turning blocks into others --------------------------------------------------------

/** The Markdown strings a block's text amounts to, one per line it would become. */
function texts(b) {
  if (TEXT_KINDS.has(b.kind)) return [b.text];
  if (b.kind === 'code') return b.text.split('\n').map(line => serializeRuns([{ t: line }]));
  if (b.kind === 'list') return flatItems(b.items).map(({ item }) => item.text);
  return null;
}

/**
 * Turn a block into another style: 'title' 'h1' 'h2' 'h3' 'text' 'quote' 'code'
 * 'bullets' 'numbers' 'checklist' or 'callout:<tone>'. The first resulting block
 * keeps the id, so comments stay on it.
 */
export function turnInto(doc, id, target, caret = 0) {
  const b = findBlock(doc, id);
  const pieces = b && texts(b);
  if (!pieces) return null;
  const lines = pieces.length ? pieces : [''];
  const same = { select: { block: id, part: startOf(b)?.part ?? 'text', offset: caret } };
  if (b.kind === 'heading' && /^h[123]$/.test(target)) return { ops: [update(id, { level: Number(target[1]) })], ...same };
  if (b.kind === 'callout' && target.startsWith('callout:')) return { ops: [update(id, { tone: target.slice(8) })], ...same };
  if (b.kind === 'list' && (target === 'bullets' || target === 'numbers')) {
    return { ops: [update(id, { ordered: target === 'numbers' })], ...same };
  }
  if (b.kind === 'list' && target === 'checklist') {
    const items = clone(b.items);
    for (const { item } of flatItems(items)) item.checked ??= false;
    return { ops: [update(id, { items: tidyItems(items), ordered: false })], ...same };
  }
  const taken = idsOf(doc);
  const blocks = [];
  const one = (text, extra) => ({ ...extra, id: blocks.length ? newId(taken) : id, text });
  if (target === 'bullets' || target === 'numbers' || target === 'checklist') {
    const items = lines.map(text => target === 'checklist' ? { text, checked: false } : { text });
    blocks.push({ id, kind: 'list', ordered: target === 'numbers', items });
  } else if (target === 'code') {
    blocks.push({ id, kind: 'code', lang: '', text: lines.map(l => plainText(parseInline(l))).join('\n') });
  } else {
    const shape = target === 'title' ? { kind: 'title' } : target === 'text' ? { kind: 'paragraph' } : target === 'quote' ? { kind: 'quote' }
      : /^h[123]$/.test(target) ? { kind: 'heading', level: Number(target[1]) } : target.startsWith('callout:') ? { kind: 'callout', tone: target.slice(8) } : null;
    if (!shape) return null;
    const joinedText = b.kind === 'list' || b.kind === 'code' ? lines : [lines.join('\n')];
    for (const text of joinedText) blocks.push(one(text, shape));
  }
  const first = startOf(blocks[0]);
  return { ops: replaceOps(doc, id, blocks), select: first && { ...first, offset: Math.min(caret, partPlain(blocks[0], first.part).length) } };
}

/** Insert a fresh block after `afterId`, or in place of an empty paragraph. */
export function insertBlock(doc, afterId, spec, { replaceEmpty = true } = {}) {
  const taken = idsOf(doc), id = newId(taken);
  const created = { ...clone(spec), id };
  const current = afterId == null ? null : findBlock(doc, afterId);
  const select = startOf(created) ?? { block: id, part: null, side: 'after' };
  const ops = replaceEmpty && current?.kind === 'paragraph' && partPlain(current, 'text') === ''
    ? replaceOps(doc, current.id, [created])
    : [{ op: 'insert_blocks', after: afterId ?? undefined, blocks: [created] }];
  // Structures with no text to type in get a paragraph after them to continue in.
  const island = !startOf(created);
  if (island) {
    const next = newId(new Set([...taken, id]));
    const tail = { id: next, kind: 'paragraph', text: '' };
    const last = ops.at(-1);
    if (last.op === 'insert_blocks') last.blocks.push(tail); else ops.push({ op: 'insert_blocks', after: id, blocks: [tail] });
    return { ops, select: { block: next, part: 'text', offset: 0 }, id };
  }
  return { ops, select, id };
}

export function duplicateBlock(doc, id) {
  const b = findBlock(doc, id), copy = { ...clone(b), id: newId(idsOf(doc)) };
  return { ops: [{ op: 'insert_blocks', after: id, blocks: [copy] }], select: startOf(copy), id: copy.id };
}

export function deleteBlock(doc, id) {
  const at = indexOf(doc, id), neighbour = doc.blocks[at + 1] ?? doc.blocks[at - 1];
  const ops = [{ op: 'delete_blocks', ids: [id] }];
  const select = neighbour ? (at > 0 && doc.blocks[at - 1] ? endOf(doc.blocks[at - 1]) : startOf(neighbour)) : null;
  return { ops, select };
}

export function moveBlock(doc, id, beforeId) {
  return { ops: moveOps(doc, id, beforeId), select: null };
}

// -- tables ------------------------------------------------------------------------------

export function tableAddRow(doc, id, after) {
  const b = findBlock(doc, id), columns = b.rows[0]?.length || 1;
  const rows = clone(b.rows);
  rows.splice(after + 1, 0, Array(columns).fill(''));
  return { ops: [update(id, { rows })], select: { block: id, part: `cell:${after + 1}:0`, offset: 0 } };
}
export function tableAddColumn(doc, id, after) {
  const b = findBlock(doc, id);
  const rows = b.rows.map(row => { const copy = [...row]; copy.splice(after + 1, 0, ''); return copy; });
  const patch = { rows };
  if (b.align?.length) { const align = [...b.align]; align.splice(after + 1, 0, 'left'); patch.align = align; }
  return { ops: [update(id, patch)], select: { block: id, part: `cell:0:${after + 1}`, offset: 0 } };
}
export function tableDeleteRow(doc, id, row) {
  const b = findBlock(doc, id);
  if (b.rows.length < 2) return null;
  const rows = b.rows.filter((_, r) => r !== row);
  return { ops: [update(id, { rows })], select: { block: id, part: `cell:${Math.min(row, rows.length - 1)}:0`, offset: 0 } };
}
export function tableDeleteColumn(doc, id, column) {
  const b = findBlock(doc, id);
  if ((b.rows[0]?.length || 0) < 2) return null;
  const patch = { rows: b.rows.map(row => row.filter((_, c) => c !== column)) };
  if (b.align?.length) patch.align = b.align.filter((_, c) => c !== column);
  return { ops: [update(id, patch)], select: { block: id, part: `cell:0:${Math.min(column, patch.rows[0].length - 1)}`, offset: 0 } };
}
/** Enter in a cell goes down a row, adding one at the bottom. */
export function tableDown(doc, loc) {
  const b = findBlock(doc, loc.block), [r, c] = loc.part.slice(5).split(':').map(Number);
  if (r + 1 < b.rows.length) return { ops: [], select: { block: b.id, part: `cell:${r + 1}:${c}`, offset: partPlain(b, `cell:${r + 1}:${c}`).length } };
  const added = tableAddRow(doc, b.id, r);
  added.select = { block: b.id, part: `cell:${r + 1}:${c}`, offset: 0 };
  return added;
}
/** Tab / Shift+Tab move between cells; Tab in the last cell adds a row. */
export function tableStep(doc, loc, back) {
  const b = findBlock(doc, loc.block), [r, c] = loc.part.slice(5).split(':').map(Number);
  const columns = b.rows[0].length;
  let next = r * columns + c + (back ? -1 : 1);
  if (next < 0) return null;
  if (next >= b.rows.length * columns) {
    const added = tableAddRow(doc, b.id, r);
    added.select = { block: b.id, part: `cell:${r + 1}:0`, offset: 0 };
    return added;
  }
  next = [Math.floor(next / columns), next % columns];
  const part = `cell:${next[0]}:${next[1]}`;
  return { ops: [], select: { block: b.id, part, offset: partPlain(b, part).length } };
}

// -- text in a part ----------------------------------------------------------------------

/** The patch that writes a part's new Markdown. */
export function setPartOps(doc, id, path, value) {
  const b = setPart(findBlock(doc, id), path, value);
  return [update(id, partPatch(b, path))];
}

/** Insert text (with marks from runs) into a part at an offset. Returns the new Markdown. */
export function insertIntoPart(b, path, offset, text, marks = {}) {
  return writeRuns(b, path, insertText(runsOf(b, path), offset, text, isRich(b, path) ? marks : {}));
}

// -- deleting across parts -----------------------------------------------------------------

const textual = b => TEXT_KINDS.has(b.kind);
const pointKey = (doc, loc) => {
  const b = findBlock(doc, loc.block), parts = partsOf(b).map(p => p.path);
  return [indexOf(doc, loc.block), loc.part ? Math.max(0, parts.indexOf(loc.part)) : loc.side === 'after' ? 1e6 : -1, loc.offset || 0];
};
/** Two locations in document order. */
export function ordered(doc, a, b) {
  const [x, y] = [pointKey(doc, a), pointKey(doc, b)];
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i] ? [a, b] : [b, a];
  return [a, b];
}
const joinMd = (b, path, left, other, otherPath, right) =>
  writeRuns(b, path, [...runsOf(setPart(b, path, left), path), ...runsOf(setPart(other, otherPath, right), otherPath)]);

/**
 * Delete the selection between two locations, in either order. The first block
 * keeps its kind and the last one's remaining text joins it, as in any word
 * processor. Whole blocks in between go.
 */
export function deleteSelection(doc, one, two) {
  const [from, to] = ordered(doc, one, two);
  const F = findBlock(doc, from.block), L = findBlock(doc, to.block);
  if (from.block === to.block) return deleteWithin(doc, F, from, to);
  const ops = [], doomed = new Set(doc.blocks.slice(indexOf(doc, F.id) + 1, indexOf(doc, L.id)).map(b => b.id));
  const start = from.part ? from : null, end = to.part ? to : null;
  if (!start && from.side === 'before') doomed.add(F.id);
  if (!end && to.side === 'after') doomed.add(L.id);
  let select = start ?? (from.side === 'after' ? endOf(F) : null);
  if (start && end && textual(L) && (textual(F) || F.kind === 'list')) {
    const [keep] = splitPart(F, start.part, start.offset, false), [, tail] = splitPart(L, end.part, end.offset, false);
    if (textual(F)) {
      ops.push(update(F.id, { text: joinMd(F, 'text', keep, L, 'text', tail) }));
      select = { block: F.id, part: 'text', offset: partPlain(setPart(F, 'text', keep), 'text').length };
    } else {
      const flat = flatten(F.items), at = readingIndex(F, start.part);
      flat[at].text = joinMd(F, start.part, keep, L, 'text', tail);
      ops.push(update(F.id, { items: rebuild(flat.slice(0, at + 1)) }));
    }
    doomed.add(L.id);
  } else if (start && end && textual(F) && L.kind === 'list') {
    const [keep] = splitPart(F, start.part, start.offset, false), [, tail] = splitPart(L, end.part, end.offset, false);
    ops.push(update(F.id, { text: joinMd(F, 'text', keep, L, 'item:1', tail) }));
    select = { block: F.id, part: 'text', offset: partPlain(setPart(F, 'text', keep), 'text').length };
    const rest = flatten(L.items).slice(readingIndex(L, end.part) + 1);
    if (rest.length) ops.push(update(L.id, { items: rebuild(rest) })); else doomed.add(L.id);
  } else {
    // Anything else: clear the covered text in the end blocks and drop what lies between.
    if (start) ops.push(...clearFrom(F, start));
    if (end) ops.push(...clearTo(L, end));
  }
  if (doomed.size) ops.push({ op: 'delete_blocks', ids: [...doomed] });
  return { ops, select: select && !doomed.has(select.block) ? select : null };
}

function clearFrom(b, loc) {
  if (b.kind === 'list') {
    const flat = flatten(b.items), at = readingIndex(b, loc.part);
    flat[at].text = splitPart(b, loc.part, loc.offset)[0];
    return [update(b.id, { items: rebuild(flat.slice(0, at + 1)) })];
  }
  if (b.kind === 'table') return clearCells(b, loc, null);
  if (textual(b)) return [update(b.id, { text: splitPart(b, 'text', loc.offset)[0] })];
  if (b.kind === 'code' && loc.part === 'text') return [update(b.id, { text: b.text.slice(0, loc.offset) })];
  return [];
}
function clearTo(b, loc) {
  if (b.kind === 'list') {
    const flat = flatten(b.items), at = readingIndex(b, loc.part);
    flat[at].text = splitPart(b, loc.part, loc.offset)[1];
    return [update(b.id, { items: rebuild(flat.slice(at)) })];
  }
  if (b.kind === 'table') return clearCells(b, null, loc);
  if (textual(b)) return [update(b.id, { text: splitPart(b, 'text', loc.offset)[1] })];
  if (b.kind === 'code' && loc.part === 'text') return [update(b.id, { text: b.text.slice(loc.offset) })];
  return [];
}

/** Clear table cells from one location to another; the end cells keep what is outside the selection. */
function clearCells(b, from, to) {
  const rows = clone(b.rows), columns = rows[0].length;
  const index = loc => Number(loc.part.split(':')[1]) * columns + Number(loc.part.split(':')[2]);
  const lo = from ? index(from) : 0, hi = to ? index(to) : rows.length * columns - 1;
  for (let i = lo; i <= hi; i++) {
    const r = Math.floor(i / columns), c = i % columns;
    rows[r][c] = from && i === lo ? splitPart(b, from.part, from.offset)[0] : to && i === hi ? splitPart(b, to.part, to.offset)[1] : '';
  }
  return [update(b.id, { rows })];
}

/** A selection inside one block: one part, or two parts of a list or table. */
function deleteWithin(doc, b, from, to) {
  if (!from.part || !to.part) return { ops: [{ op: 'delete_blocks', ids: [b.id] }], select: null };
  if (from.part === to.part) {
    const text = writeRuns(b, from.part, deleteRunRange(runsOf(b, from.part), from.offset, to.offset));
    return { ops: setPartOps(doc, b.id, from.part, text), select: from };
  }
  if (b.kind === 'list') {
    const flat = flatten(b.items), a = readingIndex(b, from.part), z = readingIndex(b, to.part);
    const [keep] = splitPart(b, from.part, from.offset, false), [, tail] = splitPart(b, to.part, to.offset, false);
    flat[a].text = joinMd(b, from.part, keep, b, to.part, tail);
    flat.splice(a + 1, z - a);
    return { ops: [update(b.id, { items: rebuild(flat) })], select: from };
  }
  if (b.kind === 'table') return { ops: clearCells(b, from, to), select: from };
  return { ops: [], select: from };
}
