import { chooseTemplate, templateDrop } from './templates.js';
import { canvasController } from './canvas.js';
import { stripController } from './strip.js';
import { panelController } from './panel.js';
import { feedbackController } from './feedback.js';
import { editingController } from './editing.js';
import { hydrateIcons } from './icons.js';
import { textNode, id, allNodes, slideSize, nodeLabel, parentOf } from './helpers.js';
import { applyLocal, signature, slideAssets } from './local.js';
import { playbackController } from './playback.js';
import { animatePanelController } from './animate-panel.js';
import { libraryController } from './library.js';

hydrateIcons();
const $ = selector => document.querySelector(selector);
const state = { deck:null, revision:'', hashes:{}, images:{}, slideId:null, selected:new Set(), scopeSlide:false, preview:null, threads:[], activities:{}, linked:false, flash:[], mode:'edit', panelTab:'design', playStep:0, side:null, openThread:null, draftAnchor:null, drafts:{}, showResolved:false, zoom:null, panX:0, panY:0, spacePan:false };
// One right sidebar: properties, comments, history or design. The two older
// flags read and write it, so the panel modules keep asking the same question.
for (const tab of ['properties', 'comments']) Object.defineProperty(state, tab, { get:() => state.side === tab, set:on => { if (on) state.side = tab; else if (state.side === tab) state.side = null; } });
try { state.leftHidden = localStorage.getItem('nexus-deck-left') === 'hidden'; } catch { state.leftHidden = false; }
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
function toast(text, view, actions = []) {
  // The library covers the window; its notices go above it.
  if (library.visible) { library.toast(text, [...(view ? [['View', view]] : []), ...actions]); return; }
  const node = $('#status'); node.textContent = text; node.hidden = !text;
  if (view) { const action = document.createElement('button'); action.textContent = 'View'; action.onclick = () => { node.hidden = true; view(); }; node.append(document.createTextNode(' · '), action); }
  for (const [label, run] of actions) { const action = document.createElement('button'); action.textContent = label; action.onclick = run; node.append(document.createTextNode(' · '), action); }
  clearTimeout(toastTimer); toastTimer = setTimeout(() => node.hidden = true, actions.length ? 12000 : 6000);
}
function pump() {
  if (pending || !queue.length) return;
  pending = queue.shift();
  try { window.ipc.postMessage(JSON.stringify(pending.build())); }
  catch (error) { rollback(); toast(error.message); pending = null; pump(); }
}
function command(build, complete) { queue.push({ build:typeof build === 'function' ? build : () => build, complete, barrier:typeof build === 'function' || !['Focus','ListFonts','Load'].includes(build.command) }); pump(); }
// The library sends through whichever mode is live (doc mode has its own queue).
const library = libraryController({ command:(cmd, complete) => (docUi ? docUi.api.command(cmd, complete) : command(cmd, complete)), toast });
let gesture, authoritative;
function rollback() {
  gesture = null; state.scrubbing = false;
  // Unsent edits may depend on the rejected one. Do not send those previews.
  for (let i = queue.length - 1; i >= 0; i--) if (queue[i].ops) queue.splice(i, 1);
  if (pending?.ops) delete pending.ops;
  if (authoritative) { state.deck = authoritative; renderDeck(); }
}
function apply(build) {
  if (gesture) {
    const ops = build(); gesture.ops = ops;
    state.deck = applyLocal(gesture.deck, ops) || gesture.deck;
    paintCanvas(); return;
  }
  const ops = build();
  const next = ![pending, ...queue].some(item => item?.barrier) && applyLocal(state.deck, ops);
  const entry = { ops:next ? ops : null, barrier:!next, build:() => ({ command:'Apply', ops:(next ? ops : build()).map(op => op.op === 'update_node' ? { ...op, base_hash:state.hashes[op.id] || '' } : op) }) };
  if (next) { state.deck = next; paintCanvas(); }
  queue.push(entry); pump();
}
const api = {
  state, apply, command, toast, render, ask,
  beginGesture() { state.scrubbing = true; gesture = { deck:structuredClone(state.deck), ops:[] }; panel.positionToolbar(); },
  endGesture(cancel = false) {
    if (!gesture) return;
    const finished = gesture; gesture = null; state.scrubbing = false; state.deck = finished.deck;
    if (!cancel && finished.ops.length) apply(() => finished.ops); else renderDeck();
  },
  wrap:unwrap => editing.wrap(unwrap),
  align:action => editing.align(action),
  alignRow:container => editing.alignRow(container),
  slide:() => state.deck?.slides.find(s => s.id === state.slideId),
  renderCanvas:() => { if (state.deck) { paintCanvas(); panel.renderToolbar(); animationPanel.badges(); } },
  renderAnimations:container => animationPanel.render(container),
  updateMany(slide, updates) { apply(() => updates.map(({ id, fields }) => ({ op:'update_node', id, fields, base_hash:state.hashes[id] || '' }))); },
  rectOf:nodeId => canvas.rectOf(nodeId),
  positionToolbar:() => panel.positionToolbar(),
  selectionChanged() { canvas.decorate(); panel.render(true); panelKey = panelSignatures().props; feedback.render(); feedbackKey = feedbackSignature(); animationPanel.badges(); focusChanged(); },
  duplicate:() => canvas.duplicate(), remove:() => canvas.remove(),
  clearPreview:() => feedback.clearPreview(),
  commentAt(anchor) { state.draftAnchor = anchor; state.openThread = null; state.selected.clear(); feedback.clearPreview(); render(); setTimeout(() => $('#thread-popover textarea')?.focus(), 0); },
};
const canvas = canvasController(api), strip = stripController(api), panel = panelController(api), feedback = feedbackController(api), editing = editingController(api);
const animationPanel = animatePanelController(api), playback = playbackController(api);
for (const tab of ['design','animate']) $(`#tab-${tab}`).onclick = () => { state.panelTab = tab; render(); };
let canvasKey, layersKey, panelKey, feedbackKey, ancillaryFrame, deckActivities;
const currentCanvasKey = () => signature([api.slide(), state.deck.theme, slideAssets(api.slide(), state.deck.theme, state.images), state.preview]);
function paintCanvas() { canvas.render(); canvasKey = currentCanvasKey(); panel.positionToolbar(); }
function deferAncillary() {
  if (ancillaryFrame) return;
  // The first frame paints the canvas; ancillary DOM work runs after it.
  ancillaryFrame = requestAnimationFrame(() => { ancillaryFrame = requestAnimationFrame(() => { ancillaryFrame = null; renderAncillary(); }); });
}
function renderDeck() {
  if (canvasKey !== currentCanvasKey()) paintCanvas();
  deferAncillary();
}
const feedbackSignature = () => signature([state.threads, state.activities, state.slideId, state.openThread, state.draftAnchor, state.linked]);
function panelSignatures() {
  const slide = api.slide();
  const tree = nodes => nodes.map(n => [n.id, n.kind, nodeLabel(n), Boolean(n.hidden), Boolean(n.locked), tree(n.children || [])]);
  const layers = signature([state.slideId, tree(slide?.children || [])]);
  const selected = allNodes(slide).filter(n => state.selected.has(n.id));
  const ancestors = selected.map(n => { const out = []; for (let p = parentOf(slide, n.id)?.parent; p && p !== slide; p = parentOf(slide, p.id)?.parent) out.push({ ...p, children:undefined }); return out; });
  const props = signature([state.slideId, [...state.selected], selected, selected.map(n => api.rectOf(n.id)), ancestors, slide && { ...slide, children:undefined }, state.deck.theme, state.panelTab, state.properties, state.mode, state.preview]);
  return { layers, props };
}
function renderAncillary() {
  if (deckActivities) { feedback.updateActivities(deckActivities); deckActivities = null; }
  const { layers, props } = panelSignatures();
  if (layers !== layersKey || props !== panelKey) { panel.render(layers === layersKey); layersKey = layers; panelKey = props; }
  else panel.renderToolbar();
  strip.render();
  const comments = feedbackSignature();
  if (comments !== feedbackKey) { feedback.render(); feedbackKey = comments; }
  renderLabels(); animationPanel.badges(); focusChanged(); playback.refresh();
}
function renderLabels() {
  $('#deck-title-label').textContent = state.deck.title;
  $('#link-status').title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  const saving = Boolean(pending?.ops || queue.some(item => item.ops));
  $('#dirty').textContent = saving ? 'Saving…' : 'Saved'; $('#save').classList.toggle('busy', saving);
  renderStatusDot(saving);
  $('#speaker-notes').value = api.slide()?.notes || ''; $('#speaker-notes').disabled = !api.slide();
  $('#slide-label').textContent = api.slide() ? `Slide ${state.deck.slides.indexOf(api.slide()) + 1} of ${state.deck.slides.length}` : '';
  $('#slide-wrap').hidden = !api.slide(); $('#canvas-empty').hidden = Boolean(api.slide());
}
function render() {
  if (!state.deck) return;
  renderSidebar();
  $('#workspace').classList.toggle('commenting', state.mode === 'comment');
  if (deckActivities) { feedback.updateActivities(deckActivities); deckActivities = null; }
  feedback.render(); paintCanvas(); strip.render(); panel.render();
  ({ layers:layersKey, props:panelKey } = panelSignatures()); feedbackKey = feedbackSignature();
  renderLabels();
  applyViewport();
  for (const mode of ['edit','comment']) $(`#${mode}-mode`).setAttribute('aria-pressed', String(state.mode === mode));
  for (const node of document.querySelectorAll('#canvas .block')) node.classList.toggle('external-flash', state.flash.includes(node.dataset.id));
  feedback.renderLink();
  $('#link-status').title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  for (const tab of ['design','animate']) $(`#tab-${tab}`).setAttribute('aria-selected',String(state.panelTab === tab));
  feedback.renderPins(); animationPanel.badges(); focusChanged(); playback.refresh();
  renderSidebar();
}
function showDetails(title, items) {
  $('#export-details-heading').textContent = title;
  $('#export-warnings').replaceChildren(...items.map(text => { const item = document.createElement('li'); item.textContent = text; return item; }));
  $('#export-details').showModal();
}
function showApproximations(title, text, items) {
  toast(text, null, [[`${items.length} approximations · Details`, () => showDetails(title, items)]]);
}
function handleEvent(event, reply) {
  const finished = reply ? pending : null; if (reply) pending = null;
  if (event.event === 'Deck') {
    if (gesture) api.endGesture(true);
    if (event.colors) applyTheme(event.colors);
    // A kept version stays on screen through later edits; a suggested edit's
    // preview is stale once the deck moves.
    if (!state.preview?.history) feedback.clearPreview();
    authoritative = structuredClone(event.deck);
    Object.assign(state, { deck:authoritative, revision:event.revision, hashes:event.hashes, images:event.images, threads:event.threads || [], linked:event.linked, flash:event.changed || [] });
    // A reply acknowledges only its own command, not previews still in flight.
    for (const item of [pending, ...queue]) if (item?.ops) state.deck = applyLocal(state.deck, item.ops) || state.deck;
    if (!api.slide()) state.slideId = state.deck.slides[0]?.id || null;
    const known = new Set(allNodes(api.slide()).map(n => n.id)); state.selected = new Set([...state.selected].filter(id => known.has(id)));
    if (state.openNewest) { state.openThread = state.threads.at(-1)?.id; state.openNewest = false; }
    if (event.notice) toast(event.notice);
    state.template = !!event.template;
    // Only a deck that follows a template has a choice to make; checked, the
    // formats above export into the template's package, unchecked a plain file.
    $('#export-template').hidden = !state.template;
    $('#export-template').setAttribute('aria-checked', String(state.template && !plainFile));
    if (event.import_warnings?.length) showApproximations('Import approximations', 'Imported from PowerPoint', event.import_warnings);
    deckActivities = event.activities;
    renderDeck();
    if (state.side === 'history') command({ command:'History' });
    library.onDocument(event);
    for (const node of document.querySelectorAll('#canvas .block')) node.classList.toggle('external-flash', state.flash.includes(node.dataset.id));
    if (state.flash.length) { clearTimeout(flashTimer); flashTimer = setTimeout(() => { state.flash = []; document.querySelectorAll('.external-flash').forEach(n => n.classList.remove('external-flash')); }, 1800); }
  } else if (event.event === 'History') {
    state.versions = event.versions; renderHistory();
  } else if (event.event === 'HistoryPreview' && event.deck) {
    showVersion(event.id, event.deck);
  } else if (event.event === 'Designs') {
    const list = $('#deck-design-list'); list.replaceChildren();
    for (const design of event.available) {
      const row = element('button', `design-row${design.current ? ' on' : ''}`); row.type = 'button'; row.setAttribute('aria-pressed', String(design.current));
      row.append(element('span', 'design-row-name', design.name));
      row.onclick = () => { if (!design.current) command({ command: 'ApplyDesign', design: design.slug }); }; list.append(row);
    }
  } else if (event.event === 'TemplateImported') {
    const actions = [['Use for this deck', () => command({ command: 'ApplyDesign', design: event.slug })]];
    if (event.warnings?.length) actions.push(['Warnings', () => showDetails('Template warnings', event.warnings)]);
    toast(`Imported ${event.name}`, null, actions);
    command({ command: 'Designs' });
  } else if (event.event === 'Theme') {
    applyTheme(event.colors);
  } else if (event.event === 'Library' || event.event === 'Idle') {
    library.onEvent(event);
  } else if (event.event === 'Focus') {
    if (state.deck.slides.some(s => s.id === event.slide)) { state.slideId = event.slide; state.selected.clear(); render(); }
  } else {
    feedback.event(event);
    if (event.event === 'Error') { rollback(); library.onEvent(event); toast(event.message); }
    if (event.event === 'Saved') {
      const name = event.path.split(/[\\/]/).pop();
      toast(`Saved a copy: ${name}${event.assets ? ` with ${event.assets} asset${event.assets === 1 ? '' : 's'}` : ''}`, null, [['Reveal', () => command({ command:'Reveal', path:event.path })]]);
    }
    if (event.event === 'Exported') {
      const warnings = event.warnings || [], actions = [['Reveal', () => command({ command:'Reveal', path:event.path })]];
      if (warnings.length) actions.push([`${warnings.length} approximations · Details`, () => showDetails('Export approximations', warnings)]);
      toast(`Exported ${event.path.split(/[\\/]/).pop()}`, null, actions);
    }
  }
  finished?.complete?.(event); pump();
}
// Doc mode lives in ./doc/doc.js, loaded when the backend opens a doc. Its
// events go there; a deck event brings the deck UI back.
const DOC_EVENTS = new Set(['Doc', 'DocFeedback', 'DocPreview', 'DocFocus', 'Pages']);
const SHARED_EVENTS = new Set(['Asset', 'Exported', 'Saved', 'Revealed', 'Error', 'Theme', 'Designs', 'TemplateImported', 'Library', 'Idle', 'History', 'HistoryPreview']);
let docUi = null, docQueue = Promise.resolve();
function route(event, reply) {
  if (docUi && event.event === 'Deck') {
    docQueue = docQueue.then(() => { docUi?.unmount(); docUi = null; document.body.classList.remove('doc-mode'); handleEvent(event, reply); });
    return;
  }
  if (DOC_EVENTS.has(event.event) || (docUi && SHARED_EVENTS.has(event.event))) {
    if (reply) pending = null;
    docQueue = docQueue.then(async () => {
      if (!docUi) {
        const module = await import('./doc/doc.js');
        docUi = module.mount({ applyTheme, ask, library });
        document.body.classList.add('doc-mode');
      }
      docUi.handle(event, reply);
    });
    return;
  }
  handleEvent(event, reply);
}
window.studioEvent = event => route(event, true);
window.studioAsyncEvent = event => route(event, false);
$('#undo').onclick = () => command({ command:'Undo' }); $('#redo').onclick = () => command({ command:'Redo' });
function deckMenu(open) { $('#deck-menu').hidden = !open; $('#deck-title').setAttribute('aria-expanded', String(open)); }
$('#deck-title').onclick = () => deckMenu($('#deck-menu').hidden);
document.addEventListener('pointerdown', event => { if (!event.target.closest('.deck-menu')) deckMenu(false); });
document.addEventListener('keydown', event => { if (event.key === 'Escape' && !$('#deck-menu').hidden) { deckMenu(false); $('#deck-title').focus(); } });
function openSide(tab) {
  if (state.side === 'comments' && tab !== 'comments') feedback.closeThread();
  state.side = tab;
  if (tab === 'history') { renderHistory(); command({ command:'History' }); }
  if (tab === 'design') renderDesignPane();
  render();
}
const toggleSide = tab => openSide(state.side === tab ? null : tab);
function properties() { toggleSide('properties'); }
function renderSidebar() {
  $('#sidebar').hidden = !state.side;
  document.body.classList.toggle('side-open', Boolean(state.side));
  document.body.classList.toggle('left-hidden', Boolean(state.leftHidden));
  for (const tab of document.querySelectorAll('#sidebar .side-tabs [role=tab]')) { const on = tab.dataset.side === state.side; tab.setAttribute('aria-selected', String(on)); tab.tabIndex = on ? 0 : -1; }
  $('#properties-panel').hidden = state.side !== 'properties'; $('#comments-panel').hidden = state.side !== 'comments';
  $('#history-pane').hidden = state.side !== 'history'; $('#design-pane').hidden = state.side !== 'design';
  $('#toggle-comments').setAttribute('aria-pressed', String(state.side === 'comments'));
  $('#toggle-history').setAttribute('aria-pressed', String(state.side === 'history'));
  $('#deck-design').setAttribute('aria-pressed', String(state.side === 'design'));
  const count = $('#comment-count').textContent;
  $('#side-comment-count').textContent = count === '0' ? '' : count; $('#toggle-comments').dataset.count = count;
  $('#toggle-left').setAttribute('aria-label', state.leftHidden ? 'Show slides' : 'Hide slides');
}
for (const tab of document.querySelectorAll('#sidebar .side-tabs [role=tab]')) tab.onclick = () => openSide(tab.dataset.side);
$('#close-side').onclick = () => openSide(null);
$('#toggle-comments').onclick = () => toggleSide('comments');
$('#toggle-history').onclick = () => toggleSide('history');
$('#deck-design').onclick = () => toggleSide('design');
$('#more-history').onclick = () => { moreMenu(false); openSide('history'); };
$('#more-design').onclick = () => { moreMenu(false); openSide('design'); };
$('#more-properties').onclick = () => { moreMenu(false); openSide('properties'); };
function toggleLeft() {
  if (matchMedia('(max-width: 899px)').matches) { $('#strip-pane').classList.toggle('open'); return; }
  state.leftHidden = !state.leftHidden;
  try { localStorage.setItem('nexus-deck-left', state.leftHidden ? 'hidden' : ''); } catch { /* storage may be unavailable */ }
  renderSidebar(); applyViewport();
}
$('#toggle-left').onclick = toggleLeft;
function moreMenu(open) {
  $('#more-menu').hidden = !open; $('#more').setAttribute('aria-expanded', String(open));
  if (open) { $('#undo').disabled = false; $('#more-menu button:not(.narrow-only)').focus(); }
}
$('#more').onclick = () => moreMenu($('#more-menu').hidden);
document.addEventListener('pointerdown', event => { if (!event.target.closest('.more-menu')) moreMenu(false); });
for (const id of ['undo', 'redo', 'play-from-here']) $(`#${id}`).addEventListener('click', () => moreMenu(false));
document.addEventListener('keydown', event => {
  if ($('#more-menu').hidden) return;
  const items = [...document.querySelectorAll('#more-menu button')].filter(b => b.offsetParent), index = items.indexOf(document.activeElement);
  if (['ArrowDown','ArrowUp','Escape'].includes(event.key)) {
    event.preventDefault(); event.stopImmediatePropagation();
    if (event.key === 'Escape') { moreMenu(false); $('#more').focus(); }
    else items[(index + (event.key === 'ArrowUp' ? -1 : 1) + items.length) % items.length]?.focus();
  }
});
function renderStatusDot(saving) {
  const pill = $('#link-status'), working = pill.classList.contains('working');
  const dot = $('#status-dot');
  dot.className = `status-dot${saving ? ' saving' : working ? ' working' : ''}`;
  const label = `${saving ? 'Saving' : 'Saved'} · ${pill.textContent || 'Not linked'}`;
  dot.title = label; dot.setAttribute('aria-label', label);
}

