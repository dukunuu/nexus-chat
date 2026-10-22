import { resolveFont } from './helpers.js';
import { element, fontStack } from './render.js';
import { compact } from './fields.js';
import { actionMenu, button, icon } from './icons.js';

export function fontController(api, field, nodes) {
  let families = null, requested = false, popup, fileSignature = '';
  const patch = fields => api.updateMany(api.slide().id, nodes().filter(n => ['text','code','table'].includes(n.kind)).map(n => ({ id:n.id, fields })));
  const font = fields => patch({ font:fields });
  const concrete = name => api.state.deck.theme.fonts[name] || name;
  function request() {
    const signature = JSON.stringify(api.state.deck.theme.font_files || []);
    if (signature !== fileSignature) { fileSignature = signature; requested = false; }
    if (requested) return;
    requested = true;
    api.command({ command:'ListFonts' }, event => { if (event.event === 'Fonts') { families = event.families; if (popup) open(popup.anchor, popup.search.value); else if (!api.state.scrubbing) api.selectionChanged(); } else requested = false; });
  }
  function info(family) {
    const name = concrete(family), files = (api.state.deck.theme.font_files || []).filter(f => f.family === name);
    const installed = families?.find(f => f.family === name);
    return { weights:[...new Set([...(installed?.weights || []), ...files.map(f => f.weight)])].sort((a, b) => a - b), variable:installed?.variable };
  }
  function close() { popup?.observer?.disconnect(); popup?.anchor?.setAttribute('aria-expanded', 'false'); popup?.element.remove(); popup = null; }
  function open(anchor, query = '') {
    close(); request();
    const panel = element('div', 'font-popover popover'); panel.id = 'font-picker';
    const search = element('input'); search.placeholder = 'Search fonts'; search.setAttribute('aria-label', 'Search fonts');
    const list = element('div', 'font-list'); list.setAttribute('role', 'listbox');
    const importButton = element('button', 'import-font', 'Import font…'); importButton.onclick = () => {
      close();
      const input = element('input'); input.type = 'file'; input.accept = '.ttf,.otf,.woff2'; input.hidden = true; document.body.append(input);
      input.oncancel = () => input.remove();
      input.onchange = async () => {
        const file = input.files[0]; input.remove(); if (!file) return;
        if (file.size > 16 * 1024 * 1024) { api.toast('Font exceeds the 16 MB size limit.'); return; }
        const bytes = new Uint8Array(await file.arrayBuffer()), hex = Array.from({ length:256 }, (_, i) => i.toString(16).padStart(2, '0')), parts = [];
        for (let i = 0; i < bytes.length; i += 16384) parts.push(Array.from(bytes.subarray(i, i + 16384), b => hex[b]).join(''));
        api.command({ command:'ImportFontData', name:file.name, data:parts.join('') });
      };
      input.click();
    };
    panel.append(search, list, importButton); anchor.closest('.family-row').append(panel); anchor.setAttribute('aria-expanded', 'true');
    const observer = new IntersectionObserver(entries => { for (const entry of entries) if (entry.isIntersecting) { entry.target.style.fontFamily = fontStack(entry.target.dataset.family); observer.unobserve(entry.target); } }, { root:list });
    popup = { element:panel, anchor, observer, search }; search.value = query;
    let active = 0, rows = [];
    const highlight = () => { rows.forEach((row, i) => row.setAttribute('aria-selected', String(i === active))); rows[active]?.scrollIntoView({ block:'nearest' }); };
    function draw() {
      observer.disconnect(); list.replaceChildren(); rows = []; active = 0;
      const theme = api.state.deck.theme, deckNames = [...new Set([...Object.values(theme.fonts), ...(theme.font_files || []).map(f => f.family)])];
      for (const [title, names] of [['In this deck', deckNames], ['Installed', (families || []).map(f => f.family).filter(n => !deckNames.includes(n))]]) {
        const matches = names.filter(n => n.toLowerCase().includes(search.value.toLowerCase()));
        if (!matches.length) continue;
        list.append(element('div', 'font-group', title));
        for (const name of matches) {
          const row = element('button', 'font-row', name); row.dataset.family = name; row.setAttribute('role', 'option'); row.tabIndex = -1;
          row.onclick = () => { font({ family:name }); close(); };
          row.onpointermove = () => { active = rows.indexOf(row); rows.forEach((r, i) => r.setAttribute('aria-selected', String(i === active))); };
          rows.push(row); list.append(row); observer.observe(row);
        }
      }
      highlight();
    }
    search.oninput = draw;
    panel.onkeydown = event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); anchor?.focus(); }
      else if (['ArrowDown','ArrowUp','Home','End'].includes(event.key)) { event.preventDefault(); active = event.key === 'Home' ? 0 : event.key === 'End' ? rows.length - 1 : Math.max(0, Math.min(rows.length - 1, active + (event.key === 'ArrowDown' ? 1 : -1))); highlight(); }
      else if (event.key === 'Enter') { event.preventDefault(); rows[active]?.click(); }
    };
    draw(); search.focus({ preventScroll:true });
  }
  document.addEventListener('pointerdown', event => { if (popup && !popup.element.contains(event.target) && !popup.anchor.contains(event.target)) close(); });
  function render(container, node) {
    request();
    const theme = api.state.deck.theme, resolved = resolveFont(theme, node.style, node.font), selected = nodes().filter(n => ['text','code','table'].includes(n.kind));
    const values = key => selected.map(n => resolveFont(theme, n.style, n.font)[key]);
    const detached = n => (n.style || (n.kind === 'code' ? 'code' : 'body')) === (n.kind === 'code' ? 'code' : 'body') && Object.keys(resolveFont(theme, n.style, n.font)).every(key => n.font?.[key] != null);
    const pair = (...controls) => { const row = element('div', 'field-pair'); row.append(...controls); return row; };
    const numeric = (label, value, change, glyph) => compact(field(label, value, change, null, 'number'), glyph);
    const style = compact(field('Text style', selected.map(n => detached(n) ? 'detached' : n.style || 'body'), style => { if (style !== 'detached') patch({ style, font:null }); }, [...(selected.some(detached) ? [['detached','Detached']] : []), ...Object.keys(theme.text_styles)]), 'Text style ·');
    style.classList.add('text-style-row');
    const menu = actionMenu('Text style actions', [
      ['Detach', () => api.updateMany(api.slide().id, selected.map(n => ({ id:n.id, fields:{ style:null, font:resolveFont(theme, n.style, n.font) } })))],
      ['Update style', () => api.apply(() => [{ op:'set_text_style', name:node.style || 'body', font:resolved }])],
      ['Create style', () => api.ask('Create style', 'Style name', 'Up to 32 lowercase letters, digits, - and _. Spaces become dashes.', '', value => {
        const name = value.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, '-').slice(0, 32);
        if (name) api.apply(() => [{ op:'set_text_style', name, font:resolved }, ...selected.map(n => ({ op:'update_node', id:n.id, fields:{ style:name, font:null }, base_hash:api.state.hashes[n.id] }))]);
      })],
    ]);
    const styleRow = element('div', 'text-style-header'); styleRow.append(style, menu); container.append(styleRow);
    const familyRow = element('div', 'family-row'), picker = element('button', 'family-picker', values('family').every(f => f === resolved.family) ? concrete(resolved.family) : 'Mixed');
    picker.setAttribute('aria-label', 'Font family'); picker.setAttribute('aria-expanded', 'false'); picker.append(icon('chevron')); picker.title = 'Font family'; picker.onclick = () => popup ? close() : open(picker); familyRow.append(picker); container.append(familyRow);
    const size = numeric('Size (px)', values('size'), n => { if (n > 0) font({ size:n }); }, 'Size');
    const sizes = element('select', 'size-presets'); sizes.setAttribute('aria-label', 'Size presets'); sizes.title = 'Size presets';
    const placeholder = element('option', '', ''); placeholder.value = ''; placeholder.disabled = true; sizes.append(placeholder);
    for (const n of [12,14,16,18,20,24,28,32,36,40,48,56,64,72,80,96,120,144,160,200]) { const option = element('option', '', String(n)); option.value = n; sizes.append(option); }
    sizes.value = ''; sizes.onchange = () => font({ size:Number(sizes.value) }); size.append(sizes);
    const metadata = info(resolved.family), weights = metadata.weights.length ? metadata.weights : [resolved.weight];
    const weight = compact(field('Weight', values('weight'), value => { if (value !== 'variable') font({ weight:Number(value) }); }, [...weights.map(n => [String(n), String(n)]), ...(metadata.variable ? [['variable','Variable']] : [])]), 'Wt');
    container.append(pair(weight, size));
    if (metadata.variable) container.append(numeric('Variable weight', values('weight'), value => font({ weight:Math.max(100, Math.min(900, Math.round(value))) }), 'Wt'));
    const line = numeric('Line height', values('line_height'), n => { if (n >= .5 && n <= 5) font({ line_height:n }); }, icon('line-height'));
    const auto = element('button', 'inline-mode', 'Auto'); auto.title = 'Automatic line height'; auto.setAttribute('aria-label', 'Automatic line height'); auto.setAttribute('aria-pressed', String(node.font?.line_height == null)); auto.onclick = () => font({ line_height:node.font?.line_height == null ? resolved.line_height : null }); line.append(auto);
    container.append(pair(line, numeric('Letter spacing (%)', values('letter_spacing').map(n => Math.round(n * 10000) / 100), n => font({ letter_spacing:n / 100 }), icon('letter-spacing'))));
    const segments = (key, label, options) => {
      const group = element('div', 'segmented'); group.setAttribute('role', 'group'); group.setAttribute('aria-label', label);
      for (const [value, glyph, title] of options) { const control = button(glyph, title, () => font({ [key]:value })); control.setAttribute('aria-pressed', String(values(key).every(v => v === value))); group.append(control); }
      return group;
    };
    container.append(pair(segments('align', 'Text alignment', [['left','align-left','Text align left'],['center','align-center','Text align center'],['right','align-right','Text align right']]), segments('valign', 'Vertical alignment', [['top','object-top','Text align top'],['middle','object-v-center','Text align middle'],['bottom','object-bottom','Text align bottom']])));
    const toggles = element('div', 'type-toggles');
    for (const [value, title] of [['uppercase','Uppercase'],['lowercase','Lowercase'],['capitalize','Capitalize']]) {
      const active = values('transform').every(v => v === value), control = button(`case-${value === 'uppercase' ? 'upper' : value === 'lowercase' ? 'lower' : 'capitalize'}`, title, () => font({ transform:active ? 'none' : value })); control.setAttribute('aria-pressed', String(active)); toggles.append(control);
    }
    const italic = button('italic', 'Italic', () => font({ italic:!values('italic').every(Boolean) })); italic.setAttribute('aria-pressed', String(values('italic').every(Boolean))); toggles.append(italic); container.append(toggles);
  }
  return { render, close };
}
