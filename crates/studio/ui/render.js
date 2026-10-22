import { markdownTokens, inlineTokens, resolveFont, slideSize } from './helpers.js';

export function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
export function fontStack(name = '') {
  const clean = name.replace(/["'\\;]/g, '');
  if (/mono|code|consolas|menlo/i.test(clean)) return `"${clean}", ui-monospace, SFMono-Regular, Menlo, monospace`;
  if (/serif|georgia|times|baskerville|garamond|new york/i.test(clean) && !/sans/i.test(clean)) return `"${clean}", ui-serif, "New York", Georgia, serif`;
  return `"${clean}", system-ui, -apple-system, "Segoe UI", sans-serif`;
}
export function availableFont(stack) {
  const context = document.createElement('canvas').getContext('2d');
  const sample = 'mmmmmmmmmmWWWWWWiiiiii0123456789';
  const generic = { 'system-ui':'System UI', '-apple-system':'System UI', 'ui-serif':'System serif', 'ui-monospace':'System mono', 'sans-serif':'Sans serif', serif:'Serif', monospace:'Monospace' };
  for (const part of stack.match(/"[^"]+"|'[^']+'|[^,]+/g)) {
    const family = part.trim().replace(/^["']|["']$/g, '');
    if (generic[family]) return generic[family];
    const available = ['monospace', 'serif', 'sans-serif'].some(fallback => {
      context.font = `72px ${fallback}`; const width = context.measureText(sample).width;
      context.font = `72px "${family}", ${fallback}`; return Math.abs(context.measureText(sample).width - width) > .01;
    });
    if (available) return family;
  }
  return 'System UI';
}
export const color = (value, theme) => theme.colors[value] || (/^#[0-9a-f]{3,8}$/i.test(value || '') ? value : theme.colors.fg);
export function markdown(text) {
  const fragment = document.createDocumentFragment();
  let list;
  for (const line of markdownTokens(text)) {
    const isList = line.kind === 'ul' || line.kind === 'ol';
    if (!isList || list?.tagName.toLowerCase() !== line.kind) list = null;
    if (isList && !list) { list = element(line.kind); fragment.append(list); }
    const row = element(isList ? 'li' : line.kind);
    for (const token of inlineTokens(line.text)) {
      // Links are readable but intentionally inert: the page cannot navigate
      // to a remote site or grant that site access to the IPC channel.
      const node = element(token.kind === 'text' || token.kind === 'link' ? 'span' : token.kind, token.kind === 'link' ? 'deck-link' : '', token.text);
      if (token.kind === 'link') node.title = token.href;
      row.append(node);
    }
    (isList ? list : fragment).append(row);
    if (!line.text) row.append(element('br'));
  }
  return fragment;
}
function svgNode(tag, attributes = {}) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, String(value));
  return node;
}
function renderChart(block, theme) {
  const svg = svgNode('svg', { viewBox: '0 0 320 180', preserveAspectRatio: 'none' });
  const values = block.series.flatMap(s => s.values);
  const max = values.reduce((max, value) => Math.max(max, Math.abs(value)), 1);
  const count = Math.max(1, block.labels.length);
  const palette = [theme.colors.accent, theme.colors.fg, theme.colors.muted];
  if (block.chart === 'pie') {
    const data = block.series[0]?.values.map(v => Math.max(0, v)) || [];
    const total = data.reduce((a, b) => a + b, 0) || 1;
    let angle = -Math.PI / 2;
    data.forEach((v, i) => {
      const end = angle + v / total * Math.PI * 2;
      if (v / total >= 0.999999) svg.append(svgNode('circle', { cx: 160, cy: 80, r: 65, fill: palette[i % palette.length] }));
      else if (v > 0) svg.append(svgNode('path', { d: `M160 80 L${160 + 65 * Math.cos(angle)} ${80 + 65 * Math.sin(angle)} A65 65 0 ${end - angle > Math.PI ? 1 : 0} 1 ${160 + 65 * Math.cos(end)} ${80 + 65 * Math.sin(end)} Z`, fill: palette[i % palette.length] }));
      angle = end;
    });
  } else {
    block.series.forEach((series, s) => {
      const points = [];
      series.values.forEach((v, i) => {
        const x = 10 + (i + 0.5) * 300 / count;
        const y = 140 - v / max * 120;
        points.push(`${x},${y}`);
        if (block.chart === 'bar') svg.append(svgNode('rect', { x: 10 + i * 300 / count + s * 260 / count / block.series.length, y: Math.min(140, y), width: 250 / count / block.series.length, height: Math.abs(140 - y), fill: palette[s % palette.length] }));
      });
      if (block.chart === 'line') svg.append(svgNode('polyline', { points: points.join(' '), fill: 'none', stroke: palette[s % palette.length], 'stroke-width': 3 }));
    });
  }
  block.labels.forEach((label, i) => {
    const node = svgNode('text', { x: 10 + (i + 0.5) * 300 / count, y: 170, 'text-anchor': 'middle', fill: theme.colors.fg, 'font-size': 10 });
    node.textContent = label;
    svg.append(node);
  });
  return svg;
}

// -- units -------------------------------------------------------------------------
// Slide lengths are px on a slide `size.w` wide. They are written in `cqw`, a
// percentage of the slide's width, so one slide scales to any display size.
const unit = (px, theme) => `${(px / slideSize(theme).w * 100).toFixed(4)}cqw`;
const MAIN = { start:'flex-start', center:'center', end:'flex-end', space_between:'space-between' };
const CROSS = { start:'flex-start', center:'center', end:'flex-end', stretch:'stretch' };

/** The core renderer's mapping of a deck's auto-layout onto flexbox. See `deck/layout.rs`. */
function layoutStyle(node, layout = {}, theme) {
  const mode = layout.mode || 'none', style = node.style;
  style.display = 'flex';
  style.flexDirection = mode === 'column' ? 'column' : 'row';
  style.flexWrap = layout.wrap && mode !== 'none' ? 'wrap' : 'nowrap';
  const gap = unit(layout.gap || 0, theme), cross = unit(layout.gap_cross ?? layout.gap ?? 0, theme);
  style.gap = mode === 'column' ? `${gap} ${cross}` : `${cross} ${gap}`;
  const [t, r, b, l] = layout.padding || [0, 0, 0, 0];
  style.padding = `${unit(t, theme)} ${unit(r, theme)} ${unit(b, theme)} ${unit(l, theme)}`;
  style.justifyContent = MAIN[layout.main] || 'flex-start';
  style.alignItems = CROSS[layout.cross] || 'flex-start';
  if (layout.wrap) style.alignContent = 'flex-start';
}
function sizeStyle(el, node, theme, parent) {
  const s = node.sizing || {}, w = s.w ?? 'hug', h = s.h ?? 'hug', style = el.style;
  const length = value => typeof value === 'number' ? unit(value, theme) : 'auto';
  style.width = length(w); style.height = length(h);
  if (s.min_w != null) style.minWidth = unit(s.min_w, theme);
  if (s.max_w != null) style.maxWidth = unit(s.max_w, theme);
  if (s.min_h != null) style.minHeight = unit(s.min_h, theme);
  if (s.max_h != null) style.maxHeight = unit(s.max_h, theme);
  const absolute = parent.mode === 'none' || typeof node.position === 'object';
  if (absolute) {
    const at = typeof node.position === 'object' ? node.position : { x: 0, y: 0 };
    Object.assign(style, { position: 'absolute', left: unit(at.x, theme), top: unit(at.y, theme) });
    if (w === 'fill') style.right = '0';
    if (h === 'fill') style.bottom = '0';
  } else {
    style.position = 'relative';
    const row = parent.mode === 'row', main = row ? w : h, cross = row ? h : w;
    style.flex = typeof main === 'number' ? '0 0 auto' : main === 'fill' ? '1 1 0px' : '0 1 auto';
    if (cross === 'fill') style.alignSelf = 'stretch';
  }
  if (node.opacity !== undefined && node.opacity !== 1) style.opacity = String(node.opacity);
  if (node.rotation) style.transform = `rotate(${node.rotation}deg)`;
  if (node.hidden) style.display = 'none';
}
/** Charts, shapes and images have a natural size and no automatic minimum. */
function naturalSize(el, node, theme, [width, height], ratio) {
  const s = node.sizing || {}, hugW = (s.w ?? 'hug') === 'hug', hugH = (s.h ?? 'hug') === 'hug';
  el.style.minWidth = el.style.minWidth || '0'; el.style.minHeight = el.style.minHeight || '0';
  if (hugW && hugH && node.kind !== 'image') { el.style.width = unit(width, theme); el.style.height = unit(height, theme); }
  else if (ratio && (hugW || hugH)) el.style.aspectRatio = `${width} / ${height}`;
}
const stackFor = (theme, family) => fontStack(theme.fonts?.[family] || family);

/** A style tag that makes the deck's imported font files usable by name. */
let fontCss = '';
export function ensureDeckFonts(theme) {
  const rules = (theme.font_files || []).map(file => {
    const family = String(file.family).replace(/["'\\;{}<>]/g, '');
    const src = `/${String(file.file).split('/').map(encodeURIComponent).join('/')}`;
    return `@font-face{font-family:"${family}";src:url("${src}");font-weight:${Number(file.weight) || 400};font-style:${file.italic ? 'italic' : 'normal'};}`;
  }).join('\n');
  if (rules === fontCss) return;
  fontCss = rules;
  let tag = document.getElementById('deck-fonts');
  if (!tag) { tag = element('style'); tag.id = 'deck-fonts'; document.head.append(tag); }
  tag.textContent = rules;
}
function textStyle(el, node, theme) {
  const font = resolveFont(theme, node.style, node.font), style = el.style;
  style.fontFamily = stackFor(theme, font.family);
  style.setProperty('--heading-font', stackFor(theme, font.family === 'body' ? 'heading' : font.family));
  style.fontSize = unit(font.size, theme); style.fontWeight = String(font.weight);
  style.fontStyle = font.italic ? 'italic' : 'normal'; style.lineHeight = String(font.line_height);
  style.letterSpacing = `${font.letter_spacing}em`; style.textAlign = font.align;
  style.textTransform = font.transform === 'none' ? '' : font.transform;
  style.color = color(font.color, theme);
  el.classList.toggle('tight', font.line_height < 1.2);
  if (font.valign !== 'top') { style.flexDirection = 'column'; style.justifyContent = font.valign === 'middle' ? 'center' : 'flex-end'; style.display = 'flex'; }
  return font;
}
export function renderNode(node, theme, images = {}, parent = { mode: 'none' }) {
  const el = element(node.kind === 'image' ? 'img' : 'div', `block block-${node.kind}`);
  el.dataset.id = node.id;
  sizeStyle(el, node, theme, parent);
  if (node.kind === 'frame') {
    layoutStyle(el, node.layout, theme);
    if (node.hidden) el.style.display = 'none';
    if (node.fill) el.style.backgroundColor = color(node.fill, theme);
    if (node.stroke?.width) el.style.boxShadow = `inset 0 0 0 ${unit(node.stroke.width, theme)} ${color(node.stroke.color, theme)}`;
    if (node.radius) el.style.borderRadius = unit(node.radius, theme);
    if (node.clip) el.style.overflow = 'hidden';
    for (const child of node.children || []) el.append(renderNode(child, theme, images, node.layout || { mode: 'none' }));
  } else if (node.kind === 'text') {
    textStyle(el, node, theme); if (node.hidden) el.style.display = 'none';
    el.append(markdown(node.text));
  } else if (node.kind === 'image') {
    // Only the backend's origin can supply image bytes. Remote images are
    // resolved through the core fetcher and exposed by content hash.
    el.src = node.src.startsWith('https://') ? `/asset/${images[node.src] || 'missing'}` : `/assets/${node.src.replace(/^assets\//, '')}`;
    el.alt = 'Slide image'; el.draggable = false;
    el.style.objectFit = node.fit || 'contain';
    naturalSize(el, node, theme, [640, 360], true);
  } else if (node.kind === 'shape') {
    const fill = node.fill ? color(node.fill, theme) : 'transparent', stroke = node.stroke?.width ? color(node.stroke.color, theme) : null;
    const width = node.stroke?.width || 0;
    if (node.shape === 'line' || node.shape === 'arrow') {
      const lineColor = stroke || color(node.fill || 'accent', theme), line = width || 4;
      const bar = element('span', 'shape-line'); Object.assign(bar.style, { background: lineColor, height: unit(line, theme) });
      el.append(bar);
      if (node.shape === 'arrow') { const head = element('span', 'shape-head'); head.style.borderLeftColor = lineColor; el.append(head); }
      naturalSize(el, node, theme, [240, 40], false);
    } else {
      el.style.backgroundColor = fill;
      if (width) el.style.boxShadow = `inset 0 0 0 ${unit(width, theme)} ${stroke}`;
      el.style.borderRadius = node.shape === 'ellipse' ? '50%' : unit(node.radius || 0, theme);
      naturalSize(el, node, theme, [160, 160], false);
    }
  } else if (node.kind === 'table') {
    textStyle(el, node, theme);
    const table = element('table');
    for (const row of node.rows) { const tr = element('tr'); for (const cell of row) tr.append(element('td', '', cell)); table.append(tr); }
    el.append(table);
  } else if (node.kind === 'code') {
    const font = textStyle(el, node, theme);
    el.style.fontFamily = `"${String(theme.fonts?.[font.family] || font.family).replace(/["'\\;]/g, '')}", ui-monospace, SFMono-Regular, Menlo, monospace`;
    el.textContent = node.source;
  } else if (node.kind === 'chart') {
    el.append(renderChart(node, theme));
    naturalSize(el, node, theme, [640, 360], true);
  }
  if (node.hidden) el.style.display = 'none';
  return el;
}
/** Where a node sits among its siblings is up to its parent; this only re-applies its own box. */
export function renderSlide(container, slide, theme, images) {
  ensureDeckFonts(theme);
  container.replaceChildren();
  container.style.aspectRatio = `${slideSize(theme).w} / ${slideSize(theme).h}`;
  const root = element('div', 'slide-root');
  Object.assign(root.style, { position: 'absolute', inset: '0' });
  root.style.backgroundColor = container.style.backgroundColor = color(slide?.fill || 'bg', theme);
  root.style.color = theme.colors.fg;
  root.style.fontFamily = stackFor(theme, 'body');
  root.style.setProperty('--mono-font', /mono|code|consolas|menlo/i.test(theme.fonts?.mono || '') ? fontStack(theme.fonts.mono) : '"Menlo", SFMono-Regular, monospace');
  layoutStyle(root, slide?.layout, theme);
  container.append(root);
  if (slide) for (const node of slide.children) root.append(renderNode(node, theme, images, slide.layout || { mode: 'none' }));
}