// -- version history ------------------------------------------------------------------------

const clockOf = iso => new Date(iso).toLocaleTimeString([], { hour:'2-digit', minute:'2-digit' });
function dayOf(iso) {
  const d = new Date(iso), today = new Date(), yesterday = new Date(Date.now() - 864e5);
  if (d.toDateString() === today.toDateString()) return 'Today';
  if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
  return d.toLocaleDateString([], { month:'short', day:'numeric' });
}
function element(tag, className, text) { const node = document.createElement(tag); if (className) node.className = className; if (text !== undefined) node.textContent = text; return node; }
function versionRow(title, detail, dot, on, run) {
  const row = element('button', `version${on ? ' on' : ''}`); row.type = 'button';
  const text = element('span', 'version-text'); text.append(element('span', 'version-title', title), element('span', 'version-detail', detail));
  row.append(element('span', `version-dot ${dot}`), text); row.onclick = run; return row;
}
function renderHistory() {
  const pane = $('#history-pane'); if (pane.hidden && state.side !== 'history') return;
  pane.replaceChildren(versionRow('Current version', 'What you see now', 'now', !state.preview?.history, () => { if (state.preview?.history) { feedback.clearPreview(); render(); renderHistory(); } }));
  if (!state.versions) { pane.append(element('div', 'menu-empty', 'Loading…')); return; }
  if (!state.versions.length) { pane.append(element('p', 'side-note', 'Earlier versions appear here after the first edit.')); return; }
  let day = '';
  for (const v of state.versions) {
    const d = dayOf(v.at); if (d !== day) { day = d; pane.append(element('div', 'side-eyebrow', d)); }
    pane.append(versionRow(`${clockOf(v.at)} · ${v.who === 'model' ? 'nexus' : 'You'}`, v.summary, v.who === 'model' ? 'model' : '', state.preview?.history === v.id, () => command({ command:'HistoryPreview', id:v.id })));
  }
  pane.append(element('p', 'side-note', 'Each version is the deck before that edit. Restoring makes a new version, so it can be undone too.'));
}
function showVersion(versionId, deck) {
  const version = state.versions?.find(v => v.id === versionId);
  const slideId = deck.slides.some(s => s.id === state.slideId) ? state.slideId : deck.slides[0]?.id;
  state.preview = { deck, slideId, before:false, history:versionId }; state.previewTarget = null; state.slideId = slideId || state.slideId;
  $('#preview-bar').hidden = false;
  $('#preview-bar strong').textContent = version ? `Before ${version.who === 'model' ? 'nexus\u2019s' : 'your'} edit at ${clockOf(version.at)}` : 'An earlier version';
  $('#preview-toggle').textContent = 'That version · B for now';
  $('#preview-apply').textContent = 'Restore this version';
  $('#preview-apply').onclick = () => { feedback.clearPreview(); command({ command:'HistoryRestore', id:versionId }, event => { if (event.event !== 'Error') toast('Restored that version', null, [['Undo', () => command({ command:'Undo' })]]); }); };
  render(); renderHistory();
}

