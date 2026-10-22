import { allNodes, childrenOf, parentOf, isAbsolute, nodeLabel, resolveFont, slideSize } from './helpers.js';
import { element, color } from './render.js';
import { fields, compact } from './fields.js';
import { fontController } from './fonts.js';
import { button, icon } from './icons.js';

const TEXTY = ['text', 'code', 'table'];

export function panelController(api) {
  const panel = document.querySelector('#properties'), toolbar = document.querySelector('#floating-toolbar');
  const nodes = () => { const slide = api.slide(); return slide ? allNodes(slide).filter(n => api.state.selected.has(n.id)) : []; };
  const number = value => { const n = Number(value); return Number.isFinite(n) ? n : null; };
  const field = fields(api), fonts = fontController(api, field, nodes);
  const collapsed = new Set(), perSide = new Set(), limits = new Set(), sectionStates = new Map();
  /** Merge-patch every selected node. */
  const patch = fields => api.updateMany(api.slide().id, nodes().map(node => ({ id: node.id, fields: structuredClone(fields) })));
  const font = fields => patch({ font: fields });
  function mark(marker) {
    api.updateMany(api.slide().id, nodes().filter(n => n.kind === 'text').map(n => ({ id: n.id, fields: { text: n.text.startsWith(marker) && n.text.endsWith(marker) ? n.text.slice(marker.length, -marker.length) : `${marker}${n.text}${marker}` } })));
  }
  function reorder(nodeId, delta) {
    const at = parentOf(api.slide(), nodeId); if (!at) return;
    const to = Math.max(0, Math.min(childrenOf(at.parent).length - 1, at.index + delta));
    if (to !== at.index) api.apply(() => [{ op: 'move_node', id: nodeId, parent: at.parent.id, index: to }]);
  }
  /** The color roles a selection can take, applied where its kind keeps color. */
  function colors(container, labels = false) {
    const selected = nodes();
    const current = node => TEXTY.includes(node.kind) ? resolveFont(api.state.deck.theme, node.style, node.font).color : node.fill || 'fg';
    const set = value => api.apply(() => selected.map(node => ({ op: 'update_node', id: node.id, fields: TEXTY.includes(node.kind) ? { font: { color: value } } : { fill: value }, base_hash: api.state.hashes[node.id] })));
    for (const [role, label] of [['accent','Accent'], ['fg','Text'], ['muted','Muted']]) {
      const node = button('check', `${label} color`, () => set(role), 'color-option'); node.replaceChildren(element('span', 'color-chip'));
      node.setAttribute('aria-pressed', String(selected.length > 0 && selected.every(n => current(n) === role)));
      if (labels) node.append(element('span', 'color-name', label));
      node.style.setProperty('--swatch', api.state.deck.theme.colors[role]); container.append(node);
    }
    const custom = element('label', 'custom-color'); custom.title = 'Custom color';
    custom.classList.toggle('selected', selected.length > 0 && selected.every(n => current(n).startsWith('#') && current(n) === current(selected[0])));
    const picker = element('input', 'color-picker'); picker.type = 'color'; picker.title = 'Custom color'; picker.setAttribute('aria-label', 'Custom color'); picker.value = color(selected[0] ? current(selected[0]) : 'fg', api.state.deck.theme); picker.onchange = () => set(picker.value); custom.append(picker);
    if (labels) custom.append(element('span', 'color-name', 'Custom'));
    container.append(custom);
  }
  function renderToolbar() {
    toolbar.replaceChildren();
    const selected = nodes(), node = selected[0];
    toolbar.hidden = !node || api.state.properties || api.state.mode === 'comment' || Boolean(api.state.preview);
    if (toolbar.hidden) return;
    if (selected.some(n => TEXTY.includes(n.kind))) {
      const size = field('Text size', resolveFont(api.state.deck.theme, node.style, node.font).size, value => { const n = number(value); if (n > 0) font({ size: n }); }, null, 'number').lastChild;
      size.setAttribute('aria-label', 'Text size in px'); size.min = 1; toolbar.append(size);
    }
    if (selected.some(n => n.kind === 'text')) toolbar.append(button('bold', 'Bold', () => mark('**')), button('italic', 'Italic', () => mark('*')));
    if (selected.some(n => TEXTY.includes(n.kind))) for (const align of ['left', 'center', 'right']) toolbar.append(button(`align-${align}`, `Align ${align}`, () => font({ align })));
    toolbar.append(element('span', 'toolbar-divider')); colors(toolbar); toolbar.append(element('span', 'toolbar-divider'));
    const rect = api.rectOf(node.id), size = slideSize(api.state.deck.theme);
    toolbar.append(button('forward', 'Bring forward', () => reorder(node.id, 1)), button('back', 'Send back', () => reorder(node.id, -1)), button('duplicate', 'Duplicate', api.duplicate), button('trash', 'Delete', api.remove), button('comment', 'Comment on selection', () => api.commentAt({ slide: api.slide().id, block: node.id, x: Math.max(0, Math.min(size.w - 1, Math.round(rect?.x || 0))), y: Math.max(0, Math.min(size.h - 1, Math.round(rect?.y || 0))) })));
    // Keep the toolbar above the selection, within the slide's horizontal bounds.
    const wide = Math.max(1, document.querySelector('#canvas').clientWidth);
    toolbar.style.left = `${Math.max(0, Math.min(100 - toolbar.scrollWidth / wide * 100, (rect?.x || 0) / size.w * 100))}%`;
    toolbar.style.top = `${(rect?.y || 0) / size.h * 100 - 3}%`;
  }
  // -- layers ----------------------------------------------------------------------
  function layerRows(slide, list, depth) {
    [...list].reverse().forEach(node => {
      const row = element('div', `layer${api.state.selected.has(node.id) ? ' active' : ''}${node.hidden ? ' hidden-layer' : ''}`);
      row.dataset.layer = node.id; row.tabIndex = 0; row.setAttribute('role', 'treeitem'); row.setAttribute('aria-selected', String(api.state.selected.has(node.id)));
      row.style.paddingLeft = `${8 + depth * 14}px`;
      if (node.kind === 'frame') {
        const toggle = button('chevron', collapsed.has(node.id) ? 'Expand frame' : 'Collapse frame', event => { event.stopPropagation(); collapsed.has(node.id) ? collapsed.delete(node.id) : collapsed.add(node.id); render(); }, 'layer-toggle');
        toggle.classList.toggle('expanded', !collapsed.has(node.id)); row.append(toggle); row.setAttribute('aria-expanded', String(!collapsed.has(node.id)));
      }
      const name = element('span', 'layer-name', nodeLabel(node)); row.append(icon(`node-${node.kind}`), name);
      const select = event => { if (event.shiftKey) { if (api.state.selected.has(node.id)) api.state.selected.delete(node.id); else api.state.selected.add(node.id); } else api.state.selected = new Set([node.id]); api.selectionChanged(); };
      row.onclick = select; row.onkeydown = event => { if (event.key === 'Enter') select(event); };
      name.ondblclick = event => {
        event.stopPropagation(); row.draggable = false;
        const input = element('input', 'layer-rename'); input.value = node.name || nodeLabel(node); name.replaceWith(input); input.focus(); input.select();
        input.onclick = e => e.stopPropagation();
        let cancel = false;
        input.onkeydown = e => { e.stopPropagation(); if (['Enter','Escape'].includes(e.key)) { cancel = e.key === 'Escape'; input.blur(); } };
        input.onblur = () => { if (!cancel && input.value.trim()) api.updateMany(slide.id, [{ id:node.id, fields:{ name:input.value.trim() } }]); else render(); };
      };
      for (const [key, glyph, title] of [['hidden', node.hidden ? 'eye-off' : 'eye', node.hidden ? 'Show layer' : 'Hide layer'], ['locked', node.locked ? 'lock' : 'unlock', node.locked ? 'Unlock layer' : 'Lock layer']]) {
        const control = button(glyph, title, event => { event.stopPropagation(); api.updateMany(slide.id, [{ id:node.id, fields:{ [key]:!node[key] } }]); }, 'layer-control'); control.setAttribute('aria-pressed', String(Boolean(node[key]))); row.append(control);
      }
      row.draggable = true;
      row.ondragstart = event => event.dataTransfer.setData('text/plain', node.id);
      row.ondragover = event => { event.preventDefault(); event.stopPropagation(); const box = row.getBoundingClientRect(); row.dataset.drop = node.kind === 'frame' && event.clientY > box.top + box.height * .25 && event.clientY < box.bottom - box.height * .25 ? 'inside' : event.clientY < box.top + box.height / 2 ? 'above' : 'below'; };
      row.ondragleave = () => delete row.dataset.drop;
      row.ondrop = event => {
        event.preventDefault(); event.stopPropagation();
        const dragged = event.dataTransfer.getData('text/plain'), from = parentOf(slide, dragged), to = parentOf(slide, node.id), drop = row.dataset.drop; delete row.dataset.drop;
        if (!from || !to || dragged === node.id || allNodes({ children:[childrenOf(from.parent)[from.index]] }).some(n => n.id === node.id)) return;
        if (drop === 'inside') api.apply(() => [{ op:'move_node', id:dragged, parent:node.id }]);
        else {
          const index = to.index + (drop === 'above' ? 1 : 0);
          api.apply(() => [{ op:'move_node', id:dragged, parent:to.parent.id, index:index - (from.parent === to.parent && from.index < index ? 1 : 0) }]);
        }
      };
      listContainer.append(row);
      if (!collapsed.has(node.id)) layerRows(slide, childrenOf(node), depth + 1);
    });
  }
  // -- sections --------------------------------------------------------------------
  const listContainer = document.querySelector('#layers');
  const numeric = (label, value, change, glyph = label) => compact(field(label, value, change, null, 'number'), glyph);
  function pair(...controls) { const row = element('div', 'field-pair'); row.append(...controls); return row; }
  function toggle(glyph, label, value, change) {
    const control = button(glyph, label, () => change(!value), 'icon-toggle'); control.setAttribute('aria-pressed', String(value)); return control;
  }
  function section(title, actions = [], folded = false) {
    const group = element('section', 'design-section'), header = element('div', 'section-heading'), body = element('div', 'section-body');
    const heading = element('button', 'section-toggle', title), key = `${title}:${Boolean(nodes().length)}`;
    body.hidden = sectionStates.get(key) ?? folded;
    heading.setAttribute('aria-expanded', String(!body.hidden));
    heading.onclick = () => { body.hidden = !body.hidden; heading.setAttribute('aria-expanded', String(!body.hidden)); sectionStates.set(key, body.hidden); };
    header.append(heading, ...actions); group.append(header, body); panel.append(group); return body;
  }
  function layoutFields(container, layout, target, isSlide) {
    const write = fields => isSlide
      ? api.apply(() => [{ op:'update_slide', id:target.id, fields:{ layout:fields } }])
      : api.updateMany(api.slide().id, [{ id:target.id, fields:{ layout:fields } }]);
    const modes = element('div', 'layout-modes'); modes.setAttribute('aria-label', 'Layout direction');
    for (const [mode, title] of [['none','None'],['column','Column'],['row','Row'],['wrap','Wrap']]) {
      const control = button(`layout-${mode}`, title, () => api.apply(() => [{ op:'set_layout', target:target.id, layout:{ ...layout, mode:mode === 'wrap' ? 'row' : mode, wrap:mode === 'wrap' } }]));
      control.setAttribute('aria-pressed', String((layout.wrap ? 'wrap' : layout.mode || 'none') === mode)); modes.append(control);
    }
    const tools = element('div', 'layout-tools'); tools.append(modes);
    if (!isSlide) tools.append(toggle('clip', 'Clip content', Boolean(target.clip), clip => api.updateMany(api.slide().id, [{ id:target.id, fields:{ clip } }])));
    container.append(tools);
    if ((layout.mode || 'none') === 'none') return;
    const cluster = element('div', 'layout-cluster'), grid = element('div', 'alignment-grid'); grid.setAttribute('aria-label', 'Auto-layout alignment');
    for (let y = 0; y < 3; y++) for (let x = 0; x < 3; x++) {
      const positions = ['start','center','end'], main = positions[layout.mode === 'column' ? y : x], cross = positions[layout.mode === 'column' ? x : y];
      const label = `Align ${['top','middle','bottom'][y]} ${['left','center','right'][x]}`, control = element('button'); control.setAttribute('aria-label', label); control.title = label;
      control.setAttribute('aria-pressed', String((layout.main || 'start') === main && (layout.cross || 'start') === cross));
      control.append(element('span')); control.onclick = () => write({ main, cross }); grid.append(control);
    }
    const metrics = element('div', 'layout-metrics');
    const gap = numeric('Gap', layout.gap || 0, n => write({ gap:Math.max(0, n) }), icon('gap'));
    gap.append(toggle('object-distribute-h', 'Auto gap (space between)', layout.main === 'space_between', auto => write({ main:auto ? 'space_between' : 'start' })));
    metrics.append(gap);
    if (layout.wrap) metrics.append(numeric('Cross gap', layout.gap_cross ?? layout.gap ?? 0, n => write({ gap_cross:Math.max(0, n) }), icon('layout-wrap')));
    const pad = layout.padding || [0,0,0,0], sides = perSide.has(target.id);
    if (sides) {
      const padding = element('div', 'field-pair padding-sides');
      for (const [i, label] of ['Padding top','Padding right','Padding bottom','Padding left'].entries()) padding.append(numeric(label, pad[i], n => write({ padding:pad.map((v, j) => i === j ? Math.max(0, n) : v) }), ['T','R','B','L'][i]));
      metrics.append(padding);
    } else {
      metrics.append(pair(
        numeric('Horizontal padding', [pad[1],pad[3]], n => write({ padding:[pad[0],Math.max(0, n),pad[2],Math.max(0, n)] }), icon('padding-h')),
        numeric('Vertical padding', [pad[0],pad[2]], n => write({ padding:[Math.max(0, n),pad[1],Math.max(0, n),pad[3]] }), icon('padding-v'))));
    }
    const options = element('div', 'layout-options');
    options.append(toggle('stretch', 'Stretch children', layout.cross === 'stretch', value => write({ cross:value ? 'stretch' : 'start' })), toggle('padding-sides', sides ? 'Paired padding' : 'Per-side padding', sides, value => { if (value) perSide.add(target.id); else perSide.delete(target.id); render(); }));
    metrics.append(options); cluster.append(grid, metrics); container.append(cluster);
  }
  function geometry(container, node, slide) {
    const rect = api.rectOf(node.id) || { x:0, y:0, w:0, h:0 }, free = isAbsolute(slide, node), selected = nodes();
    const place = (key, label) => {
      const row = field(label, selected.map(n => isAbsolute(slide, n) ? (typeof n.position === 'object' ? Math.round(n.position[key] * 1000) / 1000 : 0) : 'auto'), value => {
        api.updateMany(slide.id, selected.filter(n => isAbsolute(slide, n)).map(n => ({ id:n.id, fields:{ position:{ ...(typeof n.position === 'object' ? n.position : { x:0, y:0 }), [key]:value } } })));
      }, null, free ? 'number' : 'text'); row.lastChild.disabled = !free; return row;
    };
    const position = pair(place('x', 'X'), place('y', 'Y')); position.classList.add('position-fields');
    position.append(toggle('absolute', 'Ignore auto layout', selected.every(n => typeof n.position === 'object'), value => api.updateMany(slide.id, selected.map(n => {
      const r = api.rectOf(n.id) || rect, p = parentOf(slide, n.id)?.parent, o = p === slide ? { x:0,y:0 } : api.rectOf(p.id) || { x:0,y:0 };
      return { id:n.id, fields:{ position:value ? { x:Math.round(r.x - o.x), y:Math.round(r.y - o.y) } : 'auto' } };
    }))));
    container.append(position);
    const dimensions = element('div', 'field-pair');
    for (const [key, label] of [['w', 'W'], ['h', 'H']]) {
      const row = numeric(label, selected.map(n => Math.round(api.rectOf(n.id)?.[key] || 0)), n => { if (n >= 0) patch({ sizing:{ [key]:n } }); });
      const mode = field(key === 'w' ? 'Width mode' : 'Height mode', selected.map(n => typeof n.sizing?.[key] === 'number' ? 'fixed' : n.sizing?.[key] || 'hug'), value => api.updateMany(slide.id, selected.map(n => ({ id:n.id, fields:{ sizing:{ [key]:value === 'fixed' ? Math.round(api.rectOf(n.id)?.[key] || 0) : value } } }))), [['fixed','Fix'],['hug','Hug'],['fill','Fill']]).lastChild;
      mode.classList.add('sizing-mode'); row.append(mode); dimensions.append(row);
    }
    container.append(dimensions);
    const rotation = numeric('Rotation', selected.map(n => n.rotation || 0), n => patch({ rotation:n }), icon('rotation'));
    const radius = selected.every(n => ['shape','frame'].includes(n.kind)) ? numeric('Layout corner radius', selected.map(n => n.radius || 0), n => patch({ radius:Math.max(0, n) }), icon('radius')) : element('span');
    container.append(pair(rotation, radius));
    if (limits.has(node.id)) {
      const grid = element('div', 'field-pair limits-fields');
      for (const key of ['min_w','max_w','min_h','max_h']) {
        const row = numeric(key.replace('_',' ').toUpperCase(), selected.map(n => n.sizing?.[key] ?? ''), n => patch({ sizing:{ [key]:Math.max(0, n) } }), key.replace('_',' '));
        row.append(button('x', `Clear ${key}`, () => patch({ sizing:{ [key]:null } }))); grid.append(row);
      }
      container.append(grid);
    }
  }
  function paint(container, label, values, change) {
    const valid = value => Object.hasOwn(api.state.deck.theme.colors, value) || /^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.test(value);
    const row = field(label, values, value => { if (valid(value)) change(value); }); row.classList.add('paint-field'); row.lastChild.setAttribute('list', 'paint-roles');
    const swatch = element('input', 'paint-swatch'); swatch.type = 'color'; swatch.value = color(values[0] || 'fg', api.state.deck.theme); swatch.setAttribute('aria-label', `Custom ${label.toLowerCase()}`); swatch.title = `Custom ${label.toLowerCase()}`; swatch.onchange = () => change(swatch.value);
    row.insertBefore(swatch, row.lastChild); container.append(row); return row;
  }
  function render(preserveLayers = false) {
    fonts.close(); panel.replaceChildren(); renderToolbar();
    const slide = api.slide();
    if (!preserveLayers) listContainer.replaceChildren();
    if (!slide) { panel.append(element('p', 'hint', 'Add a slide to get started.')); return; }
    if (preserveLayers) listContainer.querySelectorAll('[data-layer]').forEach(row => { const active = api.state.selected.has(row.dataset.layer); row.classList.toggle('active', active); row.setAttribute('aria-selected', String(active)); });
    else layerRows(slide, slide.children, 0);
    const roles = element('datalist'); roles.id = 'paint-roles';
    for (const role of Object.keys(api.state.deck.theme.colors)) { const option = element('option'); option.value = role; roles.append(option); } panel.append(roles);
    const selected = nodes(), node = selected[0];
    if (node) {
      const selection = element('div', 'selection-name', selected.length > 1 ? `${selected.length} selected layers` : node.name || node.kind); selection.title = selection.textContent; panel.append(selection);
      const more = button('more', 'Min/max size', () => { limits.has(node.id) ? limits.delete(node.id) : limits.add(node.id); render(); }); more.setAttribute('aria-expanded', String(limits.has(node.id)));
      const geometrySection = section('Layout / position', [more]); api.alignRow(geometrySection); geometry(geometrySection, node, slide);
    }
    const parent = node && parentOf(slide, node.id)?.parent;
    const target = !node ? slide : selected.length === 1 && node.kind === 'frame' ? node : selected.length === 1 && parent?.layout?.mode !== 'none' && parent?.layout?.mode ? parent : null;
    const add = button('plus', 'Wrap selection (Shift+A)', () => api.wrap()); add.disabled = !node;
    const remove = button('minus', 'Unwrap (Alt+Shift+A)', () => { if (target === node) api.wrap(true); else api.apply(() => [{ op:'unwrap', id:target.id }]); }); remove.disabled = !target || target === slide;
    const auto = section('Auto layout', [add, remove], !target);
    if (target) {
      if (node && target !== node) auto.append(element('div', 'context-label', `In ${nodeLabel(target)}`));
      layoutFields(auto, target.layout || { mode:'none' }, target, target === slide);
    }
    if (node) {
      const text = selected.find(n => TEXTY.includes(n.kind));
      if (text) fonts.render(section('Typography'), text);
      const appearance = section('Appearance'), opacity = numeric('Opacity', selected.map(n => n.opacity ?? 1), n => patch({ opacity:Math.max(0, Math.min(1, n)) }), icon('opacity'));
      appearance.append(pair(opacity, selected.every(n => ['shape','frame'].includes(n.kind)) ? numeric('Corner radius', selected.map(n => n.radius || 0), n => patch({ radius:Math.max(0, n) }), icon('radius')) : element('span')));
      paint(appearance, text ? 'Text color' : 'Fill', selected.map(n => TEXTY.includes(n.kind) ? resolveFont(api.state.deck.theme, n.style, n.font).color : n.fill || 'fg'), value => api.updateMany(slide.id, selected.map(n => ({ id:n.id, fields:TEXTY.includes(n.kind) ? { font:{ color:value } } : { fill:value } }))));
      if (selected.every(n => ['shape','frame'].includes(n.kind))) {
        const row = paint(appearance, 'Stroke', selected.map(n => n.stroke?.color || 'muted'), value => api.updateMany(slide.id, selected.map(n => ({ id:n.id, fields:{ stroke:{ color:value, width:n.stroke?.width || 4 } } }))));
        row.append(toggle('check', 'Stroke enabled', selected.every(n => n.stroke), enabled => patch({ stroke:enabled ? { color:'muted', width:4 } : null })));
      }
      if (node.kind === 'image') appearance.append(field('Image fit', node.fit, fit => patch({ fit }), ['contain','cover']));
    } else {
      paint(section('Appearance'), 'Background', [slide.fill || 'bg'], fill => api.apply(() => [{ op:'update_slide', id:slide.id, fields:{ fill } }]));
      panel.append(element('p', 'hint', 'Select a layer to make it yours. Double-click text to edit.'));
    }
    const themeSection = section('Theme', [], Boolean(node)), presets = element('div', 'quick-actions');
    for (const [name, values] of [['Warm', ['#faf9f6','#282923','#ae593e','#76766b']], ['Dark', ['#242721','#f1eee5','#d9a184','#a0a18e']]]) {
      const preset = element('button', '', name); preset.onclick = () => api.apply(() => { const theme = structuredClone(api.state.deck.theme); ['bg','fg','accent','muted'].forEach((k, i) => theme.colors[k] = values[i]); return [{ op:'set_theme', theme }]; }); presets.append(preset);
    }
    themeSection.append(presets);
    const swatches = element('div', 'swatches');
    for (const [role, value] of Object.entries(api.state.deck.theme.colors)) {
      const row = element('label', 'swatch-label'); const picker = element('input'); picker.type = 'color'; picker.value = value; picker.setAttribute('aria-label', `${role} theme color`); picker.onchange = () => api.apply(() => { const theme = structuredClone(api.state.deck.theme); theme.colors[role] = picker.value; return [{ op:'set_theme', theme }]; }); row.append(picker, element('span', '', ({bg:'Canvas',fg:'Text',accent:'Accent',muted:'Muted'})[role] || role)); swatches.append(row);
    }
    themeSection.append(swatches);
  }
  return { render, renderToolbar };
}
