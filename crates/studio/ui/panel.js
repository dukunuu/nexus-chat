import { layouts } from './helpers.js';
import { element, color, fontStack, availableFont } from './render.js';
import { button, icon } from './icons.js';

export function panelController(api) {
  const panel = document.querySelector('#properties'), toolbar = document.querySelector('#floating-toolbar');
  const blocks = () => api.slide()?.blocks.filter(b => api.state.selected.has(b.id)) || [];
  function field(label, value, change, options, type = 'text') {
    const row = element('label', 'property'); row.append(element('span', '', label));
    const input = element(options ? 'select' : 'input');
    if (options) for (const [value, label] of options.map(o => Array.isArray(o) ? o : [o, o])) { const node = element('option', '', label); node.value = value; input.append(node); }
    else input.type = type;
    input.value = value ?? ''; input.onchange = () => change(input.value); row.append(input); return row;
  }
  function restyle(patch) {
    api.apply(() => blocks().map(block => ({ op: 'restyle', target: block.id, style: { ...block.style, ...patch } })));
  }
  function mark(marker) {
    api.updateMany(api.slide().id, blocks().filter(b => b.kind === 'text').map(b => ({ id: b.id, fields: { text: b.text.startsWith(marker) && b.text.endsWith(marker) ? b.text.slice(marker.length, -marker.length) : `${marker}${b.text}${marker}` } })));
  }
  function reorder(id, to) {
    const slide = api.slide();
    // Reinsert the same blocks in the desired order as one transaction.
    const ordered = [...slide.blocks]; const index = ordered.findIndex(b => b.id === id);
    const [block] = ordered.splice(index, 1); ordered.splice(Math.max(0, Math.min(ordered.length, to)), 0, block);
    api.apply(() => [...slide.blocks.map(b => ({ op: 'delete_block', slide: slide.id, id: b.id })), ...ordered.map(block => ({ op: 'insert_block', slide: slide.id, block }))]);
  }
  function colors(container, labels = false) {
    const selected = blocks();
    for (const [role, label] of [['accent','Accent'], ['fg','Text'], ['muted','Muted']]) {
      const node = button('check', `${label} color`, () => restyle({ color: role }), 'color-option'); node.replaceChildren(element('span', 'color-chip'));
      node.setAttribute('aria-pressed', String(selected.length > 0 && selected.every(b => (b.style.color || 'fg') === role)));
      if (labels) node.append(element('span', 'color-name', label));
      node.style.setProperty('--swatch', api.state.deck.theme.colors[role]); container.append(node);
    }
    const custom = element('label', 'custom-color'); custom.title = 'Custom color';
    custom.classList.toggle('selected', selected.length > 0 && selected.every(b => b.style.color?.startsWith('#') && b.style.color === selected[0].style.color));
    const picker = element('input', 'color-picker'); picker.type = 'color'; picker.title = 'Custom color'; picker.setAttribute('aria-label', 'Custom color'); picker.value = color(selected[0]?.style.color || 'fg', api.state.deck.theme); picker.onchange = () => restyle({ color: picker.value }); custom.append(picker);
    if (labels) custom.append(element('span', 'color-name', 'Custom'));
    container.append(custom);
  }
  function renderToolbar() {
    toolbar.replaceChildren();
    const selected = blocks(); const block = selected[0];
    toolbar.hidden = !block || api.state.properties || api.state.mode === 'comment' || Boolean(api.state.preview);
    if (toolbar.hidden) return;
    const size = field('Text size', block.style.size || 'm', size => restyle({ size }), [['s', 'Small'], ['m', 'Body'], ['l', 'Large'], ['xl', 'Title'], ['xxl', 'Hero']]).lastChild;
    size.setAttribute('aria-label', 'Text size'); toolbar.append(size);
    if (selected.some(b => b.kind === 'text')) toolbar.append(button('bold', 'Bold', () => mark('**')), button('italic', 'Italic', () => mark('*')));
    for (const align of ['left', 'center', 'right']) toolbar.append(button(`align-${align}`, `Align ${align}`, () => restyle({ align })));
    toolbar.append(element('span', 'toolbar-divider')); colors(toolbar); toolbar.append(element('span', 'toolbar-divider'));
    toolbar.append(button('forward', 'Bring forward', () => reorder(block.id, api.slide().blocks.findIndex(b => b.id === block.id) + 1)), button('back', 'Send back', () => reorder(block.id, api.slide().blocks.findIndex(b => b.id === block.id) - 1)), button('duplicate', 'Duplicate', api.duplicate), button('trash', 'Delete', api.remove), button('comment', 'Comment on selection', () => api.commentAt({ slide: api.slide().id, block: block.id, x: block.x, y: block.y })));
    // Keep the toolbar above the selection, within the slide's horizontal bounds.
    toolbar.style.left = `${Math.max(0, Math.min(100 - toolbar.scrollWidth / Math.max(1, document.querySelector('#canvas').clientWidth) * 100, block.x / 32 * 100))}%`;
    toolbar.style.top = `${block.y / 18 * 100 - 3}%`;
  }
  function render() {
    panel.replaceChildren(); renderToolbar();
    const slide = api.slide();
    if (!slide) { panel.append(element('p', 'hint', 'Add a slide to get started.')); return; }
    panel.append(element('h3', '', 'Layers'));
    [...slide.blocks].reverse().forEach(block => {
      const row = element('button', `layer${api.state.selected.has(block.id) ? ' active' : ''}`); row.append(icon(block.kind === 'text' ? 'align-left' : 'layers'), element('span', '', block.text?.replace(/[#*]/g, '').split('\n')[0] || block.kind));
      row.onclick = () => { api.state.selected = new Set([block.id]); api.selectionChanged(); }; row.draggable = true;
      row.ondragstart = e => e.dataTransfer.setData('text/plain', block.id); row.ondragover = e => e.preventDefault(); row.ondrop = e => { e.preventDefault(); const id = e.dataTransfer.getData('text/plain'); if (slide.blocks.some(b => b.id === id)) reorder(id, slide.blocks.indexOf(block)); }; panel.append(row);
    });
    panel.append(element('h3', '', 'Slide'), field('Layout', slide.layout, layout => api.apply(() => [{ op: 'set_layout', slide: slide.id, layout }]), layouts));
    const selected = blocks(), block = selected[0];
    if (block) {
      panel.append(element('h3', '', selected.length > 1 ? `${selected.length} selected layers` : 'Typography'));
      const rendered = [...document.querySelectorAll('#canvas .block')].find(node => node.dataset.id === block.id);
      const content = rendered?.querySelector('h1, h2, h3, pre') || rendered;
      const stack = content ? getComputedStyle(content).fontFamily : fontStack(api.state.deck.theme.fonts.body);
      const font = element('p', 'hint', `Font · ${availableFont(stack)}`); font.title = stack; panel.append(font);
      panel.append(field('Size', block.style.size || 'm', size => restyle({ size }), [['s', 'Small'], ['m', 'Body'], ['l', 'Large'], ['xl', 'Title'], ['xxl', 'Hero']]));
      panel.append(field('Alignment', block.style.align || 'left', align => restyle({ align }), ['left', 'center', 'right']));
      const chips = element('div', 'swatches text-colors'); colors(chips, true); panel.append(chips);
      panel.append(element('h3', '', 'Position & size'));
      const rect = element('div', 'rect-fields');
      for (const [key, label] of [['x','Left'],['y','Top'],['w','Width'],['h','Height']]) rect.append(field(label, block[key], value => { const number = Number(value); if (Number.isFinite(number)) api.updateMany(slide.id, [{ id: block.id, fields: { [key]: Math.round(number) } }]); }, null, 'number'));
      panel.append(rect);
      if (block.kind === 'image') panel.append(field('Image fit', block.fit, fit => api.updateMany(slide.id, [{ id: block.id, fields: { fit } }]), ['contain','cover']));
      if (block.kind === 'shape') for (const key of ['fill','stroke']) panel.append(field(key === 'fill' ? 'Fill' : 'Stroke', color(block[key], api.state.deck.theme), value => api.updateMany(slide.id, [{ id: block.id, fields: { [key]: value } }]), null, 'color'));
    } else panel.append(element('p', 'hint', 'Select a layer to make it yours. Double-click text to edit.'));
    panel.append(element('h3', '', 'Theme'));
    const presets = element('div', 'quick-actions');
    for (const [name, values] of [['Warm', ['#faf9f6','#282923','#ae593e','#76766b']], ['Dark', ['#242721','#f1eee5','#d9a184','#a0a18e']]]) {
      const node = element('button', '', name); node.onclick = () => api.apply(() => { const theme = structuredClone(api.state.deck.theme); ['bg','fg','accent','muted'].forEach((k, i) => theme.colors[k] = values[i]); return [{ op:'set_theme', theme }]; }); presets.append(node);
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