// -- design tab ---------------------------------------------------------------------------------

function renderDesignPane() {
  const theme = state.deck?.theme; if (!theme) return;
  const info = $('#deck-design-info'); info.replaceChildren();
  info.append(element('div', 'side-eyebrow', 'This deck follows'), element('div', 'design-name', state.deck.design || 'Custom'));
  const swatches = element('span', 'd-swatches');
  for (const role of ['bg', 'fg', 'accent', 'accent2', 'muted']) { const value = theme.colors?.[role]; if (!value) continue; const swatch = element('span', 'swatch'); swatch.style.background = value; swatch.title = role; swatches.append(swatch); }
  info.append(swatches, element('div', 'design-fonts', Object.entries(theme.fonts || {}).map(([role, family]) => `${role[0].toUpperCase()}${role.slice(1)} · ${family}`).join('  ·  ')));
  command({ command:'Designs' });
}
for (const mode of ['edit','comment']) $(`#${mode}-mode`).onclick = () => { state.mode = mode; state.selected.clear(); render(); };
// A slide is 1920 px wide natively (the theme's size). Fit is responsive;
// numeric zoom is screen pixels per native slide pixel, not a multiple of fit.
let nativeWidth = 1920; const stage = $('#stage'), wrap = $('#slide-wrap');
function applyViewport() {
  nativeWidth = state.deck ? slideSize(state.deck.theme).w : 1920;
  const scale = state.zoom === null ? 1 : state.zoom * nativeWidth / Math.max(1, parseFloat(getComputedStyle(wrap).width));
  wrap.style.transform = `translate(${state.panX}px,${state.panY}px) scale(${scale})`;
  $('#zoom-fit').textContent = state.zoom === null ? 'Fit' : `${Math.round(state.zoom * 100)}%`;
  animationPanel.badges(); panel.positionToolbar();
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
$('#open-deck').onclick = () => { deckMenu(false); command({ command:'OpenDialog' }); };
$('#new-doc').onclick = () => { deckMenu(false); ask('New doc', 'Title', 'Created in your active space’s files/docs folder.', 'Untitled', title => command({ command:'NewDoc', title })); };
$('#save-copy').onclick = () => { deckMenu(false); command({ command:'SaveCopy' }); };
$('#open-library').onclick = () => library.show();
$('#new-deck').onclick = () => { deckMenu(false); ask('New deck', 'Title', 'Created in your active space’s files/decks folder.', 'Untitled', title => command({ command:'NewDeck', title })); };
function insert(block) {
  const slide = api.slide(); if (!slide) { toast('Add a slide first.'); return; }
  state.selected = new Set([block.id]); state.mode = 'edit'; apply(() => [{ op:'insert_node', parent:slide.id, node:block }]);
}
$('#add-text').onclick = () => insert(textNode('Double-click to edit', { position:{ x:160, y:160 }, sizing:{ w:960 } }));
$('#add-shape').onclick = () => insert({ id:id('b'), kind:'shape', shape:'rect', fill:'accent', position:{ x:480, y:300 }, sizing:{ w:600, h:420 } });
$('#add-image').onclick = () => ask('Import image', 'Image path', 'PNG, JPEG or WebP. A sanitized copy stays with the deck.', '', path => command({ command:'ImportAsset', path }, event => { if (event.event === 'Asset') insert({ id:id('b'), kind:'image', src:event.src, fit:'contain', position:{ x:240, y:180 }, sizing:{ w:1200, h:720 } }); }));
$('#speaker-notes').onchange = event => { const slide = api.slide(), notes = event.target.value; if (slide) apply(() => [{ op:'set_notes', slide:slide.id, notes }]); };
let plainFile = false;
// "Save to exports/ without asking": the quick export, remembered on this device.
const quickExport = () => { try { return localStorage.getItem('nexus-quick-export') === '1'; } catch { return false; } };
const exportCommand = format => ({ command:quickExport() ? 'Export' : 'ExportAs', format, ...(plainFile ? { template:'none' } : {}) });
$('#add-template').onclick = () => chooseTemplate(command);
templateDrop(command, toast, undefined, () => !docUi);
$('#export-template').onclick = () => { plainFile = !plainFile; $('#export-template').setAttribute('aria-checked', String(state.template && !plainFile)); };
$('#export-quick').onclick = () => { try { localStorage.setItem('nexus-quick-export', quickExport() ? '0' : '1'); } catch { /* storage may be unavailable */ } syncQuick(); };
function syncQuick() { $('#export-quick').setAttribute('aria-checked', String(quickExport())); }
syncQuick();
function exportMenu(open, focus = false) {
  $('#export-menu').hidden = !open; $('#export').setAttribute('aria-expanded', String(open));
  if (open && focus) $('#export-menu button').focus();
}
$('#export').onclick = () => exportMenu($('#export-menu').hidden, true);
for (const item of document.querySelectorAll('#export-menu [data-format]')) item.onclick = () => {
  exportMenu(false); $('#export').focus();
  command(exportCommand(item.dataset.format));
};
document.addEventListener('pointerdown', event => { if (!event.target.closest('.export-menu')) exportMenu(false); });
document.addEventListener('keydown', event => {
  if ($('#export-menu').hidden) return;
  const items = [...document.querySelectorAll('#export-menu button')], index = items.indexOf(document.activeElement);
  if (['ArrowDown','ArrowUp','Home','End','Escape'].includes(event.key)) {
    event.preventDefault(); event.stopImmediatePropagation();
    if (event.key === 'Escape') { exportMenu(false); $('#export').focus(); }
    else items[event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (index + (event.key === 'ArrowUp' ? -1 : 1) + items.length) % items.length].focus();
  } else if (event.key === 'Tab') exportMenu(false);
});
document.addEventListener('keydown', event => {
  if (docUi) return; // doc mode has its own keys
  if (playback.key(event)) return;
  const key = event.key.toLowerCase(), mod = event.metaKey || event.ctrlKey, editingField = event.target.closest('input,textarea,select,[contenteditable]');
  if (mod && key === '\\') { event.preventDefault(); if (event.shiftKey) toggleLeft(); else properties(); return; }
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
  else if (key === 'v' && !mod) { state.mode = 'edit'; render(); }
  else if (event.key === 'Escape') { const wasHistory = state.preview?.history; feedback.clearPreview(); if (wasHistory) renderHistory(); state.openThread = null; state.draftAnchor = null; state.selected.clear(); render(); }
  else if (key === 'c') { state.mode = state.mode === 'comment' ? 'edit' : 'comment'; state.selected.clear(); render(); }
  else if (mod && key === 'z') { event.preventDefault(); command({ command:event.shiftKey ? 'Redo' : 'Undo' }); }
  else if (mod && key === 'd') { event.preventDefault(); canvas.duplicate(); }
  else if (['Delete','Backspace'].includes(event.key)) { event.preventDefault(); canvas.remove(); }
  else if (event.key.startsWith('Arrow')) { event.preventDefault(); const step = event.shiftKey ? 10 : 1; canvas.nudge(event.key === 'ArrowRight' ? step : event.key === 'ArrowLeft' ? -step : 0, event.key === 'ArrowDown' ? step : event.key === 'ArrowUp' ? -step : 0); }
  else if (event.key === 'F5' || (event.shiftKey && event.key === 'Enter')) { event.preventDefault(); playback.start(event.key === 'Enter'); }
});
command({ command:'Load' });
