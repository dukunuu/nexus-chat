// The doc as the editor sees it: blocks, their editable parts, local operation
// application for the operations the editor itself sends, and the block shortcuts.
// Pure: no DOM, no network.
import { parseInline, plainText } from './inline.js';

export const TEXT_KINDS = new Set(['title', 'heading', 'paragraph', 'quote', 'callout']);
/** Kinds whose text is one editable part named `text`. */
export const FLOW_KINDS = new Set(['title', 'heading', 'paragraph', 'quote', 'callout', 'code']);
/** Kinds with no text of their own; they are placed, selected and deleted whole. */
export const ISLAND_KINDS = new Set(['divider', 'page_break', 'toc', 'chart', 'image']);

let counter = 0;
/** An id that is not in `taken`. The backend accepts any unused id. */
export function newId(taken = new Set()) {
  for (;;) {
    const id = `n${(Date.now() % 1e6).toString(36)}${(counter++).toString(36)}${Math.random().toString(36).slice(2, 5)}`;
    if (!taken.has(id)) return id;
  }
}
export const idsOf = doc => new Set(doc.blocks.map(b => b.id));
export const clone = value => structuredClone(value);
export const indexOf = (doc, id) => doc.blocks.findIndex(b => b.id === id);
export const block = (doc, id) => doc.blocks.find(b => b.id === id);

// -- parts -------------------------------------------------------------------------

/** A list item by its one-based dotted path ("2.1" is the first child of the second item). */
export function itemAt(items, path) {
  let list = items, item;
  for (const step of String(path).split('.')) {
    item = list?.[Number(step) - 1];
    if (!item) return undefined;
    list = item.children;
  }
  return item;
}
/** Every item path of a list, in reading order. */
export function itemPaths(items, prefix = '') {
  return items.flatMap((item, index) => {
    const path = `${prefix}${index + 1}`;
    return [path, ...itemPaths(item.children || [], `${path}.`)];
  });
}

/** The editable parts of a block: `rich` parts hold inline Markdown, `plain` ones raw text. */
export function partsOf(b) {
  switch (b.kind) {
    case 'title': case 'heading': case 'paragraph': case 'quote': case 'callout': return [{ path: 'text', rich: true }];
    case 'code': return [{ path: 'lang', rich: false }, { path: 'text', rich: false }];
    case 'list': return itemPaths(b.items).map(p => ({ path: `item:${p}`, rich: true }));
    case 'table': return b.rows.flatMap((row, r) => row.map((_, c) => ({ path: `cell:${r}:${c}`, rich: true })));
    case 'image': return [{ path: 'caption', rich: false }, { path: 'alt', rich: false }];
    default: return [];
  }
}
export const isRich = (b, path) => partsOf(b).find(p => p.path === path)?.rich ?? false;

/** The stored string of a part (inline Markdown for rich parts). */
export function getPart(b, path) {
  if (path === 'text' || path === 'lang' || path === 'caption' || path === 'alt') return b[path] ?? '';
  if (path.startsWith('item:')) return itemAt(b.items, path.slice(5))?.text ?? '';
  if (path.startsWith('cell:')) { const [r, c] = path.slice(5).split(':').map(Number); return b.rows[r]?.[c] ?? ''; }
  return '';
}
/** A copy of the block with one part replaced. */
export function setPart(b, path, value) {
  const next = clone(b);
  if (path === 'text' || path === 'lang' || path === 'caption' || path === 'alt') next[path] = value;
  else if (path.startsWith('item:')) { const item = itemAt(next.items, path.slice(5)); if (item) item.text = value; }
  else if (path.startsWith('cell:')) { const [r, c] = path.slice(5).split(':').map(Number); if (next.rows[r]) next.rows[r][c] = value; }
  return next;
}
/** The plain text of a part, which is what comment offsets count. */
export function partPlain(b, path) {
  const value = getPart(b, path);
  return isRich(b, path) ? plainText(parseInline(value)) : value;
}
/** The plain text of a whole block, as the core's `plain_text` builds it. */
export function blockPlain(b) {
  return partsOf(b).filter(p => p.path !== 'lang' && p.path !== 'alt' || b.kind === 'image' && p.path === 'alt')
    .map(p => partPlain(b, p.path)).join('\n');
}

