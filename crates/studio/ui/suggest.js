import { element } from './render.js';

export function suggestController(api) {
  const panel = document.querySelector('#suggestions-panel');
  const results = document.querySelector('#suggestion-results');
  const input = document.querySelector('#instruction');
  const button = document.querySelector('#suggest');
  const host = document.querySelector('#host-status');
  let connected = false, busy = false, previewId = null;
  function tab(suggest) {
    panel.hidden = !suggest; document.querySelector('#properties').hidden = suggest;
    document.querySelector('#suggest-tab').setAttribute('aria-selected', String(suggest));
    document.querySelector('#properties-tab').setAttribute('aria-selected', String(!suggest));
  }
  document.querySelector('#suggest-tab').onclick = () => tab(true);
  document.querySelector('#properties-tab').onclick = () => tab(false);
  const selection = () => ({ slides: api.state.scopeSlide && api.slide() ? [api.slide().id] : [], blocks: [...api.state.selected].map(id => [api.state.slideId, id]) });
  function scope() {
    const selected = selection();
    document.querySelector('#suggest-scope').textContent = selected.slides.length || selected.blocks.length ? 'selection' : 'whole deck';
  }
  const prompts = ['Tighten', 'Fix layout', 'Make it visual', 'Consistent style', 'Speaker notes'];
  for (const prompt of prompts) {
    const quick = element('button', '', prompt);
    quick.onclick = () => { input.value = prompt; input.focus(); };
    document.querySelector('#quick-prompts').append(quick);
  }
  function controls() { button.disabled = !connected || busy; button.classList.toggle('waiting', busy); button.textContent = busy ? 'Suggesting…' : 'Suggest'; }
  button.onclick = () => api.command({ command: 'Suggest', instruction: input.value, selection: selection() });
  document.querySelector('#start-host').onclick = () => api.command({ command: 'StartHost' });
  host.onclick = () => api.command({ command: 'HostStatus' });
  function clearPreview() {
    previewId = null; api.state.preview = null; document.querySelector('#preview-toggle').hidden = true; document.querySelector('#close-preview').hidden = true; api.renderCanvas();
  }
  function targetSlide(suggestion) {
    for (const op of suggestion.ops) {
      if (typeof op.slide === 'string') return op.slide;
      if (op.slide?.id) return op.slide.id;
      const slide = api.state.deck.slides.find(s => s.id === op.id || s.id === op.target || s.blocks.some(b => b.id === op.target));
      if (slide) return slide.id;
    }
    return api.state.slideId;
  }
  function preview(suggestion) {
    if (previewId === suggestion.id) return;
    previewId = suggestion.id;
    api.command({ command: 'Preview', suggestion_id: suggestion.id }, event => {
      if (event.event !== 'Preview' || previewId !== suggestion.id) return;
      api.state.preview = { deck: event.deck, slideId: targetSlide(suggestion), before: false };
      document.querySelector('#preview-toggle').hidden = false; document.querySelector('#close-preview').hidden = false;
      document.querySelector('#preview-toggle').textContent = 'After · B for Before'; api.renderCanvas();
    });
  }
  function toggle() {
    if (!api.state.preview) return;
    api.state.preview.before = !api.state.preview.before;
    document.querySelector('#preview-toggle').textContent = api.state.preview.before ? 'Before · B for After' : 'After · B for Before'; api.renderCanvas();
  }
  document.querySelector('#preview-toggle').onclick = toggle;
  document.querySelector('#close-preview').onclick = clearPreview;
  function summary(suggestion) {
    const slides = new Set(), edited = new Set(), details = [];
    for (const op of suggestion.ops) {
      const id = typeof op.slide === 'string' ? op.slide : op.id;
      const index = api.state.deck.slides.findIndex(s => s.id === id);
      if (index >= 0) slides.add(index + 1);
      if (op.op === 'update_block' || op.op === 'restyle') edited.add(op.id || op.target);
      else if (op.op === 'set_layout') details.push(`new layout ${op.layout}`);
      else details.push(op.op.replaceAll('_', ' '));
    }
    return [...(slides.size ? [`slide ${[...slides].join(', ')}`] : []), ...(edited.size ? [`edit ${edited.size} block${edited.size === 1 ? '' : 's'}`] : []), ...new Set(details)].join(' · ');
  }
  function render() {
    scope(); results.replaceChildren();
    const entries = (api.state.history || []).flatMap(h => h.suggestions);
    document.querySelector('#accept-all').disabled = !entries.some(e => e.status === 'pending');
    for (const suggestion of entries) {
      const card = element('article', `suggestion ${suggestion.status}`);
      card.tabIndex = 0;
      card.append(element('p', 'why', suggestion.why), element('p', 'hint', summary(suggestion)));
      if (suggestion.status === 'pending') {
        const actions = element('div', 'suggestion-actions');
        for (const action of ['Accept', 'Reject']) {
          const control = element('button', '', action);
          control.onclick = () => { clearPreview(); api.command(action === 'Accept' ? { command: action, suggestion_id: suggestion.id } : { command: action, id: suggestion.id }); };
          actions.append(control);
        }
        card.append(actions);
        card.onmouseenter = () => preview(suggestion);
        card.onfocusin = () => preview(suggestion);
      } else card.append(element('p', 'hint', suggestion.status === 'outdated' ? 'outdated — the slide changed' : suggestion.status));
      results.append(card);
    }
  }
  document.querySelector('#accept-all').onclick = () => { clearPreview(); api.command({ command: 'AcceptAll' }); };
  function event(event) {
    if (event.event === 'HostStatus') {
      connected = event.status.state === 'connected';
      host.className = event.status.state;
      host.textContent = `host · ${event.status.model || (connected ? 'default model' : event.status.state)}`;
      host.title = event.status.message;
      document.querySelector('#host-explanation').textContent = event.status.state === 'mismatch' ? event.status.message : '';
      document.querySelector('#start-host').hidden = !event.status.can_start;
    } else if (event.event === 'Suggesting') busy = true;
    else if (event.event === 'Suggestions') { busy = false; host.textContent = `host · ${event.model}`; }
    else if (event.event === 'Error') busy = false;
    controls();
  }
  controls();
  return { render, event, scope, clearPreview, toggle, focus: () => { tab(true); input.focus(); } };
}
