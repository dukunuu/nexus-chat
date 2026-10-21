import { canvasController } from './canvas.js';
import { stripController } from './strip.js';
import { panelController } from './panel.js';
import { suggestController } from './suggest.js';
import { textBlock, id } from './helpers.js';
import { renderSlide } from './render.js';

const state = { deck: null, revision: '', hashes: {}, images: {}, dirty: false, slideId: null, selected: new Set(), scopeSlide: false, history: [], preview: null };
const status = document.querySelector('#status');
const queue = [];
let pending = null;
function pump() {
  if (pending || !queue.length) return;
  pending = queue.shift();
  try { window.ipc.postMessage(JSON.stringify(pending.build())); }
  catch (error) { status.textContent = error.message; pending = null; pump(); }
}
function command(build, complete) { queue.push({ build: typeof build === 'function' ? build : () => build, complete }); pump(); }
function apply(build) { command(() => ({ command: 'Apply', ops: build() })); }
const api = {
  state, apply, command,
  renderCanvas: () => { if (state.deck) canvas.render(); },
  slide: () => state.deck?.slides.find(s => s.id === state.slideId),
  updateMany(slide, updates) {
    apply(() => updates.map(({ id, fields }) => ({ op: 'update_block', slide, id, fields, base_hash: state.hashes[id] || '' })));
  },
  selectionChanged() { canvas.decorate(); panel.render(); suggestions.scope(); },
  render,
};
const canvas = canvasController(api), strip = stripController(api), panel = panelController(api), suggestions = suggestController(api);
function render() {
  if (!state.deck) return;
  canvas.render(); strip.render(); panel.render(); suggestions.render();
  document.querySelector('#deck-title').textContent = state.deck.title;
  document.querySelector('#dirty').textContent = state.dirty ? '· unsaved' : '· saved';
  const notes = document.querySelector('#speaker-notes');
  notes.value = api.slide()?.notes || ''; notes.disabled = !api.slide();
  renderPresentation();
}
function handleEvent(event, reply) {
  const finished = reply ? pending : null;
  if (reply) pending = null;
  suggestions.event(event);
  if (event.event === 'Deck') {
    suggestions.clearPreview();
    Object.assign(state, { deck: event.deck, revision: event.revision, hashes: event.hashes, images: event.images, dirty: event.dirty, history: event.history });
    if (!api.slide()) state.slideId = state.deck.slides[0]?.id || null;
    const blocks = new Set(api.slide()?.blocks.map(b => b.id) || []);
    state.selected = new Set([...state.selected].filter(id => blocks.has(id)));
    status.textContent = 'Ready'; render();
  } else if (event.event === 'Suggestions') {
    state.history = event.history; suggestions.render(); status.textContent = event.notes.join('; ') || 'Suggestions ready';
  } else if (event.event === 'Error') status.textContent = event.message;
  finished?.complete?.(event);
  pump();
}
window.studioEvent = event => handleEvent(event, true);
window.studioAsyncEvent = event => handleEvent(event, false);
function save() { command(() => ({ command: 'Save', deck: state.deck })); }
document.querySelector('#save').onclick = save;
document.querySelector('#undo').onclick = () => command({ command: 'Undo' });
document.querySelector('#redo').onclick = () => command({ command: 'Redo' });
const pathDialog = document.querySelector('#path-dialog');
let pathAction;
function ask(heading, label, hint, value, action) {
  document.querySelector('#path-heading').textContent = heading;
  document.querySelector('#path-label').textContent = label;
  document.querySelector('#path-hint').textContent = hint;
  document.querySelector('#path-confirm').textContent = heading;
  document.querySelector('#path-input').value = value;
  pathAction = action; pathDialog.showModal(); document.querySelector('#path-input').focus();
}
pathDialog.addEventListener('close', () => { if (pathDialog.returnValue === 'ok') pathAction(document.querySelector('#path-input').value); });
const canLeave = () => !state.dirty || window.confirm('Discard unsaved changes? Save first to keep them.');
document.querySelector('#open-deck').onclick = () => {
  if (canLeave()) ask('Open', 'Deck path', 'Enter a local .deck.json path.', '', path => command({ command: 'Open', path }));
};
document.querySelector('#new-deck').onclick = () => {
  if (canLeave()) ask('New deck', 'Title', 'Created in your active space’s files/decks folder.', 'Untitled', title => command({ command: 'NewDeck', title }));
};
function insert(block) {
  const slide = api.slide();
  if (!slide) { status.textContent = 'Add a slide first.'; return; }
  state.selected = new Set([block.id]);
  apply(() => [{ op: 'insert_block', slide: slide.id, block }]);
}
document.querySelector('#add-text').onclick = () => insert(textBlock());
document.querySelector('#add-shape').onclick = () => insert({ id: id('b'), kind: 'shape', x: 8, y: 5, w: 10, h: 7, shape: 'rect', fill: 'accent', stroke: 'accent', style: {} });
document.querySelector('#add-image').onclick = () => {
  if (!api.slide()) { status.textContent = 'Add a slide first.'; return; }
  const slide = api.slide().id;
  ask('Import image', 'Image path', 'PNG, JPEG or WebP. A sanitized copy stays in the deck’s assets folder.', '', path => {
    command({ command: 'ImportAsset', path }, event => {
      if (event.event !== 'Asset') return;
      const block = { id: id('b'), kind: 'image', x: 4, y: 3, w: 20, h: 12, src: event.src, fit: 'contain', style: {} };
      state.selected = new Set([block.id]);
      apply(() => [{ op: 'insert_block', slide, block }]);
    });
  });
};
document.querySelector('#speaker-notes').onchange = event => {
  const slide = api.slide(); const notes = event.target.value;
  if (slide) apply(() => [{ op: 'set_notes', slide: slide.id, notes }]);
};
let presenting = false, presentIndex = 0;
const presentation = document.querySelector('#presentation');
function renderPresentation() {
  if (!presenting) return;
  presentIndex = Math.max(0, Math.min(state.deck.slides.length - 1, presentIndex));
  renderSlide(document.querySelector('#present-slide'), state.deck.slides[presentIndex], state.deck.theme, state.images);
  document.querySelector('#present-counter').textContent = `${state.deck.slides.length ? presentIndex + 1 : 0} / ${state.deck.slides.length}`;
}
function present() {
  if (!state.deck?.slides.length) return;
  presenting = true; presentIndex = Math.max(0, state.deck.slides.findIndex(s => s.id === state.slideId));
  presentation.hidden = false; renderPresentation();
  const request = presentation.requestFullscreen || presentation.webkitRequestFullscreen;
  if (request) Promise.resolve(request.call(presentation)).catch(() => { status.textContent = 'Presenting in window; system full-screen is unavailable.'; });
}
function exitPresent() {
  presenting = false; presentation.hidden = true;
  if (document.fullscreenElement) document.exitFullscreen();
  else if (document.webkitFullscreenElement) document.webkitExitFullscreen();
}
document.querySelector('#present').onclick = present;
document.querySelector('#exit-present').onclick = exitPresent;
presentation.onclick = event => { if (event.target.id !== 'exit-present') { presentIndex++; renderPresentation(); } };
for (const name of ['fullscreenchange', 'webkitfullscreenchange']) document.addEventListener(name, () => {
  if (!document.fullscreenElement && !document.webkitFullscreenElement && presenting) { presenting = false; presentation.hidden = true; }
});
document.addEventListener('keydown', event => {
  if (presenting) {
    if (event.key === 'Escape') exitPresent();
    else if (['ArrowRight', 'ArrowDown', ' '].includes(event.key)) { presentIndex++; renderPresentation(); }
    else if (['ArrowLeft', 'ArrowUp'].includes(event.key)) { presentIndex--; renderPresentation(); }
    event.preventDefault(); return;
  }
  const editing = event.target.closest('input, textarea, select, [contenteditable]');
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
    event.preventDefault(); suggestions.focus(); return;
  }
  if (!editing && event.key.toLowerCase() === 'b' && state.preview) {
    event.preventDefault(); suggestions.toggle(); return;
  }
  if (!editing && event.key === 'Escape') {
    suggestions.clearPreview(); state.selected.clear(); state.scopeSlide = false; api.selectionChanged(); return;
  }
  if (event.key === 's' && (event.metaKey || event.ctrlKey)) {
    event.preventDefault(); if (editing) event.target.blur(); save(); return;
  }
  if (editing || document.querySelector('dialog[open]')) return;
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'z') {
    event.preventDefault(); command({ command: event.shiftKey ? 'Redo' : 'Undo' });
  } else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'd') {
    event.preventDefault(); canvas.duplicate();
  } else if (event.key === 'Delete' || event.key === 'Backspace') {
    event.preventDefault(); canvas.remove();
  } else if (event.key.startsWith('Arrow')) {
    event.preventDefault(); const step = event.shiftKey ? 4 : 1;
    canvas.nudge(event.key === 'ArrowRight' ? step : event.key === 'ArrowLeft' ? -step : 0, event.key === 'ArrowDown' ? step : event.key === 'ArrowUp' ? -step : 0);
  } else if (event.key === 'F5') { event.preventDefault(); present(); }
});
command({ command: 'Load' });
command({ command: 'HostStatus' });