/** The patch that writes one part back. Lists and tables send their whole array. */
export function partPatch(b, path) {
  if (path.startsWith('item:')) return { items: b.items };
  if (path.startsWith('cell:')) return { rows: b.rows };
  return { [path]: b[path] ?? '' };
}

/** The text style a block is set in (the doc's `text_styles` name). */
export function blockStyle(b) {
  if (b.style) return b.style;
  if (b.kind === 'title') return 'title';
  if (b.kind === 'heading') return `h${b.level}`;
  if (b.kind === 'quote') return 'quote';
  if (b.kind === 'code') return 'code';
  return 'body';
}

// -- applying operations locally -----------------------------------------------------

function mergePatch(target, patch) {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch)) return patch;
  const out = target && typeof target === 'object' && !Array.isArray(target) ? { ...target } : {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete out[key]; else out[key] = mergePatch(out[key], value);
  }
  return out;
}

/**
 * Apply the operations the editor sends to a copy of the doc, so the page can
 * show an edit before the backend has answered. Operations that need the
 * backend (Markdown parsing, designs) make this return null: the page waits.
 */
export function applyOps(doc, ops) {
  const next = clone(doc);
  for (const op of ops) {
    if (op.op === 'update_block') {
      const at = indexOf(next, op.id);
      if (at < 0 || 'kind' in op.patch || 'id' in op.patch) return null;
      next.blocks[at] = mergePatch(next.blocks[at], op.patch);
    } else if (op.op === 'insert_blocks') {
      const at = op.after == null ? next.blocks.length : indexOf(next, op.after) + 1;
      if (op.after != null && at === 0) return null;
      next.blocks.splice(at, 0, ...clone(op.blocks));
    } else if (op.op === 'delete_blocks') {
      next.blocks = next.blocks.filter(b => !op.ids.includes(b.id));
    } else if (op.op === 'move_blocks') {
      const moved = next.blocks.filter(b => op.ids.includes(b.id));
      next.blocks = next.blocks.filter(b => !op.ids.includes(b.id));
      const at = op.after == null ? next.blocks.length : indexOf(next, op.after) + 1;
      if (op.after != null && at === 0) return null;
      next.blocks.splice(at, 0, ...moved);
    } else if (op.op === 'set_title') {
      next.title = op.title;
    } else return null;
  }
  return next;
}

/** The operations that put the blocks in `order` (ids), using only `after` placement. */
export function moveOps(doc, id, beforeId) {
  const ids = doc.blocks.map(b => b.id).filter(other => other !== id);
  const at = beforeId == null ? ids.length : ids.indexOf(beforeId);
  if (at < 0) return [];
  if (at > 0) return [{ op: 'move_blocks', ids: [id], after: ids[at - 1] }];
  // There is no "first" in `after`, so everything else moves behind the block.
  return ids.length ? [{ op: 'move_blocks', ids, after: id }] : [];
}

/** Operations that insert `blocks` before the block at `index` (0 is the top). */
export function insertAtOps(doc, index, blocks) {
  if (index > 0) return [{ op: 'insert_blocks', after: doc.blocks[index - 1].id, blocks }];
  if (!doc.blocks.length) return [{ op: 'insert_blocks', blocks }];
  const ids = doc.blocks.map(b => b.id);
  return [{ op: 'insert_blocks', blocks }, { op: 'move_blocks', ids, after: blocks.at(-1).id }];
}

/** Replace one block by others, keeping the first one's id (kinds cannot change in place). */
export function replaceOps(doc, id, blocks) {
  const at = indexOf(doc, id);
  return [{ op: 'delete_blocks', ids: [id] }, ...insertAtOps({ blocks: doc.blocks.filter(b => b.id !== id) }, at, blocks)];
}

// -- outline and summaries -------------------------------------------------------------

/** Headings for the outline: { id, level, text } with level 0 for the title. */
export function outline(doc) {
  return doc.blocks.flatMap(b => b.kind === 'title' ? [{ id: b.id, level: 0, text: plainText(parseInline(b.text)) }]
    : b.kind === 'heading' ? [{ id: b.id, level: b.level, text: plainText(parseInline(b.text)) }] : []);
}
export const KIND_LABEL = { title: 'Title', heading: 'Heading', paragraph: 'Text', quote: 'Quote', callout: 'Callout', code: 'Code', list: 'List', table: 'Table', image: 'Image', chart: 'Chart', divider: 'Divider', page_break: 'Page break', toc: 'Contents' };
export function blockLabel(b) {
  if (b.kind === 'heading') return `Heading ${b.level}`;
  if (b.kind === 'list') return b.items.some(i => i.checked != null) ? 'Checklist' : b.ordered ? 'Numbered list' : 'Bulleted list';
  return KIND_LABEL[b.kind] || b.kind;
}

