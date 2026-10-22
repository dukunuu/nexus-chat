import { allNodes, childrenOf, parentOf, isAbsolute, nodeLabel, resolveFont, slideSize } from './helpers.js';
import { element, color, fontStack, availableFont } from './render.js';
import { button, icon } from './icons.js';

const TEXTY = ['text', 'code', 'table'];
const WEIGHTS = [100, 200, 300, 400, 500, 600, 700, 800, 900];

export function panelController(api) {
  const panel = document.querySelector('#properties'), toolbar = document.querySelector('#floating-toolbar');
  const nodes = () => { const slide = api.slide(); return slide ? allNodes(slide).filter(n => api.state.selected.has(n.id)) : []; };
  const number = value => { const n = Number(value); return Number.isFinite(n) ? n : null; };
  function field(label, value, change, options, type = 'text') {
    const row = element('label', 'property'); row.append(element('span', '', label));
    const input = element(options ? 'select' : 'input');
    if (options) for (const [value, label] of options.map(o => Array.isArray(o) ? o : [o, o])) { const node = element('option', '', label); node.value = value; input.append(node); }
    else input.type = type;
    if (type === 'checkbox') { input.checked = Boolean(value); input.onchange = () => change(input.checked); }
    else { input.value = value ?? ''; input.onchange = () => change(input.value); }
    row.append(input); return row;
  }
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
      const row = element('button', `layer${api.state.selected.has(node.id) ? ' active' : ''}`);
      row.style.paddingLeft = `${8 + depth * 14}px`;
      row.append(icon(node.kind === 'text' ? 'align-left' : 'layers'), element('span', '', nodeLabel(node)));
      row.onclick = () => { api.state.selected = new Set([node.id]); api.selectionChanged(); }; row.draggable = true;
      row.ondragstart = event => event.dataTransfer.setData('text/plain', node.id);
      row.ondragover = event => event.preventDefault();
      row.ondrop = event => {
        event.preventDefault();
        const dragged = event.dataTransfer.getData('text/plain'), from = parentOf(slide, dragged), to = parentOf(slide, node.id);
        if (!from || !to || dragged === node.id) return;
        if (event.altKey && node.kind === 'frame') api.apply(() => [{ op: 'move_node', id: dragged, parent: node.id }]);
        else api.apply(() => [{ op: 'move_node', id: dragged, parent: to.parent.id, index: to.index + 1 - (from.parent === to.parent && from.index < to.index ? 1 : 0) }]);
      };
      panel.append(row);
      layerRows(slide, childrenOf(node), depth + 1);
    });
  }
  // -- sections --------------------------------------------------------------------
  const MAIN = ['start', 'center', 'end', 'space_between'], CROSS = ['start', 'center', 'end', 'stretch'];
  function layoutFields(container, layout, target, isSlide) {
    const write = (fields, extra) => isSlide
      ? api.apply(() => [{ op: 'update_slide', id: target.id, fields: { layout: fields } }])
      : api.updateMany(api.slide().id, [{ id: target.id, fields: { layout: fields, ...extra } }]);
    container.append(field('Auto-layout', layout.mode || 'none', mode => api.apply(() => [{ op: 'set_layout', target: target.id, layout: { ...layout, mode } }]), [['none', 'None'], ['row', 'Row'], ['column', 'Column']]));
    if ((layout.mode || 'none') === 'none') return;
    container.append(field('Gap', layout.gap || 0, value => { const n = number(value); if (n !== null) write({ gap: Math.max(0, n) }); }, null, 'number'));
    container.append(field('Padding', layout.padding?.[0] || 0, value => { const n = number(value); if (n !== null) write({ padding: Array(4).fill(Math.max(0, n)) }); }, null, 'number'));
    container.append(field('Main axis', layout.main || 'start', main => write({ main }), MAIN));
    container.append(field('Cross axis', layout.cross || 'start', cross => write({ cross }), CROSS));
    container.append(field('Wrap', layout.wrap, wrap => write({ wrap }), null, 'checkbox'));
  }
  function typography(container, node) {
    const theme = api.state.deck.theme, resolved = resolveFont(theme, node.style, node.font);
    const rendered = [...document.querySelectorAll('#canvas .block')].find(n => n.dataset.id === node.id);
    const content = rendered?.querySelector('h1, h2, h3') || rendered;
    const stack = content ? getComputedStyle(content).fontFamily : fontStack(theme.fonts.body);
    const hint = element('p', 'hint', `Font · ${availableFont(stack)}`); hint.title = stack; container.append(hint);
    container.append(field('Text style', node.style || 'body', style => patch({ style }), Object.keys(theme.text_styles || { body: 1 })));
    container.append(field('Family', node.font?.family ?? '', family => font({ family: family.trim() || null })));
    container.append(field('Size (px)', resolved.size, value => { const n = number(value); if (n > 0) font({ size: n }); }, null, 'number'));
    container.append(field('Weight', resolved.weight, value => font({ weight: Number(value) }), WEIGHTS.map(String)));
    container.append(field('Italic', resolved.italic, italic => font({ italic }), null, 'checkbox'));
    container.append(field('Line height', resolved.line_height, value => { const n = number(value); if (n > 0) font({ line_height: n }); }, null, 'number'));
    container.append(field('Letter spacing (em)', resolved.letter_spacing, value => { const n = number(value); if (n !== null) font({ letter_spacing: n }); }, null, 'number'));
    container.append(field('Alignment', resolved.align, align => font({ align }), ['left', 'center', 'right']));
    container.append(field('Vertical', resolved.valign, valign => font({ valign }), ['top', 'middle', 'bottom']));
    container.append(field('Case', resolved.transform, transform => font({ transform }), ['none', 'uppercase', 'lowercase', 'capitalize']));
  }
  function geometry(container, node, slide) {
    const rect = api.rectOf(node.id) || { x: 0, y: 0, w: 0, h: 0 }, free = isAbsolute(slide, node);
    const at = typeof node.position === 'object' ? node.position : { x: 0, y: 0 };
    const holder = parentOf(slide, node.id)?.parent, origin = holder && holder !== slide ? api.rectOf(holder.id) || { x: 0, y: 0 } : { x: 0, y: 0 };
    const grid = element('div', 'rect-fields');
    const place = (key, label) => { const row = field(label, free ? at[key] : 'auto', value => { const n = number(value); if (n !== null) patch({ position: { x: at.x, y: at.y, [key]: Math.round(n) } }); }, null, free ? 'number' : 'text'); row.lastChild.disabled = !free; return row; };
    grid.append(place('x', 'Left'), place('y', 'Top'));
    for (const [key, label, size] of [['w', 'Width', rect.w], ['h', 'Height', rect.h]]) {
      grid.append(field(label, Math.round(size), value => { const n = number(value); if (n !== null && n >= 0) patch({ sizing: { [key]: Math.round(n) } }); }, null, 'number'));
    }
    container.append(grid);
    for (const [key, label, size] of [['w', 'Width mode', rect.w], ['h', 'Height mode', rect.h]]) {
      const mode = typeof node.sizing?.[key] === 'number' ? 'fixed' : node.sizing?.[key] || 'hug';
      container.append(field(label, mode, value => patch({ sizing: { [key]: value === 'fixed' ? Math.round(size) : value } }), [['fixed', 'Fixed'], ['hug', 'Hug'], ['fill', 'Fill']]));
    }
    container.append(field('Position', free ? 'absolute' : 'auto', value => patch({ position: value === 'auto' ? 'auto' : { x: Math.round(rect.x - origin.x), y: Math.round(rect.y - origin.y) } }), [['auto', 'Follow auto-layout'], ['absolute', 'Absolute']]));
    container.append(field('Opacity', node.opacity ?? 1, value => { const n = number(value); if (n !== null) patch({ opacity: Math.max(0, Math.min(1, n)) }); }, null, 'number'));
  }
  function render() {
    panel.replaceChildren(); renderToolbar();
    const slide = api.slide();
    if (!slide) { panel.append(element('p', 'hint', 'Add a slide to get started.')); return; }
    panel.append(element('h3', '', 'Layers'));
    layerRows(slide, slide.children, 0);
    const selected = nodes(), node = selected[0];
    if (node) {
      panel.append(element('h3', '', selected.length > 1 ? `${selected.length} selected layers` : node.name || node.kind));
      if (selected.length === 1) {
        panel.append(element('h3', '', 'Position & size'));
        geometry(panel, node, slide);
        if (node.kind === 'frame') {
          panel.append(element('h3', '', 'Frame'));
          layoutFields(panel, node.layout || { mode: 'none' }, node, false);
          panel.append(field('Corner radius', node.radius || 0, value => { const n = number(value); if (n !== null) patch({ radius: Math.max(0, n) }); }, null, 'number'));
          panel.append(field('Clip content', node.clip, clip => patch({ clip }), null, 'checkbox'));
        }
      }
      if (selected.some(n => TEXTY.includes(n.kind))) {
        panel.append(element('h3', '', 'Typography'));
        if (selected.length === 1) typography(panel, node);
      }
      const chips = element('div', 'swatches text-colors'); colors(chips, true); panel.append(chips);
      if (node.kind === 'image') panel.append(field('Image fit', node.fit, fit => patch({ fit }), ['contain', 'cover']));
      if (node.kind === 'shape') for (const key of ['fill', 'stroke']) {
        const value = key === 'fill' ? node.fill : node.stroke?.color;
        panel.append(field(key === 'fill' ? 'Fill' : 'Stroke', color(value || 'accent', api.state.deck.theme), picked => patch(key === 'fill' ? { fill: picked } : { stroke: { color: picked, width: node.stroke?.width || 4 } }), null, 'color'));
      }
    } else {
      panel.append(element('h3', '', 'Slide'));
      layoutFields(panel, slide.layout || { mode: 'none' }, slide, true);
      panel.append(field('Background', color(slide.fill || 'bg', api.state.deck.theme), fill => api.apply(() => [{ op: 'update_slide', id: slide.id, fields: { fill } }]), null, 'color'));
      panel.append(element('p', 'hint', 'Select a layer to make it yours. Double-click text to edit.'));
    }
    panel.append(element('h3', '', 'Theme'));
    const presets = element('div', 'quick-actions');
    for (const [name, values] of [['Warm', ['#faf9f6','#282923','#ae593e','#76766b']], ['Dark', ['#242721','#f1eee5','#d9a184','#a0a18e']]]) {
      const preset = element('button', '', name); preset.onclick = () => api.apply(() => { const theme = structuredClone(api.state.deck.theme); ['bg','fg','accent','muted'].forEach((k, i) => theme.colors[k] = values[i]); return [{ op:'set_theme', theme }]; }); presets.append(preset);
    }
    panel.append(presets);
    const swatches = element('div', 'swatches');
    for (const [role, value] of Object.entries(api.state.deck.theme.colors)) {
      const row = element('label', 'swatch-label'); const picker = element('input'); picker.type = 'color'; picker.value = value; picker.setAttribute('aria-label', `${role} theme color`); picker.onchange = () => api.apply(() => { const theme = structuredClone(api.state.deck.theme); theme.colors[role] = picker.value; return [{ op: 'set_theme', theme }]; }); row.append(picker, element('span', '', ({bg:'Canvas',fg:'Text',accent:'Accent',muted:'Muted'})[role] || role)); swatches.append(row);
    }
    panel.append(swatches);
  }
  return { render, renderToolbar };
}
