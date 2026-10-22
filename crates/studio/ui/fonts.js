import { resolveFont } from './helpers.js';
import { element, fontStack } from './render.js';

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
  function close() { popup?.observer?.disconnect(); popup?.element.remove(); popup = null; }
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
    panel.append(search, list, importButton); document.body.append(panel);
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
    const box = anchor?.getBoundingClientRect();
    panel.style.top = `${Math.min(window.innerHeight - 440, Math.max(72, box?.top || 120))}px`;
    panel.style.left = `${Math.max(16, Math.min(window.innerWidth - 340, (box?.left || window.innerWidth - 340) - 320))}px`;
    draw(); search.focus();
  }
  document.addEventListener('pointerdown', event => { if (popup && !popup.element.contains(event.target) && event.target !== popup.anchor) close(); });
  function render(container, node) {
    request();
    const theme = api.state.deck.theme, resolved = resolveFont(theme, node.style, node.font), selected = nodes().filter(n => ['text','code','table'].includes(n.kind));
    const values = key => selected.map(n => resolveFont(theme, n.style, n.font)[key]);
    const detached = n => (n.style || (n.kind === 'code' ? 'code' : 'body')) === (n.kind === 'code' ? 'code' : 'body') && Object.keys(resolveFont(theme, n.style, n.font)).every(key => n.font?.[key] != null);
    container.append(field('Text style', selected.map(n => detached(n) ? 'detached' : n.style || 'body'), style => { if (style !== 'detached') patch({ style, font:null }); }, [...(selected.some(detached) ? [['detached','Detached']] : []), ...Object.keys(theme.text_styles)]));
    const actions = element('div', 'style-actions');
    const detach = element('button', '', 'Detach'); detach.onclick = () => api.updateMany(api.slide().id, selected.map(n => ({ id:n.id, fields:{ style:null, font:resolveFont(theme, n.style, n.font) } })));
    const update = element('button', '', 'Update style'); update.onclick = () => api.apply(() => [{ op:'set_text_style', name:node.style || 'body', font:resolved }]);
    const create = element('button', '', 'Create style'); create.onclick = () => api.ask('Create style', 'Style name', 'Up to 32 lowercase letters, digits, - and _. Spaces become dashes.', '', value => {
      const name = value.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, '-').slice(0, 32);
      if (name) api.apply(() => [{ op:'set_text_style', name, font:resolved }, ...selected.map(n => ({ op:'update_node', id:n.id, fields:{ style:name, font:null }, base_hash:api.state.hashes[n.id] }))]);
    });
    actions.append(detach, update, create); container.append(actions);
    const familyRow = element('div', 'property'), picker = element('button', 'family-picker', concrete(resolved.family)); picker.setAttribute('aria-label', 'Font family'); picker.onclick = () => open(picker); familyRow.append(element('span', '', 'Family'), picker); container.append(familyRow);
    const size = field('Size (px)', values('size'), n => { if (n > 0) font({ size:n }); }, null, 'number');
    const sizes = element('select', 'size-presets'); sizes.setAttribute('aria-label', 'Size presets');
    for (const n of [12,14,16,18,20,24,28,32,36,40,48,56,64,72,80,96,120,144,160,200]) { const option = element('option', '', String(n)); option.value = n; sizes.append(option); }
    sizes.value = resolved.size; sizes.onchange = () => font({ size:Number(sizes.value) }); size.append(sizes); container.append(size);
    const metadata = info(resolved.family), weights = metadata.weights.length ? metadata.weights : [resolved.weight];
    container.append(field('Weight', values('weight'), value => { if (value !== 'variable') font({ weight:Number(value) }); }, [...weights.map(n => [String(n), String(n)]), ...(metadata.variable ? [['variable','Variable']] : [])]));
    if (metadata.variable) container.append(field('Variable weight', values('weight'), value => font({ weight:Math.max(100, Math.min(900, Math.round(value))) }), null, 'number'));
    container.append(field('Italic', values('italic'), italic => font({ italic }), null, 'checkbox'));
    container.append(field('Line height mode', node.font?.line_height == null ? 'auto' : 'custom', mode => font({ line_height:mode === 'auto' ? null : resolved.line_height }), [['auto','Auto'],['custom','Multiplier']]));
    container.append(field('Line height', values('line_height'), n => { if (n >= .5 && n <= 5) font({ line_height:n }); }, null, 'number'));
    container.append(field('Letter spacing (%)', values('letter_spacing').map(n => Math.round(n * 10000) / 100), n => font({ letter_spacing:n / 100 }), null, 'number'));
    container.append(field('Alignment', values('align'), align => font({ align }), ['left','center','right']));
    container.append(field('Vertical', values('valign'), valign => font({ valign }), ['top','middle','bottom']));
    container.append(field('Case', values('transform'), transform => font({ transform }), ['none','uppercase','lowercase','capitalize']));
    container.append(field('Text color', values('color'), color => font({ color }), [...Object.keys(theme.colors), ...(resolved.color.startsWith('#') ? [resolved.color] : [])]));
    container.append(field('Hex color', resolved.color.startsWith('#') ? resolved.color : theme.colors[resolved.color], color => { if (/^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.test(color)) font({ color }); }));
  }
  return { render, close };
}
