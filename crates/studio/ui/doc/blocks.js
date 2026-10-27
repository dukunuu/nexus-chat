// One element per block. Text lives only in elements marked `data-part`, so the
// editor can map any caret back to (block, part, offset). Text is only ever set
// as text nodes, never parsed as markup.
import { parseInline } from './inline.js';
import { renderRuns, renderPlain } from './dom.js';
import { blockStyle, outline } from './model.js';
import { renderChart } from '../render.js';
import { glyph } from './glyphs.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const TONES = { info: 'Info', warn: 'Warning', success: 'Success', note: 'Note' };
export const TONE_LABELS = TONES;

/** A URL the page may load for an image block's `src`. */
export function imageUrl(src, assets = {}) {
  const versioned = (path, hash) => hash ? `${path}?v=${hash}` : path;
  if (src.startsWith('https://')) {
    const route = assets[src] || 'missing';
    return versioned(`/asset/${route}`, assets[`/asset/${route}`]);
  }
  return versioned(`/assets/${src.replace(/^assets\//, '').split('/').map(encodeURIComponent).join('/')}`, assets[src]);
}

function part(tag, path, style, { plain = false, placeholder = '', editable } = {}) {
  const node = el(tag, 'part');
  node.dataset.part = path;
  if (style) node.dataset.ts = style;
  if (placeholder) node.dataset.ph = placeholder;
  if (plain) node.dataset.kind = 'plain';
  if (editable) node.contentEditable = editable;
  node.spellcheck = !plain;
  return node;
}
const fill = (node, md, deco) => { renderRuns(node, parseInline(md), deco); return node; };
const decoFor = (ctx, id, path) => ctx.decos?.[id]?.[path];

/** Build the element for one block. `ctx` has the doc, asset hashes and decorations. */
export function renderBlock(b, ctx) {
  const wrap = el('div', `blk blk-${b.kind.replace('_', '-')}`);
  wrap.dataset.block = b.id;
  const style = blockStyle(b);
  switch (b.kind) {
    case 'title': {
      const h = part('h1', 'text', style, { placeholder: 'Untitled' });
      wrap.append(fill(h, b.text, decoFor(ctx, b.id, 'text')));
      break;
    }
    case 'heading': {
      const h = part(`h${b.level + 1}`, 'text', style, { placeholder: `Heading ${b.level}` });
      wrap.dataset.level = b.level;
      wrap.append(fill(h, b.text, decoFor(ctx, b.id, 'text')));
      break;
    }
    case 'paragraph': {
      if (ctx.lonely === b.id) wrap.dataset.lonely = '1';
      const p = part('p', 'text', style, { placeholder: ctx.lonely === b.id ? 'Start writing, or press / for blocks' : 'Type / for blocks' });
      wrap.append(fill(p, b.text, decoFor(ctx, b.id, 'text')));
      break;
    }
    case 'quote': {
      const q = el('blockquote');
      q.append(fill(part('p', 'text', style, { placeholder: 'Quote' }), b.text, decoFor(ctx, b.id, 'text')));
      wrap.append(q);
      break;
    }
    case 'callout': {
      wrap.dataset.tone = b.tone;
      const badge = el('span', 'callout-icon');
      badge.contentEditable = 'false';
      badge.title = `${TONES[b.tone]} callout`;
      badge.append(glyph(b.tone));
      wrap.append(badge, fill(part('div', 'text', style, { placeholder: 'Callout' }), b.text, decoFor(ctx, b.id, 'text')));
      break;
    }
    case 'code': {
      const head = el('div', 'code-head');
      head.contentEditable = 'false';
      const lang = part('span', 'lang', null, { plain: true, placeholder: 'language', editable: 'plaintext-only' });
      renderPlain(lang, b.lang);
      lang.setAttribute('aria-label', 'Code language');
      head.append(lang);
      const pre = part('pre', 'text', style, { plain: true, placeholder: 'Code' });
      pre.spellcheck = false;
      renderPlain(pre, b.text, decoFor(ctx, b.id, 'text'));
      wrap.append(head, pre);
      break;
    }
    case 'list': wrap.append(renderItems(b, b.items, '', ctx)); break;
    case 'table': wrap.append(renderTable(b, ctx)); break;
    case 'image': wrap.append(renderImage(b, ctx)); break;
    case 'chart': {
      wrap.contentEditable = 'false';
      const figure = el('figure');
      figure.append(renderChart(b.chart, { colors: ctx.doc.theme.colors }));
      wrap.append(figure);
      break;
    }
    case 'divider': wrap.contentEditable = 'false'; wrap.append(el('hr')); break;
    case 'page_break': {
      wrap.contentEditable = 'false';
      wrap.append(el('span', 'rule'), el('span', 'label', 'Page break'), el('span', 'rule'));
      break;
    }
    case 'toc': wrap.contentEditable = 'false'; wrap.append(renderToc(ctx)); break;
    default: wrap.append(el('p', 'unknown', `Unsupported block: ${b.kind}`));
  }
  return wrap;
}

