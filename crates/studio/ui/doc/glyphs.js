// Stroke icons for the doc editor, in the same style as ../icons.js (which it falls back to).
import { icon } from '../icons.js';

const paths = {
  strike: 'M4 12h16M8 7c0-1.7 1.6-3 4-3s4 1.3 4 3M16 17c0 1.7-1.6 3-4 3s-4-1.3-4-3',
  code: 'm8 6-6 6 6 6m8-12 6 6-6 6',
  link: 'M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1',
  info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM12 11v6M12 7.5h.01',
  warn: 'M12 3 2 20h20L12 3ZM12 10v5M12 17.5h.01',
  success: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM8 12.5l3 3 5-6',
  note: 'M6 3h9l4 4v14H6ZM15 3v4h4M9 12h6M9 16h6',
  pages: 'M7 3h8l4 4v14H7ZM15 3v4h4M10 12h6M10 16h6',
  design: 'M12 3a9 9 0 1 0 0 18c1.5 0 2-1 1.5-2s0-2 1.5-2h2a3 3 0 0 0 3-3c0-5-4-11-8-11ZM7.5 11h.01M10 7.5h.01M15 8h.01',
  outline: 'M4 6h16M4 12h10M4 18h13',
  down: 'm6 9 6 6 6-6',
  row: 'M3 8h18M3 16h18M3 4h18v16H3Z',
  paragraph: 'M13 4v16M17 4v16M13 4H9a4 4 0 0 0 0 8h4',
  list: 'M9 6h12M9 12h12M9 18h12M4 6h.01M4 12h.01M4 18h.01',
  quote: 'M5 17c-1-4 0-9 4-11M13 17c-1-4 0-9 4-11',
  check: 'm5 12.5 4.5 4.5L19 7.5',
  plus: 'M12 5v14M5 12h14',
  table: 'M3 5h18v14H3ZM3 10h18M9 10v9',
  'arrow-up': 'M12 19V5m-6 6 6-6 6 6',
  'arrow-down': 'M12 5v14m-6-6 6 6 6-6',
  'arrow-left': 'M19 12H5m6-6-6 6 6 6',
  'arrow-right': 'M5 12h14m-6-6 6 6-6 6',
  forward: 'M12 19V5m-6 6 6-6 6 6',
  back: 'M12 5v14m-6-6 6 6 6-6',
};
export function glyph(name) {
  if (!paths[name]) return icon(name);
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ viewBox: '0 0 24 24', width: 16, height: 16, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': true })) svg.setAttribute(key, value);
  const path = document.createElementNS(svg.namespaceURI, 'path');
  path.setAttribute('d', paths[name]);
  svg.append(path);
  return svg;
}
