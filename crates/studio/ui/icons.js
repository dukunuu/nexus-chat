// Local, deliberately small stroke icon set. No fonts, remote assets or markup parsing.
const paths = {
  'node-frame':'M5 2v20M19 2v20M2 5h20M2 19h20',
  'node-text':'M4 4h16M12 4v16M8 20h8',
  'node-image':'M3 3h18v18H3Zm0 14 6-6 5 5 3-3 4 4M16 7h.01',
  'node-shape':'M4 4h16v16H4Z',
  'node-table':'M3 3h18v18H3ZM3 9h18M9 3v18M15 3v18M3 15h18',
  'node-code':'m8 6-6 6 6 6m8-12 6 6-6 6M14 3l-4 18',
  'node-chart':'M4 2v20h18M8 17V9m5 8V5m5 12v-6',
  eye:'M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12Zm7 0a3 3 0 1 0 6 0 3 3 0 1 0-6 0',
  'eye-off':'m3 3 18 18M9 5c7-2 13 7 13 7l-4 4M6 6l-4 6s4 7 10 7l4-1',
  lock:'M5 10h14v11H5Zm3 0V6a4 4 0 0 1 8 0v4M12 14v3',
  unlock:'M5 10h14v11H5Zm3 0V6a4 4 0 0 1 8 0M12 14v3',
  'object-left':'M3 3v18M7 6h14v5H7Zm0 9h9v5H7Z',
  'object-h-center':'M12 2v20M3 6h18v5H3Zm4 9h10v5H7Z',
  'object-right':'M21 3v18M3 6h14v5H3Zm5 9h9v5H8Z',
  'object-top':'M3 3h18M5 7h5v14H5Zm9 0h5v9h-5Z',
  'object-v-center':'M2 12h20M5 3h5v18H5Zm9 4h5v10h-5Z',
  'object-bottom':'M3 21h18M5 3h5v14H5Zm9 5h5v9h-5Z',
  'object-distribute-h':'M2 3v18M22 3v18M6 5h4v14H6Zm8 0h4v14h-4Z',
  'object-distribute-v':'M3 2h18M3 22h18M5 6h14v4H5Zm0 8h14v4H5Z',
  more:'M5 12h.01M12 12h.01M19 12h.01', minus:'M4 12h16',
  rotation:'M4 10a8 8 0 1 1 2 8M4 4v6h6', radius:'M5 20V12a7 7 0 0 1 7-7h8',
  opacity:'M12 3a9 9 0 1 0 0 18V3Zm0 0a9 9 0 0 1 0 18',
  absolute:'M3 8V3h5m8 0h5v5m0 8v5h-5M8 21H3v-5M8 8h8v8H8Z',
  'layout-none':'M4 4h6v6H4Zm10 10h6v6h-6Z',
  'layout-column':'M12 3v18m-5-5 5 5 5-5M4 4h3M4 9h3M4 14h3',
  'layout-row':'M3 12h18m-5-5 5 5-5 5M4 4v3M9 4v3M14 4v3',
  'layout-wrap':'M3 5h13a5 5 0 0 1 0 10H6m4-4-4 4 4 4M3 2v6',
  gap:'M5 4v16M19 4v16M9 12h6m-4-3-3 3 3 3m2-6 3 3-3 3',
  'padding-h':'M3 3v18M21 3v18M8 6h8v12H8Z',
  'padding-v':'M3 3h18M3 21h18M6 8h12v8H6Z',
  'padding-sides':'M3 3h18v18H3ZM8 8h8v8H8Z',
  clip:'M7 2v15h15M2 7h15v15', stretch:'M3 3v18M21 3v18M8 7l-4 5 4 5m8-10 4 5-4 5M4 12h16',
  'line-height':'M8 4h13M8 20h13M14 4v16M3 4v16m-2-3 2 3 2-3M1 7l2-3 2 3',
  'letter-spacing':'m8 15 4-12 4 12M9 11h6M3 21h18m-3-3 3 3-3 3M6 18l-3 3 3 3',
  'case-upper':'m2 19 5-14 5 14M4 14h6m3 5 4-10 5 10m-7-4h5',
  'case-lower':'M10 10v9M10 12c-8-6-10 11 0 5M15 3v16M15 12c9-8 9 12 0 5',
  'case-capitalize':'m3 19 5-14 5 14M5 14h6M17 10h5m-3-4v11c0 2 2 2 3 2',
  undo: 'M6 5 2 9l4 4M2 9h8a5 5 0 0 1 0 10',
  redo: 'm18 5 4 4-4 4m4-4h-8a5 5 0 0 0 0 10',
  play: 'm8 4 12 8-12 8Z', export: 'M12 15V2m-5 5 5-5 5 5M4 14v7h16v-7',
  comment: 'M21 11a9 9 0 0 1-9 9H3l2-5a9 9 0 1 1 16-4Z',
  cursor: 'm5 2 15 11-8 1-4 7Z', plus: 'M12 4v16M4 12h16',
  trash: 'M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7',
  duplicate: 'M9 9h12v12H9ZM15 9V3H3v12h6',
  layers: 'm12 2 10 5-10 5L2 7Zm-10 10 10 5 10-5M2 17l10 5 10-5',
  'align-left': 'M3 5h18M3 10h12M3 15h18M3 20h12',
  'align-center': 'M3 5h18M6 10h12M3 15h18M6 20h12',
  'align-right': 'M3 5h18M9 10h12M3 15h18M9 20h12',
  bold: 'M6 3h8a4 4 0 0 1 0 8H6Zm0 8h9a5 5 0 0 1 0 10H6Z',
  italic: 'M10 3h10M4 21h10M15 3 9 21', chevron: 'm8 4 8 8-8 8',
  send: 'm12 20 0-16m-6 6 6-6 6 6', check: 'm4 12 5 5L20 5', x: 'm5 5 14 14M5 19 19 5',
  sparkle: 'm12 2 3 7 7 3-7 3-3 7-3-7-7-3 7-3Z',
  panel: 'M3 3h18v18H3Zm6 0v18', forward: 'm7 14 5-5 5 5M5 4h14', back: 'm7 10 5 5 5-5M5 20h14',
};
export function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ viewBox: '0 0 24 24', width: 16, height: 16, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': true })) svg.setAttribute(key, value);
  const path = document.createElementNS(svg.namespaceURI, 'path'); path.setAttribute('d', paths[name] || paths.panel); svg.append(path); return svg;
}
export function button(name, label, action, className = '') {
  const node = document.createElement('button'); node.className = className; node.title = label; node.setAttribute('aria-label', label); node.append(icon(name)); node.onclick = action; return node;
}
export function actionMenu(label, actions) {
  const menu = document.createElement('details'); menu.className = 'action-menu';
  const summary = document.createElement('summary'); summary.title = label; summary.setAttribute('aria-label', label); summary.append(icon('more'));
  const list = document.createElement('div'); list.className = 'action-list';
  for (const [title, action] of actions) {
    const control = document.createElement('button'); control.textContent = title; control.onclick = () => { menu.open = false; action(); }; list.append(control);
  }
  menu.append(summary, list);
  menu.addEventListener('focusout', event => { if (!menu.contains(event.relatedTarget)) menu.open = false; });
  return menu;
}
export function hydrateIcons() {
  document.addEventListener('pointerdown', event => document.querySelectorAll('.action-menu[open]').forEach(menu => { if (!menu.contains(event.target)) menu.open = false; }));
  document.addEventListener('keydown', event => { const menu = event.target.closest('.action-menu[open]'); if (event.key === 'Escape' && menu) { event.preventDefault(); event.stopImmediatePropagation(); menu.open = false; menu.querySelector('summary').focus(); } });
  document.querySelectorAll('[data-icon]').forEach(node => node.prepend(icon(node.dataset.icon)));
}