// -- what the slash menu and the style menu offer --------------------------------------

const item = (text = '', extra = {}) => ({ text, ...extra });
/** Blocks the insert menu makes. `image` needs a file, so it has no block here. */
export const INSERT_ITEMS = [
  { id: 'text', label: 'Text', hint: 'Plain paragraph', keys: 'paragraph body', make: () => ({ kind: 'paragraph', text: '' }) },
  { id: 'title', label: 'Title', hint: 'The document title', keys: 'title', make: () => ({ kind: 'title', text: '' }) },
  { id: 'h1', label: 'Heading 1', hint: 'Large section heading', keys: 'heading h1 #', make: () => ({ kind: 'heading', level: 1, text: '' }) },
  { id: 'h2', label: 'Heading 2', hint: 'Medium section heading', keys: 'heading h2 ##', make: () => ({ kind: 'heading', level: 2, text: '' }) },
  { id: 'h3', label: 'Heading 3', hint: 'Small section heading', keys: 'heading h3 ###', make: () => ({ kind: 'heading', level: 3, text: '' }) },
  { id: 'bullets', label: 'Bulleted list', hint: 'A simple list', keys: 'list bullet ul -', make: () => ({ kind: 'list', ordered: false, items: [item()] }) },
  { id: 'numbers', label: 'Numbered list', hint: 'A list with numbers', keys: 'list number ol 1.', make: () => ({ kind: 'list', ordered: true, items: [item()] }) },
  { id: 'checklist', label: 'Checklist', hint: 'Track tasks', keys: 'todo task check []', make: () => ({ kind: 'list', ordered: false, items: [item('', { checked: false })] }) },
  { id: 'quote', label: 'Quote', hint: 'Set a passage apart', keys: 'quote blockquote >', make: () => ({ kind: 'quote', text: '' }) },
  { id: 'callout', label: 'Callout', hint: 'Highlight a note', keys: 'callout note info', make: () => ({ kind: 'callout', tone: 'info', text: '' }) },
  { id: 'code', label: 'Code', hint: 'Monospaced code', keys: 'code snippet ```', make: () => ({ kind: 'code', lang: '', text: '' }) },
  { id: 'table', label: 'Table', hint: 'Rows and columns', keys: 'table grid', make: () => ({ kind: 'table', header: true, align: [], rows: [['', '', ''], ['', '', ''], ['', '', '']] }) },
  { id: 'image', label: 'Image', hint: 'Upload from your computer', keys: 'image picture photo', file: true },
  { id: 'chart', label: 'Chart', hint: 'A simple chart', keys: 'chart graph bar', make: () => ({ kind: 'chart', chart: { chart: 'bar', labels: ['Q1', 'Q2', 'Q3'], series: [{ name: 'Series 1', values: [3, 5, 4] }] } }) },
  { id: 'divider', label: 'Divider', hint: 'A horizontal rule', keys: 'divider rule hr ---', make: () => ({ kind: 'divider' }) },
  { id: 'page_break', label: 'Page break', hint: 'Start a new page in exports', keys: 'page break', make: () => ({ kind: 'page_break' }) },
  { id: 'toc', label: 'Contents', hint: 'A table of contents', keys: 'toc contents outline', make: () => ({ kind: 'toc' }) },
];
/** The kinds the style dropdown and "turn into" offer for text. */
export const STYLE_ITEMS = ['title', 'h1', 'h2', 'h3', 'text', 'quote', 'code'];
export const styleLabel = id => INSERT_ITEMS.find(i => i.id === id)?.label ?? id;
/** Which STYLE_ITEMS entry a block is in, or null for kinds that have none. */
export function styleItemOf(b) {
  if (b.kind === 'title') return 'title';
  if (b.kind === 'heading') return `h${b.level}`;
  if (b.kind === 'paragraph') return 'text';
  if (b.kind === 'quote') return 'quote';
  if (b.kind === 'code') return 'code';
  return null;
}
/** Filter the insert menu by what was typed after the slash. */
export function filterInsert(query) {
  const q = query.trim().toLowerCase();
  if (!q) return INSERT_ITEMS;
  const score = i => {
    const label = i.label.toLowerCase();
    if (label.startsWith(q)) return 0;
    if (label.split(' ').some(w => w.startsWith(q))) return 1;
    if (i.keys.split(' ').some(w => w.startsWith(q))) return 2;
    if (label.includes(q) || i.keys.includes(q)) return 3;
    return 9;
  };
  return INSERT_ITEMS.map(i => [score(i), i]).filter(([s]) => s < 9).sort((a, b) => a[0] - b[0]).map(([, i]) => i);
}

