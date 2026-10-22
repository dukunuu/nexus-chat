import { canvasController } from './canvas.js';
import { stripController } from './strip.js';
import { panelController } from './panel.js';
import { feedbackController } from './feedback.js';
import { editingController } from './editing.js';
import { hydrateIcons } from './icons.js';
import { textNode, id, allNodes, slideSize } from './helpers.js';
import { playbackController } from './playback.js';
import { animatePanelController } from './animate-panel.js';

hydrateIcons();
const $ = selector => document.querySelector(selector);
const state = { deck:null, revision:'', hashes:{}, images:{}, slideId:null, selected:new Set(), scopeSlide:false, preview:null, threads:[], activities:{}, linked:false, flash:[], mode:'edit', panelTab:'design', playStep:0, properties:false, comments:false, openThread:null, draftAnchor:null, drafts:{}, showResolved:false, zoom:null, panX:0, panY:0, spacePan:false };
const queue = []; let pending = null, toastTimer, focusTimer, lastFocus = '', flashTimer;
function applyTheme(colors) {
  const roles = { bg:'bg', surface:'bg', raised:'raised', control:'control', hairline:'border_dim', text:'fg', muted:'fg_dim', faint:'fg_dim', accent:'accent', accent2:'accent2', 'accent-soft':'selection', success:'success', warning:'warning', danger:'error', canvas:'bg', 'on-accent':'bg', code:'raised', present:'bg' };
  for (const [token, role] of Object.entries(roles)) if (/^#[0-9a-f]{6}$/i.test(colors[role] || '')) document.documentElement.style.setProperty(`--${token}`, colors[role]);
  const bg = colors.bg?.match(/^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i);
  if (bg) document.documentElement.style.colorScheme = parseInt(bg[1], 16) * .2126 + parseInt(bg[2], 16) * .7152 + parseInt(bg[3], 16) * .0722 > 140 ? 'light' : 'dark';
}
function focusChanged() {
  const key = JSON.stringify([state.slideId, [...state.selected]]);
  if (!state.linked || key === lastFocus) return;
  lastFocus = key; clearTimeout(focusTimer);
  focusTimer = setTimeout(() => command({ command:'Focus', slide:state.slideId, blocks:[...state.selected] }), 300);
}
function toast(text, view) {
  const node = $('#status'); node.textContent = text; node.hidden = !text;
  if (view) { const action = document.createElement('button'); action.textContent = 'View'; action.onclick = () => { node.hidden = true; view(); }; node.append(document.createTextNode(' · '), action); }
  clearTimeout(toastTimer); toastTimer = setTimeout(() => node.hidden = true, 6000);
}
function pump() {
  if (pending || !queue.length) return;
  pending = queue.shift();
  try { window.ipc.postMessage(JSON.stringify(pending.build())); }
  catch (error) { toast(error.message); pending = null; pump(); }
}
function command(build, complete) { queue.push({ build:typeof build === 'function' ? build : () => build, complete }); pump(); }
let gesture;
function apply(build) {
  if (gesture) {
    const ops = build(); gesture.ops = ops;
    state.deck = structuredClone(gesture.deck);
    const merge = (target, patch) => { for (const [key, value] of Object.entries(patch)) { if (value === null) delete target[key]; else if (value && typeof value === 'object' && !Array.isArray(value)) merge(target[key] ||= {}, value); else target[key] = value; } };
    for (const op of ops) {
      const target = op.op === 'update_slide' ? state.deck.slides.find(s => s.id === op.id) : state.deck.slides.flatMap(allNodes).find(n => n.id === op.id);
      if (target && op.fields) merge(target, op.fields);
      if (op.op === 'set_transition') state.deck.slides.find(s => s.id === op.slide).transition = op.transition;
      if (op.op === 'set_animation' && target) { target.animation ||= {}; if (op.build) target.animation[op.slot] = op.build; else delete target.animation[op.slot]; }
      if (op.op === 'reorder_builds') state.deck.slides.find(s => s.id === op.slide).builds = op.ids;
    }
    api.renderCanvas(); return;
  }
  command(() => ({ command:'Apply', ops:build() }));
}
const api = {
  state, apply, command, toast, render, ask,
  beginGesture() { state.scrubbing = true; gesture = { deck:structuredClone(state.deck), ops:[] }; },
  endGesture(cancel = false) {
    if (!gesture) return;
    const finished = gesture; gesture = null; state.scrubbing = false; state.deck = finished.deck;
    if (!cancel && finished.ops.length) apply(() => finished.ops); else render();
  },
  wrap:unwrap => editing.wrap(unwrap),
  align:action => editing.align(action),
  alignRow:container => editing.alignRow(container),
  slide:() => state.deck?.slides.find(s => s.id === state.slideId),
  renderCanvas:() => { if (state.deck) { canvas.render(); panel.renderToolbar(); animationPanel.badges(); } },
  renderAnimations:container => animationPanel.render(container),
  updateMany(slide, updates) { apply(() => updates.map(({ id, fields }) => ({ op:'update_node', id, fields, base_hash:state.hashes[id] || '' }))); },
  rectOf:nodeId => canvas.rectOf(nodeId),
  selectionChanged() { canvas.decorate(); panel.render(true); feedback.render(); animationPanel.badges(); focusChanged(); },
  duplicate:() => canvas.duplicate(), remove:() => canvas.remove(),
  clearPreview:() => feedback.clearPreview(),
  commentAt(anchor) { state.draftAnchor = anchor; state.openThread = null; state.selected.clear(); feedback.clearPreview(); render(); setTimeout(() => $('#thread-popover textarea')?.focus(), 0); },
};
const canvas = canvasController(api), strip = stripController(api), panel = panelController(api), feedback = feedbackController(api), editing = editingController(api);
const animationPanel = animatePanelController(api), playback = playbackController(api);
for (const tab of ['design','animate']) $(`#tab-${tab}`).onclick = () => { state.panelTab = tab; render(); };
function render() {
  if (!state.deck) return;
  $('#properties-panel').hidden = !state.properties; $('#comments-panel').hidden = !state.comments;
  $('#workspace').classList.toggle('commenting', state.mode === 'comment');
  feedback.render(); canvas.render(); strip.render(); panel.render();
  $('#deck-title-label').textContent = state.deck.title;
  $('#dirty').textContent = 'Saved';
  $('#speaker-notes').value = api.slide()?.notes || ''; $('#speaker-notes').disabled = !api.slide();
  $('#slide-label').textContent = api.slide() ? `Slide ${String(state.deck.slides.indexOf(api.slide()) + 1).padStart(2, '0')} / ${String(state.deck.slides.length).padStart(2, '0')}${api.slide().name ? `  ·  ${api.slide().name}` : ''}` : 'Your canvas';
  $('#slide-wrap').hidden = !api.slide(); $('#canvas-empty').hidden = Boolean(api.slide());
  applyViewport();
  for (const mode of ['edit','comment']) $(`#${mode}-mode`).setAttribute('aria-pressed', String(state.mode === mode));
  for (const node of document.querySelectorAll('#canvas .block')) node.classList.toggle('external-flash', state.flash.includes(node.dataset.id));
  feedback.renderLink();
  $('#link-status').title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  for (const tab of ['design','animate']) $(`#tab-${tab}`).setAttribute('aria-selected',String(state.panelTab === tab));
  feedback.renderPins(); animationPanel.badges(); focusChanged(); playback.refresh();
}
function handleEvent(event, reply) {
  const finished = reply ? pending : null; if (reply) pending = null;
  if (event.event === 'Deck') {
    if (gesture) api.endGesture(true);
    if (event.colors) applyTheme(event.colors);
    feedback.clearPreview(); Object.assign(state, { deck:event.deck, revision:event.revision, hashes:event.hashes, images:event.images, threads:event.threads || [], linked:event.linked, flash:event.changed || [] });
    if (!api.slide()) state.slideId = state.deck.slides[0]?.id || null;
    const known = new Set(allNodes(api.slide()).map(n => n.id)); state.selected = new Set([...state.selected].filter(id => known.has(id)));
    if (state.openNewest) { state.openThread = state.threads.at(-1)?.id; state.openNewest = false; }
    if (event.notice) toast(event.notice);
    feedback.updateActivities(event.activities);
    render();
    if (state.flash.length) { clearTimeout(flashTimer); flashTimer = setTimeout(() => { state.flash = []; document.querySelectorAll('.external-flash').forEach(n => n.classList.remove('external-flash')); }, 1800); }
  } else if (event.event === 'Theme') {
    applyTheme(event.colors);
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
// A slide is 1920 px wide natively (the theme's size). Fit is responsive;
// numeric zoom is screen pixels per native slide pixel, not a multiple of fit.
let nativeWidth = 1920; const stage = $('#stage'), wrap = $('#slide-wrap');
function applyViewport() {
  nativeWidth = state.deck ? slideSize(state.deck.theme).w : 1920;
  const scale = state.zoom === null ? 1 : state.zoom * nativeWidth / Math.max(1, parseFloat(getComputedStyle(wrap).width));
  wrap.style.transform = `translate(${state.panX}px,${state.panY}px) scale(${scale})`;
  $('#zoom-fit').textContent = state.zoom === null ? 'Fit' : `${Math.round(state.zoom * 100)}%`;
  animationPanel.badges();
}
function zoomTo(value, point) {
  const bounds = wrap.getBoundingClientRect();
  if (!bounds.width) return;
  const next = Math.max(.05, Math.min(8, value)), ratio = next / (bounds.width / nativeWidth);
  if (point) {
    state.panX += (point.x - bounds.left - bounds.width / 2) * (1 - ratio);
    state.panY += (point.y - bounds.top - bounds.height / 2) * (1 - ratio);
  }
  state.zoom = next; applyViewport(); feedback.renderPins();
}
function zoom(delta) {
  if (delta === 0) { state.zoom = null; state.panX = 0; state.panY = 0; applyViewport(); feedback.renderPins(); }
  else zoomTo(wrap.getBoundingClientRect().width / nativeWidth + delta);
}
$('#zoom-in').onclick = () => zoom(.1); $('#zoom-out').onclick = () => zoom(-.1); $('#zoom-fit').onclick = () => zoom(0);
new ResizeObserver(() => { applyViewport(); panel.renderToolbar(); feedback.renderPins(); }).observe(wrap);
stage.addEventListener('wheel', event => {
  event.preventDefault();
  const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? stage.clientHeight : 1;
  if (event.ctrlKey || event.metaKey) zoomTo(wrap.getBoundingClientRect().width / nativeWidth * Math.exp(-event.deltaY * unit * .01), { x:event.clientX, y:event.clientY });
  else { state.panX -= event.deltaX * unit; state.panY -= event.deltaY * unit; applyViewport(); }
}, { passive:false });
let pan, gestureZoom;
stage.addEventListener('pointerdown', event => {
  if (!(state.spacePan && event.button === 0) && event.button !== 1) return;
  event.preventDefault(); event.stopImmediatePropagation();
  pan = { id:event.pointerId, x:event.clientX, y:event.clientY, panX:state.panX, panY:state.panY };
  stage.setPointerCapture(event.pointerId); stage.classList.add('panning');
}, true);
stage.addEventListener('pointermove', event => {
  if (!pan) return;
  state.panX = pan.panX + event.clientX - pan.x; state.panY = pan.panY + event.clientY - pan.y; applyViewport();
});
function finishPan() {
  if (pan && stage.hasPointerCapture(pan.id)) stage.releasePointerCapture(pan.id);
  pan = null; stage.classList.remove('panning');
}
for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) stage.addEventListener(name, finishPan);
function releaseSpace() { state.spacePan = false; stage.classList.remove('pan-ready'); }
document.addEventListener('keyup', event => { if (event.code === 'Space') releaseSpace(); });
window.addEventListener('blur', () => { releaseSpace(); finishPan(); });
// WKWebView reports native pinch gestures here; Chromium uses ctrl+wheel.
stage.addEventListener('gesturestart', event => { event.preventDefault(); gestureZoom = wrap.getBoundingClientRect().width / nativeWidth; }, { passive:false });
stage.addEventListener('gesturechange', event => { event.preventDefault(); zoomTo(gestureZoom * event.scale, { x:event.clientX, y:event.clientY }); }, { passive:false });
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
  state.selected = new Set([block.id]); state.mode = 'edit'; apply(() => [{ op:'insert_node', parent:slide.id, node:block }]); $('#insert-options').hidden = true;
}
$('#insert-toggle').onclick = () => $('#insert-options').hidden = !$('#insert-options').hidden;
$('#add-text').onclick = () => insert(textNode('Double-click to edit', { position:{ x:160, y:160 }, sizing:{ w:960 } }));
$('#add-shape').onclick = () => insert({ id:id('b'), kind:'shape', shape:'rect', fill:'accent', position:{ x:480, y:300 }, sizing:{ w:600, h:420 } });
$('#add-image').onclick = () => ask('Import image', 'Image path', 'PNG, JPEG or WebP. A sanitized copy stays with the deck.', '', path => command({ command:'ImportAsset', path }, event => { if (event.event === 'Asset') insert({ id:id('b'), kind:'image', src:event.src, fit:'contain', position:{ x:240, y:180 }, sizing:{ w:1200, h:720 } }); }));
$('#speaker-notes').onchange = event => { const slide = api.slide(), notes = event.target.value; if (slide) apply(() => [{ op:'set_notes', slide:slide.id, notes }]); };
$('#export').onclick = () => ask('Export deck', 'Destination path', 'Exports deck JSON. Keep the deck’s assets folder beside the exported file.', `${state.deck.title.replace(/[^\w-]+/g, '-')}.deck.json`, path => command({ command:'ExportDeck', path }));
document.addEventListener('keydown', event => {
  if (playback.key(event)) return;
  const key = event.key.toLowerCase(), mod = event.metaKey || event.ctrlKey, editingField = event.target.closest('input,textarea,select,[contenteditable]');
  if (mod && key === '\\') { event.preventDefault(); properties(); return; }
  if (mod && ['=','+','-','0'].includes(key)) { event.preventDefault(); zoom(key === '0' ? 0 : key === '-' ? -.1 : .1); return; }
  if (mod && key === 's') { event.preventDefault(); if (editingField) event.target.blur(); toast('Every change is saved to the deck file as you make it.'); return; }
  if (state.scrubbing && event.key === 'Escape') { event.preventDefault(); api.endGesture(true); return; }
  if (editingField || document.querySelector('dialog[open]')) return;
  if (event.code === 'Space') { event.preventDefault(); state.spacePan = true; stage.classList.add('pan-ready'); return; }
  if (event.shiftKey && event.code === 'Digit1') { event.preventDefault(); zoom(0); return; }
  if (event.shiftKey && event.code === 'Digit0') { event.preventDefault(); zoomTo(1); return; }
  if (event.shiftKey && event.code === 'KeyA') { event.preventDefault(); editing.wrap(event.altKey); return; }
  if (event.altKey && !mod) {
    const action = { KeyA:'left', KeyD:'right', KeyW:'top', KeyS:'bottom', KeyH:'h-center', KeyV:'v-center' }[event.code];
    if (action) { event.preventDefault(); editing.align(action); return; }
  }
  if (key === 'b' && state.preview) { event.preventDefault(); feedback.togglePreview(); }
  else if (event.key === 'Escape') { feedback.clearPreview(); state.openThread = null; state.draftAnchor = null; state.selected.clear(); render(); }
  else if (key === 'c') { state.mode = state.mode === 'comment' ? 'edit' : 'comment'; state.selected.clear(); render(); }
  else if (mod && key === 'z') { event.preventDefault(); command({ command:event.shiftKey ? 'Redo' : 'Undo' }); }
  else if (mod && key === 'd') { event.preventDefault(); canvas.duplicate(); }
  else if (['Delete','Backspace'].includes(event.key)) { event.preventDefault(); canvas.remove(); }
  else if (event.key.startsWith('Arrow')) { event.preventDefault(); const step = event.shiftKey ? 10 : 1; canvas.nudge(event.key === 'ArrowRight' ? step : event.key === 'ArrowLeft' ? -step : 0, event.key === 'ArrowDown' ? step : event.key === 'ArrowUp' ? -step : 0); }
  else if (event.key === 'F5' || (event.shiftKey && event.key === 'Enter')) { event.preventDefault(); playback.start(event.key === 'Enter'); }
});
command({ command:'Load' });