function renderItems(b, items, prefix, ctx) {
  const list = el(b.ordered ? 'ol' : 'ul', 'items');
  items.forEach((item, index) => {
    const path = `${prefix}${index + 1}`;
    const li = el('li');
    li.dataset.item = path;
    const row = el('div', 'li-row');
    if (item.checked != null) {
      li.classList.add('task');
      li.classList.toggle('done', item.checked);
      const box = el('span', 'check');
      box.contentEditable = 'false';
      box.setAttribute('role', 'checkbox');
      box.setAttribute('aria-checked', String(item.checked));
      box.setAttribute('aria-label', 'Done');
      box.tabIndex = -1;
      box.dataset.check = `item:${path}`;
      box.append(glyph('check'));
      row.append(box);
    }
    row.append(fill(part('div', `item:${path}`, 'body', { placeholder: 'List item' }), item.text, decoFor(ctx, b.id, `item:${path}`)));
    li.append(row);
    if (item.children?.length) li.append(renderItems(b, item.children, `${path}.`, ctx));
    list.append(li);
  });
  return list;
}

function renderTable(b, ctx) {
  const scroll = el('div', 'table-wrap');
  const table = el('table');
  b.rows.forEach((row, r) => {
    const tr = el('tr');
    row.forEach((cell, c) => {
      const header = b.header && r === 0;
      const td = el(header ? 'th' : 'td');
      if (header) td.scope = 'col';
      if (b.align?.[c] && b.align[c] !== 'left') td.style.textAlign = b.align[c];
      td.append(fill(part('div', `cell:${r}:${c}`, 'body', {}), cell, decoFor(ctx, b.id, `cell:${r}:${c}`)));
      tr.append(td);
    });
    table.append(tr);
  });
  scroll.append(table);
  return scroll;
}

function renderImage(b, ctx) {
  const figure = el('figure');
  figure.contentEditable = 'false';
  figure.dataset.width = b.width || 'full';
  const img = el('img');
  img.src = imageUrl(b.src, ctx.assets);
  img.alt = b.alt || '';
  img.draggable = false;
  img.addEventListener('error', () => figure.classList.add('missing'));
  const caption = part('figcaption', 'caption', 'caption', { plain: true, placeholder: 'Add a caption', editable: 'plaintext-only' });
  renderPlain(caption, b.caption || '');
  figure.append(img, caption);
  return figure;
}

function renderToc(ctx) {
  const nav = el('div', 'toc');
  nav.append(el('div', 'toc-title', 'Contents'));
  const entries = outline(ctx.doc).filter(e => e.level > 0);
  if (!entries.length) nav.append(el('p', 'toc-empty', 'Headings will appear here.'));
  const list = el('ol');
  for (const entry of entries) {
    const li = el('li');
    li.dataset.level = entry.level;
    const a = el('a', '', entry.text || 'Untitled');
    a.dataset.goto = entry.id;
    a.href = `#${entry.id}`;
    li.append(a);
    list.append(li);
  }
  nav.append(list);
  return nav;
}
