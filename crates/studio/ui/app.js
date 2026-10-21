import { canvasController } from './canvas.js';
import { stripController } from './strip.js';
import { panelController } from './panel.js';
import { feedbackController } from './feedback.js';
import { hydrateIcons } from './icons.js';
import { textBlock, id } from './helpers.js';
import { renderSlide } from './render.js';

hydrateIcons();
const $ = selector => document.querySelector(selector);
const state = { deck:null, revision:'', hashes:{}, images:{}, slideId:null, selected:new Set(), scopeSlide:false, preview:null, threads:[], linked:false, flash:[], mode:'edit', properties:false, comments:false, openThread:null, draftAnchor:null, drafts:{}, showResolved:false, zoom:1 };
const queue = []; let pending = null, toastTimer, focusTimer, lastFocus = '', flashTimer;
function focusChanged() {
  const key = JSON.stringify([state.slideId, [...state.selected]]);
  if (!state.linked || key === lastFocus) return;
  lastFocus = key; clearTimeout(focusTimer);
  focusTimer = setTimeout(() => command({ command:'Focus', slide:state.slideId, blocks:[...state.selected] }), 300);
}
function toast(text) { const node = $('#status'); node.textContent = text; node.hidden = !text; clearTimeout(toastTimer); toastTimer = setTimeout(() => node.hidden = true, 6000); }
function pump() {
  if (pending || !queue.length) return;
  pending = queue.shift();
  try { window.ipc.postMessage(JSON.stringify(pending.build())); }
  catch (error) { toast(error.message); pending = null; pump(); }
}
function command(build, complete) { queue.push({ build:typeof build === 'function' ? build : () => build, complete }); pump(); }
function apply(build) { command(() => ({ command:'Apply', ops:build() })); }
const api = {
  state, apply, command, toast, render,
  slide:() => state.deck?.slides.find(s => s.id === state.slideId),
  renderCanvas:() => { if (state.deck) { canvas.render(); panel.renderToolbar(); } },
  updateMany(slide, updates) { apply(() => updates.map(({ id, fields }) => ({ op:'update_block', slide, id, fields, base_hash:state.hashes[id] || '' }))); },
  selectionChanged() { canvas.decorate(); panel.render(); feedback.render(); focusChanged(); },
  duplicate:() => canvas.duplicate(), remove:() => canvas.remove(),
  clearPreview:() => feedback.clearPreview(),
  commentAt(anchor) { state.draftAnchor = anchor; state.openThread = null; state.selected.clear(); feedback.clearPreview(); render(); setTimeout(() => $('#thread-popover textarea')?.focus(), 0); },
};
const canvas = canvasController(api), strip = stripController(api), panel = panelController(api), feedback = feedbackController(api);
function render() {
  if (!state.deck) return;
  $('#properties-panel').hidden = !state.properties; $('#comments-panel').hidden = !state.comments;
  $('#workspace').classList.toggle('commenting', state.mode === 'comment');
  feedback.render(); canvas.render(); strip.render(); panel.render();
  $('#deck-title-label').textContent = state.deck.title;
  $('#dirty').textContent = 'Saved';
  $('#speaker-notes').value = api.slide()?.notes || ''; $('#speaker-notes').disabled = !api.slide();
  $('#slide-label').textContent = api.slide() ? `Slide ${String(state.deck.slides.indexOf(api.slide()) + 1).padStart(2, '0')} / ${String(state.deck.slides.length).padStart(2, '0')}  ·  ${api.slide().layout.replaceAll('-', ' ')}` : 'Your canvas';
  $('#slide-wrap').hidden = !api.slide(); $('#canvas-empty').hidden = Boolean(api.slide());
  $('#slide-wrap').style.transform = `scale(${state.zoom})`; $('#zoom-fit').textContent = state.zoom === 1 ? 'Fit' : `${Math.round(state.zoom * 100)}%`;
  for (const mode of ['edit','comment']) $(`#${mode}-mode`).setAttribute('aria-pressed', String(state.mode === mode));
  for (const node of document.querySelectorAll('#canvas .block')) node.classList.toggle('external-flash', state.flash.includes(node.dataset.id));
  const pill = $('#link-status'); pill.textContent = state.linked ? 'linked to nexus' : 'not linked'; pill.classList.toggle('connected', state.linked);
  pill.title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  focusChanged(); renderPresentation();
}
function handleEvent(event, reply) {
  const finished = reply ? pending : null; if (reply) pending = null;
  if (event.event === 'Deck') {
    feedback.clearPreview(); Object.assign(state, { deck:event.deck, revision:event.revision, hashes:event.hashes, images:event.images, threads:event.threads || [], linked:event.linked, flash:event.changed || [] });
    if (!api.slide()) state.slideId = state.deck.slides[0]?.id || null;
    const blocks = new Set(api.slide()?.blocks.map(b => b.id) || []); state.selected = new Set([...state.selected].filter(id => blocks.has(id)));
    if (state.openNewest) { state.openThread = state.threads.at(-1)?.id; state.openNewest = false; }
    render();
    if (event.notice) toast(event.notice);
    if (state.flash.length) { clearTimeout(flashTimer); flashTimer = setTimeout(() => { state.flash = []; document.querySelectorAll('.external-flash').forEach(n => n.classList.remove('external-flash')); }, 1800); }
  } else if (event.event === 'Focus') {
    if (state.deck.slides.some(s => s.id === event.slide)) { state.slideId = event.slide; state.selected.clear(); render(); }
  } else {
    feedback.event(event);
    if (event.event === 'Error') toast(event.message);
    if (event.event === 'Exported') toast('Deck JSON exported. Keep the assets folder beside it when moving the deck.');
  }
  finished?.complete?.(event); pump();
}
window.studioEvent = event => handleEvent(event, true);
window.studioAsyncEvent = event => handleEvent(event, false);
$('#undo').onclick = () => command({ command:'Undo' }); $('#redo').onclick = () => command({ command:'Redo' });
function deckMenu(open) { $('#deck-menu').hidden = !open; $('#deck-title').setAttribute('aria-expanded', String(open)); }
$('#deck-title').onclick = () => deckMenu($('#deck-menu').hidden);
document.addEventListener('pointerdown', event => { if (!event.target.closest('.deck-menu')) deckMenu(false); });
document.addEventListener('keydown', event => { if (event.key === 'Escape' && !$('#deck-menu').hidden) { deckMenu(false); $('#deck-title').focus(); } });
function properties() { state.properties = !state.properties; if (state.properties) { state.comments = false; feedback.closeThread(); } render(); }
$('#toggle-properties').onclick = properties; $('#close-properties').onclick = properties;
function comments() { state.comments = !state.comments; if (state.comments) state.properties = false; render(); }
$('#toggle-comments').onclick = comments; $('#close-comments').onclick = comments;
for (const mode of ['edit','comment']) $(`#${mode}-mode`).onclick = () => { state.mode = mode; state.selected.clear(); render(); };
function zoom(delta) { state.zoom = delta === 0 ? 1 : Math.max(.5, Math.min(2, state.zoom + delta)); render(); }
$('#zoom-in').onclick = () => zoom(.1); $('#zoom-out').onclick = () => zoom(-.1); $('#zoom-fit').onclick = () => zoom(0);
new ResizeObserver(() => panel.renderToolbar()).observe($('#slide-wrap'));
const pathDialog = $('#path-dialog'); let pathAction;
function ask(heading, label, hint, value, action) {
  $('#path-heading').textContent = heading; $('#path-label').textContent = label; $('#path-hint').textContent = hint; $('#path-confirm').textContent = heading; $('#path-input').value = value;
  pathAction = action; pathDialog.showModal(); $('#path-input').focus();
}
pathDialog.addEventListener('close', () => { if (pathDialog.returnValue === 'ok') pathAction($('#path-input').value); });
$('#rename-deck').onclick = () => { deckMenu(false); ask('Rename deck', 'Title', 'Give this project a name.', state.deck.title, value => { const title = value.trim(); if (title) command(() => ({ command:'Save', deck:{ ...state.deck, title } })); }); };
$('#open-deck').onclick = () => { deckMenu(false); ask('Open', 'Deck path', 'Enter a local .deck.json path.', '', path => command({ command:'Open', path })); };
$('#new-deck').onclick = () => { deckMenu(false); ask('New deck', 'Title', 'Created in your active space’s files/decks folder.', 'Untitled', title => command({ command:'NewDeck', title })); };
function insert(block) {
  const slide = api.slide(); if (!slide) { toast('Add a slide first.'); return; }
  state.selected = new Set([block.id]); state.mode = 'edit'; apply(() => [{ op:'insert_block', slide:slide.id, block }]); $('#insert-options').hidden = true;
}
$('#insert-toggle').onclick = () => $('#insert-options').hidden = !$('#insert-options').hidden;
$('#add-text').onclick = () => insert(textBlock());
$('#add-shape').onclick = () => insert({ id:id('b'), kind:'shape', x:8,y:5,w:10,h:7,shape:'rect',fill:'accent',stroke:'accent',style:{} });
$('#add-image').onclick = () => ask('Import image', 'Image path', 'PNG, JPEG or WebP. A sanitized copy stays with the deck.', '', path => command({ command:'ImportAsset', path }, event => { if (event.event === 'Asset') insert({ id:id('b'),kind:'image',x:4,y:3,w:20,h:12,src:event.src,fit:'contain',style:{} }); }));
$('#speaker-notes').onchange = event => { const slide = api.slide(), notes = event.target.value; if (slide) apply(() => [{ op:'set_notes', slide:slide.id, notes }]); };
$('#export').onclick = () => ask('Export deck', 'Destination path', 'Exports deck JSON. Keep the deck’s assets folder beside the exported file.', `${state.deck.title.replace(/[^\w-]+/g, '-')}.deck.json`, path => command({ command:'ExportDeck', path }));
let presenting = false, presentIndex = 0;
const presentation = $('#presentation');
function renderPresentation() {
  if (!presenting) return;
  presentIndex = Math.max(0, Math.min(state.deck.slides.length - 1, presentIndex));
  renderSlide($('#present-slide'), state.deck.slides[presentIndex], state.deck.theme, state.images); $('#present-counter').textContent = `${presentIndex + 1} / ${state.deck.slides.length}`;
}
function present() {
  if (!state.deck?.slides.length) return;
  presenting = true; presentIndex = Math.max(0, state.deck.slides.findIndex(s => s.id === state.slideId)); presentation.hidden = false; renderPresentation();
  const request = presentation.requestFullscreen || presentation.webkitRequestFullscreen; if (request) Promise.resolve(request.call(presentation)).catch(() => toast('Presenting in window.'));
}
function exitPresent() { presenting = false; presentation.hidden = true; if (document.fullscreenElement) document.exitFullscreen(); else if (document.webkitFullscreenElement) document.webkitExitFullscreen(); }
$('#present').onclick = present; $('#exit-present').onclick = exitPresent;
presentation.onclick = event => { if (event.target.id !== 'exit-present') { presentIndex++; renderPresentation(); } };
for (const name of ['fullscreenchange','webkitfullscreenchange']) document.addEventListener(name, () => { if (!document.fullscreenElement && !document.webkitFullscreenElement && presenting) { presenting = false; presentation.hidden = true; } });
document.addEventListener('keydown', event => {
  if (presenting) {
    if (event.key === 'Escape') exitPresent(); else if (['ArrowRight','ArrowDown',' '].includes(event.key)) { presentIndex++; renderPresentation(); } else if (['ArrowLeft','ArrowUp'].includes(event.key)) { presentIndex--; renderPresentation(); }
    event.preventDefault(); return;
  }
  const key = event.key.toLowerCase(), mod = event.metaKey || event.ctrlKey, editing = event.target.closest('input,textarea,select,[contenteditable]');
  if (mod && key === '\\') { event.preventDefault(); properties(); return; }
  if (mod && ['=','+','-','0'].includes(key)) { event.preventDefault(); zoom(key === '0' ? 0 : key === '-' ? -.1 : .1); return; }
  if (mod && key === 's') { event.preventDefault(); if (editing) event.target.blur(); toast('Every change is saved to the deck file as you make it.'); return; }
  if (editing || document.querySelector('dialog[open]')) return;
  if (key === 'b' && state.preview) { event.preventDefault(); feedback.togglePreview(); }
  else if (event.key === 'Escape') { feedback.clearPreview(); state.openThread = null; state.draftAnchor = null; state.selected.clear(); render(); }
  else if (key === 'c') { state.mode = state.mode === 'comment' ? 'edit' : 'comment'; state.selected.clear(); render(); }
  else if (mod && key === 'z') { event.preventDefault(); command({ command:event.shiftKey ? 'Redo' : 'Undo' }); }
  else if (mod && key === 'd') { event.preventDefault(); canvas.duplicate(); }
  else if (['Delete','Backspace'].includes(event.key)) { event.preventDefault(); canvas.remove(); }
  else if (event.key.startsWith('Arrow')) { event.preventDefault(); const step = event.shiftKey ? 4 : 1; canvas.nudge(event.key === 'ArrowRight' ? step : event.key === 'ArrowLeft' ? -step : 0, event.key === 'ArrowDown' ? step : event.key === 'ArrowUp' ? -step : 0); }
  else if (event.key === 'F5') { event.preventDefault(); present(); }
});
command({ command:'Load' });
