import { layouts } from './helpers.js';
import { element } from './render.js';

export function panelController(api) {
  const panel = document.querySelector('#properties');
  function field(label, value, change, options, type = 'text') {
    const row = element('label', 'property'); row.append(element('span', '', label));
    const input = element(options ? 'select' : 'input');
    if (options) for (const option of options) { const node = element('option', '', option); node.value = option; input.append(node); }
    else input.type = type;
    input.value = value ?? '';
    input.onchange = () => change(input.value);
    row.append(input); return row;
  }
  function render() {
    panel.replaceChildren();
    const slide = api.slide();
    if (!slide) { panel.append(element('p', 'hint', 'Add a slide to get started.')); return; }
    panel.append(field('Layout', slide.layout, layout => api.apply(() => [{ op: 'set_layout', slide: slide.id, layout }]), layouts));
    const blocks = slide.blocks.filter(b => api.state.selected.has(b.id));
    if (blocks.length) {
      panel.append(element('h3', '', blocks.length === 1 ? blocks[0].kind : `${blocks.length} blocks`));
      const block = blocks[0];
      if (blocks.length === 1) {
        const rect = element('div', 'rect-fields');
        for (const key of ['x', 'y', 'w', 'h']) rect.append(field(key, block[key], value => {
          const number = Number(value);
          if (Number.isFinite(number)) api.updateMany(slide.id, [{ id: block.id, fields: { [key]: Math.round(number) } }]);
        }, null, 'number'));
        panel.append(rect);
      }
      const restyle = patch => api.apply(() => blocks.map(block => {
        const current = api.slide()?.blocks.find(b => b.id === block.id) || block;
        return { op: 'restyle', target: block.id, style: { ...current.style, ...patch } };
      }));
      panel.append(field('Text size', block.style.size || 'm', size => restyle({ size }), ['s', 'm', 'l', 'xl', 'xxl']));
      panel.append(field('Align', block.style.align || 'left', align => restyle({ align }), ['left', 'center', 'right']));
      panel.append(field('Color', block.style.color || 'fg', color => restyle({ color })));
      panel.append(element('p', 'hint', 'Color: a theme role or #hex.'));
      for (const block of blocks) {
        if (block.kind === 'image') panel.append(field('Image fit', block.fit, fit => api.updateMany(slide.id, [{ id: block.id, fields: { fit } }]), ['contain', 'cover']));
        if (block.kind === 'shape') {
          panel.append(field('Shape', block.shape, shape => api.updateMany(slide.id, [{ id: block.id, fields: { shape } }]), ['rect', 'ellipse', 'line', 'arrow']));
          for (const key of ['fill', 'stroke']) panel.append(field(key, block[key], value => api.updateMany(slide.id, [{ id: block.id, fields: { [key]: value } }])));
        }
      }
    } else panel.append(element('p', 'hint', 'Select a block to edit. Shift-click selects more. Double-click text to write.'));
    panel.append(element('h3', '', 'Theme colors'));
    for (const [role, value] of Object.entries(api.state.deck.theme.colors)) {
      panel.append(field(role, value, value => api.apply(() => {
        const theme = structuredClone(api.state.deck.theme); theme.colors[role] = value;
        return [{ op: 'set_theme', theme }];
      })));
    }
    panel.append(element('p', 'hint', 'Arrows nudge · Shift ×4\nDelete removes · ⌘/Ctrl+D duplicates'));
  }
  return { render };
}