// -- shortcuts at the start of a line --------------------------------------------------

/**
 * Markdown shortcuts typed at the start of a paragraph. Given the whole plain
 * text of the part, returns the block it becomes and the text that remains, or
 * null. `# ` `## ` `### ` `- ` `* ` `1. ` `[] ` `> ` and "```" make blocks; `---`
 * makes a divider.
 */
export function blockShortcut(text) {
  let m;
  if ((m = /^(#{1,3}) /.exec(text))) return { make: { kind: 'heading', level: m[1].length, text: '' }, rest: text.slice(m[0].length) };
  if ((m = /^(?:[-*+] )?\[([ x]?)\] /.exec(text))) return { make: { kind: 'list', ordered: false, items: [item('', { checked: m[1] === 'x' })] }, rest: text.slice(m[0].length) };
  if ((m = /^[-*+] /.exec(text))) return { make: { kind: 'list', ordered: false, items: [item()] }, rest: text.slice(m[0].length) };
  if ((m = /^\d{1,3}[.)] /.exec(text))) return { make: { kind: 'list', ordered: true, items: [item()] }, rest: text.slice(m[0].length) };
  if ((m = /^> /.exec(text))) return { make: { kind: 'quote', text: '' }, rest: text.slice(m[0].length) };
  if ((m = /^```([\w+-]*)[ \n]$/.exec(text))) return { make: { kind: 'code', lang: m[1], text: '' }, rest: '' };
  if (text === '---' || text === '***' || text === '___') return { make: { kind: 'divider' }, rest: '', whole: true };
  return null;
}
/** Enter on a line that holds only "```lang" opens a code block. */
export function fenceOnEnter(text) {
  const m = /^```([\w+-]*)$/.exec(text);
  return m ? { make: { kind: 'code', lang: m[1], text: '' }, rest: '' } : null;
}

// -- comment anchors: offsets in a block's plain text -----------------------------------

/**
 * Where each editable part sits in the block's plain text, built the way the
 * core builds it (list items one per line, table cells joined by " | "), in
 * UTF-16 units. Comment anchors are ranges of this text.
 */
export function plainLayout(b) {
  const segments = [];
  let at = 0;
  const add = (path, separator) => {
    const length = partPlain(b, path).length;
    segments.push({ path, start: at, end: at + length, separator });
    at += length + separator.length;
  };
  if (b.kind === 'list') itemPaths(b.items).forEach(p => add(`item:${p}`, '\n'));
  else if (b.kind === 'table') b.rows.forEach((row, r) => row.forEach((_, c) => add(`cell:${r}:${c}`, c < row.length - 1 ? ' | ' : '\n')));
  else if (b.kind === 'image') { add('alt', '\n'); add('caption', ''); }
  else if (partsOf(b).some(p => p.path === 'text')) add('text', '');
  return segments;
}
export function blockOffset(b, path, offset) {
  const seg = plainLayout(b).find(s => s.path === path);
  return seg ? seg.start + offset : 0;
}
/** The parts, and the offsets in each, that a range of the block's plain text covers. */
export function rangeParts(b, start, end) {
  return plainLayout(b)
    .filter(s => s.end > start && s.start < end)
    .map(s => ({ path: s.path, start: Math.max(start, s.start) - s.start, end: Math.min(end, s.end) - s.start }));
}
export function blockPlainText(b) {
  const segments = plainLayout(b);
  return segments.map((s, i) => partPlain(b, s.path) + (i < segments.length - 1 ? s.separator : '')).join('');
}
