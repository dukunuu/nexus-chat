import { markdownTokens, inlineTokens } from './helpers.js';

export function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
export const color = (value, theme) => theme.colors[value] || (/^#[0-9a-f]{3,8}$/i.test(value || '') ? value : theme.colors.fg);
function markdown(text) {
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
export function renderBlock(block, theme, images = {}) {
  const node = element('div', `block block-${block.kind}`);
  node.dataset.id = block.id;
  positionBlock(node, block);
  node.style.textAlign = block.style.align || 'left';
  node.style.color = color(block.style.color || 'fg', theme);
  node.style.fontSize = `${({ s: 2, m: 2.6, l: 3.4, xl: 4.8, xxl: 6.4 })[block.style.size || 'm']}cqw`;
  if (block.kind === 'text') node.append(markdown(block.text));
  else if (block.kind === 'image') {
    const image = element('img');
    // Only the backend's origin can supply image bytes. Remote images are
    // resolved through the core fetcher and exposed by content hash.
    image.src = block.src.startsWith('https://') ? `/asset/${images[block.src] || 'missing'}` : `/assets/${block.src.replace(/^assets\//, '')}`;
    image.alt = 'Slide image'; image.draggable = false;
    image.style.objectFit = block.fit || 'contain';
    node.append(image);
  } else if (block.kind === 'shape') {
    const fill = color(block.fill, theme), stroke = color(block.stroke, theme);
    if (block.shape === 'line' || block.shape === 'arrow') {
      const svg = svgNode('svg', { viewBox: '0 0 100 100', preserveAspectRatio: 'none' });
      svg.append(svgNode('path', { d: block.shape === 'arrow' ? 'M2 50 H97 M77 25 L97 50 L77 75' : 'M2 50 H98', stroke, fill: 'none', 'stroke-width': 2 }));
      node.append(svg);
    } else {
      node.style.backgroundColor = fill; node.style.border = `2px solid ${stroke}`;
      if (block.shape === 'ellipse') node.style.borderRadius = '50%';
    }
  } else if (block.kind === 'table') {
    const table = element('table');
    for (const row of block.rows) { const tr = element('tr'); for (const cell of row) tr.append(element('td', '', cell)); table.append(tr); }
    node.append(table);
  } else if (block.kind === 'code') {
    const pre = element('pre'); pre.style.fontFamily = theme.fonts.mono;
    pre.append(element('code', '', block.source)); node.append(pre);
  } else if (block.kind === 'chart') node.append(renderChart(block, theme));
  return node;
}
export function positionBlock(node, rect) {
  Object.assign(node.style, { left: `${rect.x / 32 * 100}%`, top: `${rect.y / 18 * 100}%`, width: `${rect.w / 32 * 100}%`, height: `${rect.h / 18 * 100}%` });
}
export function renderSlide(container, slide, theme, images) {
  container.replaceChildren();
  container.style.backgroundColor = theme.colors.bg;
  container.style.color = theme.colors.fg;
  container.style.fontFamily = theme.fonts.body;
  container.style.setProperty('--heading-font', theme.fonts.heading);
  if (slide) for (const block of slide.blocks) container.append(renderBlock(block, theme, images));
}
