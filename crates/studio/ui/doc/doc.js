// Doc mode: a calm writing surface with the document as the hero. The chrome is
// a slim icon bar, an outline on the left (with the save and link status at its
// foot) and one right sidebar with Comments, History and Design tabs; the page
// stays centered between them. Formatting appears over selected text only. The
// page follows the doc's own design; the chrome follows the terminal theme.
import { chooseTemplate, templateDrop } from '../templates.js';
import { editorController } from './editor.js';
import { menusController } from './menus.js';
import { toolbarController } from './toolbar.js';
import { commentsController } from './comments.js';
import { glyph } from './glyphs.js';
import * as M from './model.js';
import * as E from './edits.js';
import { serializeRuns, plainText, parseInline } from './inline.js';
import { offsetIn } from './dom.js';
import { ensureDeckFonts, fontStack, color } from '../render.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
const STYLES = ['title', 'h1', 'h2', 'h3', 'body', 'quote', 'caption', 'code', 'lead', 'display'];
const FALLBACK = { family: 'body', size: 11, weight: 400, line_height: 1.4, color: 'fg', italic: false, letter_spacing: 0 };

/** Build the doc UI inside `#doc-app` and return the handle app.js talks to. */
export function mount(shared) {
  const app = document.querySelector('#doc-app');
  app.replaceChildren();
  app.hidden = false;

  const state = {
    doc: null, base: null, relative: '', threads: [], activities: {}, linked: false, assets: {}, design: null, canUndo: false, canRedo: false,
    decos: {}, drafts: {}, revision: '', preview: false, zoom: 1.2, pagesRevision: '',
  };
  try { state.zoom = Number(localStorage.getItem('nexus-doc-zoom')) || 1.2; } catch { /* storage may be unavailable */ }

  // -- the shell -------------------------------------------------------------------------

  const top = el('header', 'doc-top');
  function iconButton(name, label, className = '') {
    const b = el('button', `icon-button quiet top-icon ${className}`.trim());
    b.type = 'button';
    b.append(glyph(name));
    b.setAttribute('aria-label', label);
    b.title = label;
    return b;
  }
  const outlineToggle = iconButton('panel', 'Toggle outline (⌘\\)');
  const crumbs = el('nav', 'doc-crumbs');
  crumbs.setAttribute('aria-label', 'Breadcrumb');
  const libraryLink = el('button', 'crumb-link', 'Library');
  libraryLink.type = 'button'; libraryLink.title = 'Library (⌘/Ctrl+O)';
  const titleInput = el('input', 'doc-title-input');
  titleInput.type = 'text'; titleInput.placeholder = 'Untitled'; titleInput.maxLength = 200; titleInput.spellcheck = false;
  titleInput.setAttribute('aria-label', 'Document title');
  const statusDot = el('span', 'status-dot');
  statusDot.setAttribute('role', 'img');
  crumbs.append(libraryLink, el('span', 'crumb-sep', '/'), titleInput, statusDot);
  const commentsButton = iconButton('comment', 'Comments', 'with-count');
  const commentCount = el('span', 'count', '');
  commentsButton.append(commentCount);
  const historyButton = iconButton('history', 'Version history', 'wide-only');
  const designButton = iconButton('design', 'Design', 'wide-only');
  const exportButton = iconButton('export', 'Export');
  const moreButton = iconButton('more', 'More');
  for (const b of [exportButton, moreButton]) b.setAttribute('aria-haspopup', 'menu');
  top.append(outlineToggle, crumbs, el('span', 'spacer'), commentsButton, historyButton, designButton, exportButton, moreButton);

  const body = el('div', 'doc-body');
  const outline = el('aside', 'doc-outline');
  const outlineNav = el('nav', 'outline-nav');
  outlineNav.setAttribute('aria-label', 'Outline');
  const statusBox = el('div', 'doc-status');
  statusBox.setAttribute('role', 'status');
  const saveState = el('div', 'status-line save', 'Saved');
  const pill = el('div', 'status-line link', 'Not linked');
  statusBox.append(saveState, pill);
  outline.append(outlineNav, statusBox);
  const page = el('div', 'page-scroll');
  const grid = el('div', 'page-grid');
  const sheetColumn = el('div', 'sheet-column');
  const historyBanner = el('div', 'history-banner');
  historyBanner.setAttribute('role', 'status');
  historyBanner.hidden = true;
  const sheet = el('article', 'sheet');
  const surface = el('div', 'surface');
  surface.id = 'doc-surface';
  surface.contentEditable = 'true';
  surface.spellcheck = true;
  surface.setAttribute('role', 'textbox');
  surface.setAttribute('aria-multiline', 'true');
  surface.setAttribute('aria-label', 'Document');
  sheet.append(surface);
  sheetColumn.append(sheet);
  sheet.addEventListener('mousedown', e => { if (e.target === sheet && !historyView) { e.preventDefault(); editor.focusEnd(); } });
  grid.append(sheetColumn);
  page.append(historyBanner, grid);
  const previewPane = el('div', 'preview-pane');
  previewPane.hidden = true;

  // One sidebar, three tabs; the page keeps the space that is left, centered.
  const sidebar = el('aside', 'doc-sidebar');
  sidebar.setAttribute('aria-label', 'Sidebar');
  const tabs = el('div', 'side-tabs');
  tabs.setAttribute('role', 'tablist');
  const panes = {}, tabButtons = {};
  for (const [key, label] of [['comments', 'Comments'], ['history', 'History'], ['design', 'Design']]) {
    const tab = el('button', 'side-tab');
    tab.type = 'button'; tab.setAttribute('role', 'tab'); tab.id = `doc-tab-${key}`;
    tab.append(el('span', '', label));
    if (key === 'comments') tab.append(el('span', 'tab-count', ''));
    tab.addEventListener('click', () => openSidebar(key));
    tabs.append(tab);
    tabButtons[key] = tab;
    const pane = el('div', `side-pane pane-${key}`);
    pane.setAttribute('role', 'tabpanel'); pane.setAttribute('aria-labelledby', tab.id);
    panes[key] = pane;
  }
  const closeSide = iconButton('x', 'Close sidebar', 'side-close');
  closeSide.addEventListener('click', () => openSidebar(null));
  tabs.append(el('span', 'spacer'), closeSide);
  sidebar.append(tabs, panes.comments, panes.history, panes.design);
  const margin = panes.comments;

  body.append(outline, page, previewPane, sidebar);
  const overlay = el('div', 'doc-overlay');
  const toastNode = el('div', 'doc-toast');
  toastNode.setAttribute('role', 'status');
  toastNode.hidden = true;
  app.append(top, body, overlay, toastNode);

  // `null` until someone opens or closes the sidebar on this device; until
  // then, open comments show themselves on a wide window.
  let sideTab = null, sideChosen = false;
  try { const stored = localStorage.getItem('nexus-doc-sidebar'); sideChosen = stored !== null; sideTab = stored || null; } catch { /* storage may be unavailable */ }
  let outlineHidden = false;
  try { outlineHidden = localStorage.getItem('nexus-doc-outline') === 'hidden'; } catch { /* ignore */ }
  /** Open the sidebar on a tab, or close it with `null`. */
  function openSidebar(tab) {
    sideTab = tab; sideChosen = true;
    try { localStorage.setItem('nexus-doc-sidebar', tab || ''); } catch { /* ignore */ }
    renderSidebar();
    if (tab === 'history') { renderHistory(); requestHistory(); }
    if (tab === 'design') { command({ command: 'DocDesigns' }); renderDesign(); }
  }
  function toggleSidebar(tab) { openSidebar(sideTab === tab ? null : tab); }
  function renderSidebar() {
    app.classList.toggle('side-open', Boolean(sideTab));
    sidebar.hidden = !sideTab;
    for (const [key, tab] of Object.entries(tabButtons)) {
      const on = key === sideTab;
      tab.setAttribute('aria-selected', String(on));
      tab.tabIndex = on ? 0 : -1;
      panes[key].hidden = !on;
    }
    commentsButton.setAttribute('aria-pressed', String(sideTab === 'comments'));
    historyButton.setAttribute('aria-pressed', String(sideTab === 'history'));
    designButton.setAttribute('aria-pressed', String(sideTab === 'design'));
  }
  const narrow = () => matchMedia('(max-width: 899px)').matches;
  function renderOutlinePanel() {
    app.classList.toggle('outline-hidden', outlineHidden);
    outlineToggle.title = outlineHidden ? 'Show outline (⌘\\)' : 'Hide outline (⌘\\)';
    outlineToggle.setAttribute('aria-label', outlineToggle.title);
  }

  /** Document and window listeners end with this window, so switching documents leaks nothing. */
  const lifetime = new AbortController();

  // -- the command queue ---------------------------------------------------------------------

  const queue = [];
  let pending = null, toastTimer = 0, flashTimer = 0, expectNew = null;
  const post = cmd => window.ipc.postMessage(JSON.stringify(cmd));
  function pump() {
    if (pending || !queue.length) return;
    pending = queue.shift();
    try { post(pending.cmd); } catch (error) { toast(error.message); pending = null; pump(); }
    renderSave();
  }
  function command(cmd, complete) { queue.push({ cmd, complete }); pump(); }
  const ask = cmd => new Promise((resolve, reject) => command(cmd, event => event.event === 'Error' ? reject(new Error(event.message)) : resolve(event)));
  function renderSave() {
    const saving = pending?.ops || editor.hasDirty() || queue.some(q => q.ops);
    saveState.textContent = saving ? 'Saving…' : 'Saved';
    saveState.classList.toggle('busy', Boolean(saving));
    renderStatusDot();
  }

  function toast(text, actions = []) {
    // The library covers the window; its notices go above it.
    if (shared.library?.visible) { shared.library.toast(text, actions); return; }
    toastNode.replaceChildren(document.createTextNode(text));
    for (const [label, run] of actions) {
      const b = el('button', 'quiet', label);
      b.type = 'button';
      b.addEventListener('click', () => { toastNode.hidden = true; run(); });
      toastNode.append(b);
    }
    toastNode.hidden = !text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toastNode.hidden = true; }, actions.length ? 10000 : 5000);
  }

  /**
   * Apply operations: the typing waiting to be saved goes first, so a structural
   * action and the words before it are one undo step. The page shows the result
   * at once and the backend confirms.
   */
  function apply(ops, { select = null, typing = false, focusNew = false } = {}) {
    const typed = editor.collectDirty();
    const all = coalesce([...typed, ...ops]);
    if (!all.length) { if (select) editor.place(select); return; }
    let next = state.doc;
    if (ops.length) next = M.applyOps(state.doc, ops);
    if (focusNew) expectNew = new Set(state.doc.blocks.map(b => b.id));
    if (next) state.doc = next;
    queue.push({ cmd: { command: 'DocApply', ops: all }, ops: next ? all : null });
    if (!typing) {
      comments.computeDecos();
      editor.reconcile();
      comments.render();
      renderOutline();
      if (select) { surface.focus({ preventScroll: true }); editor.place(select); }
    } else comments.layout();
    pump();
  }
  /** Neighbouring patches of one block become one, so a batch says each thing once. */
  function coalesce(ops) {
    const out = [];
    for (const op of ops) {
      const last = out.at(-1);
      if (last?.op === 'update_block' && op.op === 'update_block' && last.id === op.id) last.patch = { ...last.patch, ...op.patch };
      else out.push(structuredClone(op));
    }
    return out;
  }
  function undo() { apply([]); command({ command: 'Undo' }); }
  function redo() { apply([]); command({ command: 'Redo' }); }

  // -- events from the backend ------------------------------------------------------------------

  function handle(event, reply) {
    const finished = reply ? pending : null;
    if (reply) pending = null;
    switch (event.event) {
      case 'Doc': onDoc(event, reply); break;
      case 'DocFeedback':
        state.threads = event.threads;
        comments.updateActivities(event.activities);
        comments.refresh();
        if (event.notes?.length) toast(event.notes.join('; '));
        break;
      case 'DocPreview': comments.onPreview(event); break;
      case 'DocFocus': if (event.block) scrollToBlock(event.block, true); break;
      case 'Designs': designs.available = event.available; renderDesign(); break;
      case 'History': history.versions = event.versions; renderHistory(); break;
      case 'HistoryPreview': if (event.doc) showVersion(event.id, event.doc); break;
      case 'TemplateImported':
        toast(`Imported ${event.name}`, [['Use for this doc', () => command({ command: 'ApplyDesign', design: event.slug })], ...(event.warnings?.length ? [['Warnings', () => toast(event.warnings.join(' · '))]] : [])]);
        command({ command: 'DocDesigns' });
        break;
      case 'Pages': showPages(event); break;
      case 'Library': case 'Idle': shared.library.onEvent(event); break;
      case 'Saved': {
        const name = event.path.split(/[\\/]/).pop();
        toast(`Saved a copy: ${name}${event.assets ? ` with ${event.assets} asset${event.assets === 1 ? '' : 's'}` : ''}`, [['Reveal', () => command({ command: 'Reveal', path: event.path })]]);
        break;
      }
      case 'Theme': shared.applyTheme(event.colors); break;
      case 'Exported': {
        const actions = [['Reveal', () => command({ command: 'Reveal', path: event.path })]];
        if (event.warnings?.length) actions.push([`${event.warnings.length} approximations`, () => toast(event.warnings.join(' · '))]);
        toast(`Exported ${event.path.split(/[\\/]/).pop()}`, actions);
        break;
      }
      case 'Error':
        shared.library.onEvent(event);
        toast(event.message);
        if (reply && finished?.cmd?.command === 'DocApply') { queue.length = 0; command({ command: 'Load' }); }
        break;
      default: break;
    }
    finished?.complete?.(event);
    pump();
    renderSave();
  }

  function onDoc(event, reply) {
    if (event.colors) shared.applyTheme(event.colors);
    const first = !state.doc;
    const conflict = event.notice && /not applied/.test(event.notice);
    const external = !reply && !first;
    state.base = event.doc;
    state.doc = structuredClone(event.doc);
    if (conflict) { queue.length = 0; editor.discardAll(); }
    else {
      // Edits still on their way (in flight or queued) stay visible.
      for (const item of [pending, ...queue]) if (item?.ops) { const next = M.applyOps(state.doc, item.ops); if (next) state.doc = next; }
      if (external) for (const id of event.changed || []) editor.discardDirty(id);
      editor.overlayDirty();
    }
    Object.assign(state, {
      relative: event.relative, threads: event.threads, linked: event.linked, assets: event.assets || {}, design: event.design,
      canUndo: event.can_undo, canRedo: event.can_redo, revision: event.revision,
    });
    applyDocTheme();
    comments.updateActivities(event.activities);
    renderChrome();
    comments.refresh();
    renderOutline();
    if (event.notice) toast(event.notice, /reloaded/.test(event.notice) ? [['Undo', undo]] : []);
    if (event.changed?.length) flash(event.changed);
    if (expectNew && reply) {
      const fresh = state.doc.blocks.filter(b => !expectNew.has(b.id));
      expectNew = null;
      const last = fresh.at(-1), at = last && E.endOf(last);
      if (at) { surface.focus({ preventScroll: true }); editor.place(at); }
    }
    if (state.preview && event.revision !== state.pagesRevision) requestPages();
    if (sideTab === 'history') requestHistory();
    // A version on show stays on show; the doc it will return to is the new one.
    if (historyView) { historyView.current = state.doc; renderVersion(); }
    shared.library.onDocument(event);
    if (first && !shared.library.visible) surface.focus({ preventScroll: true });
  }

  function flash(ids) {
    for (const id of ids) surface.querySelector(`[data-block="${CSS.escape(id)}"]`)?.classList.add('flash');
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => surface.querySelectorAll('.flash').forEach(n => n.classList.remove('flash')), 1800);
    const first = surface.querySelector(`[data-block="${CSS.escape(ids[0])}"]`);
    if (first && !editor.selection()) scrollTo(first);
  }

  // -- the page follows the doc's design -----------------------------------------------------------

  function applyDocTheme() {
    const theme = state.doc.theme, set = (name, value) => app.style.setProperty(name, value);
    const colors = theme.colors;
    const mix = (a, b, pct) => `color-mix(in srgb, ${a} ${pct}%, ${b})`;
    set('--d-bg', colors.bg || '#ffffff'); set('--d-fg', colors.fg || '#1d1d1f'); set('--d-accent', colors.accent || '#0a6cff');
    set('--d-muted', colors.muted || mix(colors.fg || '#1d1d1f', colors.bg || '#fff', 55));
    set('--d-surface', colors.surface || mix(colors.fg || '#1d1d1f', colors.bg || '#fff', 6));
    set('--d-border', colors.border || mix(colors.fg || '#1d1d1f', colors.bg || '#fff', 20));
    set('--d-accent2', colors.accent2 || colors.accent || '#0a6cff');
    set('--measure', String(theme.measure_ch));
    set('--z', String(state.zoom));
    const family = name => fontStack(theme.fonts?.[name] || name);
    set('--d-font-body', family('body')); set('--d-font-heading', family('heading')); set('--d-font-mono', family('mono'));
    for (const name of STYLES) {
      const font = { ...FALLBACK, ...theme.text_styles?.[name] };
      const role = ['mono', 'heading', 'body'].includes(font.family) ? font.family : null;
      set(`--ts-${name}-size`, `${font.size}pt`);
      set(`--ts-${name}-weight`, String(font.weight));
      set(`--ts-${name}-lh`, String(font.line_height));
      set(`--ts-${name}-color`, color(font.color, { colors }));
      set(`--ts-${name}-style`, font.italic ? 'italic' : 'normal');
      set(`--ts-${name}-ls`, `${font.letter_spacing || 0}em`);
      set(`--ts-${name}-family`, role ? `var(--d-font-${role})` : fontStack(theme.fonts?.[font.family] || font.family));
    }
    ensureDeckFonts(theme, state.assets);
  }
  function setZoom(value) {
    state.zoom = Math.max(.8, Math.min(1.8, Math.round(value * 20) / 20));
    app.style.setProperty('--z', String(state.zoom));
    try { localStorage.setItem('nexus-doc-zoom', String(state.zoom)); } catch { /* ignore */ }
    comments.layout();
  }

  // -- the chrome ------------------------------------------------------------------------------------

  function renderChrome() {
    if (document.activeElement !== titleInput) titleInput.value = state.doc.title;
    sizeTitle();
    document.title = `${state.doc.title || 'Untitled'} — nexus studio`;
    renderLink();
    renderDesign();
  }
  /** The save and link status: lines at the foot of the outline, a dot by the title. */
  function renderLink() {
    const activities = Object.values(state.activities);
    const working = activities.some(a => ['working', 'progress'].includes(a.state)), waiting = activities.filter(a => a.state === 'queued').length;
    pill.textContent = !state.linked ? 'Not linked' : working ? 'nexus is working' : waiting ? `Linked · ${waiting} waiting` : 'Linked to nexus';
    pill.classList.toggle('connected', state.linked);
    pill.classList.toggle('working', state.linked && working);
    pill.title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
    renderStatusDot();
  }
  function renderStatusDot() {
    const saving = saveState.classList.contains('busy'), working = pill.classList.contains('working');
    statusDot.className = `status-dot${saving ? ' saving' : working ? ' working' : ''}`;
    const label = `${saveState.textContent} · ${pill.textContent}`;
    statusDot.title = label;
    statusDot.setAttribute('aria-label', label);
  }

  const sizeTitle = () => { titleInput.style.width = `${Math.min(48, Math.max(8, titleInput.value.length + 1))}ch`; };
  titleInput.addEventListener('input', sizeTitle);
  titleInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); titleInput.blur(); surface.focus({ preventScroll: true }); }
    else if (e.key === 'Escape') { titleInput.value = state.doc.title; titleInput.blur(); }
  });
  titleInput.addEventListener('change', () => {
    const title = titleInput.value.trim() || 'Untitled', old = state.doc.title;
    if (title === old) return;
    const ops = [{ op: 'set_title', title }];
    const block = state.doc.blocks.find(b => b.kind === 'title');
    if (block && plainText(parseInline(block.text)) === old) ops.push({ op: 'update_block', id: block.id, patch: { text: serializeRuns([{ t: title }]) } });
    apply(ops);
  });
  libraryLink.addEventListener('click', () => shared.library.show());
  commentsButton.addEventListener('click', () => toggleSidebar('comments'));
  historyButton.addEventListener('click', () => toggleSidebar('history'));
  designButton.addEventListener('click', () => toggleSidebar('design'));
  outlineToggle.addEventListener('click', toggleOutline);
  function toggleOutline() {
    // Narrow windows show the outline over the page; wide ones give it its column back.
    if (matchMedia('(max-width: 1099px)').matches) { outline.classList.toggle('open'); return; }
    outlineHidden = !outlineHidden;
    try { localStorage.setItem('nexus-doc-outline', outlineHidden ? 'hidden' : ''); } catch { /* ignore */ }
    renderOutlinePanel();
  }

  // -- outline ------------------------------------------------------------------------------------------

  function renderOutline() {
    const entries = M.outline(state.doc);
    outlineNav.replaceChildren();
    outlineNav.append(el('div', 'outline-heading', 'Outline'));
    if (!entries.length) { outlineNav.append(el('p', 'outline-empty', 'Headings appear here.')); return; }
    const list = el('ul');
    for (const entry of entries) {
      const li = el('li');
      li.dataset.level = entry.level;
      const a = el('button', 'outline-link', entry.text || (entry.level ? 'Untitled heading' : 'Untitled'));
      a.type = 'button';
      a.dataset.goto = entry.id;
      a.addEventListener('click', () => { scrollToBlock(entry.id); outline.classList.remove('open'); });
      li.append(a);
      list.append(li);
    }
    outlineNav.append(list);
    markCurrent();
  }
  function markCurrent() {
    const links = [...outlineNav.querySelectorAll('[data-goto]')];
    if (!links.length) return;
    const top = page.getBoundingClientRect().top + 96;
    let current = links[0];
    for (const link of links) {
      const node = surface.querySelector(`[data-block="${CSS.escape(link.dataset.goto)}"]`);
      if (node && node.getBoundingClientRect().top <= top) current = link;
    }
    for (const link of links) link.classList.toggle('current', link === current);
  }
  let outlineFrame = 0;
  page.addEventListener('scroll', () => { if (!outlineFrame) outlineFrame = requestAnimationFrame(() => { outlineFrame = 0; markCurrent(); }); });

  function scrollTo(node) { node.scrollIntoView({ block: 'center', behavior: reduced() ? 'auto' : 'smooth' }); }
  function scrollToBlock(id, flashIt = false) {
    const node = surface.querySelector(`[data-block="${CSS.escape(id)}"]`);
    if (!node) return;
    scrollTo(node);
    if (flashIt) flash([id]);
  }

  // -- design tab ------------------------------------------------------------------------------------------

  const designs = { available: null };
  const swatchRow = colors => {
    const row = el('span', 'd-swatches');
    for (const role of ['bg', 'fg', 'accent', 'accent2', 'muted']) {
      if (!colors[role]) continue;
      const s = el('span', 'swatch');
      s.style.background = colors[role]; s.title = role;
      row.append(s);
    }
    return row;
  };
  function renderDesign() {
    const pane = panes.design;
    if (pane.hidden || !state.design) return;
    pane.replaceChildren();
    const current = el('section', 'design-current');
    current.append(el('div', 'side-eyebrow', 'This document follows'), el('div', 'design-name', state.design.name), swatchRow(state.design.colors));
    const fonts = el('div', 'design-fonts');
    for (const role of ['heading', 'body', 'mono']) if (state.design.fonts[role]) fonts.append(el('span', '', `${role[0].toUpperCase()}${role.slice(1)} · ${state.design.fonts[role]}`));
    current.append(fonts);
    pane.append(current);
    if (state.design.update_available) {
      const update = el('div', 'design-update');
      update.append(el('span', '', 'The design changed since this doc copied it'));
      const apply1 = el('button', 'small', 'Apply update');
      apply1.type = 'button';
      apply1.addEventListener('click', () => command({ command: 'DocApplyDesign', design: null }));
      update.append(apply1);
      pane.append(update);
    }
    pane.append(el('div', 'side-eyebrow', 'Switch design'));
    const list = el('div', 'design-list');
    for (const d of designs.available || []) {
      const row = el('button', `design-row${d.current ? ' on' : ''}`);
      row.type = 'button';
      row.append(el('span', 'design-row-name', d.name), swatchRow(d.colors));
      if (d.current) row.append(glyph('check'));
      row.addEventListener('click', () => { if (!d.current) command({ command: 'DocApplyDesign', design: d.slug }); });
      list.append(row);
    }
    if (!designs.available) list.append(el('div', 'menu-empty', 'Loading…'));
    pane.append(list);
    const add = el('button', 'side-action', '');
    add.type = 'button';
    add.append(glyph('plus'), el('span', '', 'Add template…'));
    add.addEventListener('click', () => chooseTemplate(command));
    pane.append(add);
  }

  // -- history tab ------------------------------------------------------------------------------------------

  const history = { versions: null };
  let historyView = null, historyTimer = 0;
  function requestHistory() {
    clearTimeout(historyTimer);
    historyTimer = setTimeout(() => command({ command: 'History' }), 120);
  }
  const clock = iso => new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const dayOf = iso => {
    const d = new Date(iso), today = new Date(), yesterday = new Date(Date.now() - 864e5);
    if (d.toDateString() === today.toDateString()) return 'Today';
    if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
    return d.toLocaleDateString([], { month: 'short', day: 'numeric', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' });
  };
  function renderHistory() {
    const pane = panes.history;
    if (pane.hidden) return;
    pane.replaceChildren();
    const versions = history.versions;
    const current = el('button', `version${historyView ? '' : ' on'}`);
    current.type = 'button';
    current.append(el('span', 'version-dot now'), versionText('Current version', state.canUndo ? 'Undo steps back one edit at a time' : 'What you see now'));
    current.addEventListener('click', () => leaveVersion());
    pane.append(current);
    if (!versions) { pane.append(el('div', 'menu-empty', 'Loading…')); return; }
    if (!versions.length) { pane.append(el('p', 'side-note', 'Earlier versions appear here after the first edit.')); return; }
    let day = '';
    for (const v of versions) {
      const d = dayOf(v.at);
      if (d !== day) { day = d; pane.append(el('div', 'side-eyebrow', d)); }
      const row = el('button', `version${historyView?.id === v.id ? ' on' : ''}`);
      row.type = 'button';
      row.append(el('span', `version-dot ${v.who === 'model' ? 'model' : ''}`), versionText(`${clock(v.at)} · ${v.who === 'model' ? 'nexus' : 'You'}`, v.summary));
      row.title = 'Show the document as it was before this edit';
      row.addEventListener('click', () => command({ command: 'HistoryPreview', id: v.id }));
      pane.append(row);
    }
    pane.append(el('p', 'side-note', 'Each version is the document before that edit. Restoring makes a new version, so it can be undone too.'));
  }
  const versionText = (title, detail) => {
    const box = el('span', 'version-text');
    box.append(el('span', 'version-title', title), el('span', 'version-detail', detail));
    return box;
  };
  /** Show a kept version read-only in place of the doc. */
  function showVersion(id, doc) {
    apply([]);
    api.toolbar.hide(); api.menus.closeAll();
    historyView = { id, kept: doc, current: historyView?.current ?? state.doc };
    renderVersion();
    renderHistory();
    page.scrollTop = 0;
  }
  function renderVersion() {
    if (!historyView) return;
    const version = history.versions?.find(v => v.id === historyView.id);
    state.doc = historyView.kept;
    state.decos = {};
    surface.contentEditable = 'false';
    app.classList.add('viewing-version');
    editor.reconcile();
    const now = new Map(historyView.current.blocks.map(b => [b.id, JSON.stringify(b)]));
    for (const b of historyView.kept.blocks) {
      const node = surface.querySelector(`[data-block="${CSS.escape(b.id)}"]`);
      node?.classList.toggle('version-differs', now.get(b.id) !== JSON.stringify(b));
    }
    historyBanner.replaceChildren();
    historyBanner.hidden = false;
    const text = el('span', 'banner-text');
    text.append(el('strong', '', version ? `Before ${version.who === 'model' ? 'nexus\u2019s' : 'your'} edit at ${clock(version.at)}` : 'An earlier version'), document.createTextNode(' · blocks that differ from now are marked'));
    const back = el('button', 'quiet small', 'Back to current'), restore = el('button', 'small primary-quiet', 'Restore this version');
    back.type = restore.type = 'button';
    back.addEventListener('click', () => leaveVersion());
    restore.addEventListener('click', () => {
      const id = historyView.id;
      leaveVersion();
      command({ command: 'HistoryRestore', id }, e => { if (e.event !== 'Error') toast('Restored that version', [['Undo', undo]]); });
    });
    historyBanner.append(glyph('history'), text, back, restore);
    state.doc = historyView.current;
  }
  function leaveVersion() {
    if (!historyView) return;
    state.doc = historyView.current;
    historyView = null;
    historyBanner.hidden = true;
    surface.contentEditable = 'true';
    app.classList.remove('viewing-version');
    for (const node of surface.querySelectorAll('.version-differs')) node.classList.remove('version-differs');
    comments.refresh();
    renderHistory();
  }

  // -- export and more menus ---------------------------------------------------------------------------------

  const exportPop = el('div', 'popup menu export-pop');
  exportPop.hidden = true;
  exportPop.setAttribute('role', 'menu');
  exportPop.setAttribute('aria-label', 'Export');
  for (const [format, label, hint] of [['docx', 'Word document', '.docx'], ['pdf', 'PDF', '.pdf'], ['md', 'Markdown', '.md'], ['html', 'Web page', '.html']]) {
    const row = el('button', 'menu-row compact');
    row.type = 'button'; row.setAttribute('role', 'menuitem');
    row.append(el('span', 'menu-label', label), el('span', 'menu-hint', hint));
    row.addEventListener('click', () => { closePops(); exportButton.focus(); command({ command: quickExport() ? 'DocExport' : 'ExportAs', format, ...(plainFile ? { template: 'none' } : {}) }); });
    exportPop.append(row);
  }
  // One toggle, shown only when the doc follows a template: checked, every
  // format above exports into the template's package; unchecked, a plain file.
  const templateRule = el('div', 'menu-rule');
  const template = el('button', 'menu-row compact');
  template.type = 'button'; template.setAttribute('role', 'menuitemcheckbox');
  template.append(el('span', 'menu-label', 'Into template'));
  let plainFile = false;
  function exportMode() {
    const has = Boolean(state.design?.template), on = has && !plainFile;
    template.hidden = !has;
    syncQuick();
    template.replaceChildren(el('span', 'menu-label', 'Into template'), el('span', 'menu-hint', state.design?.name ?? ''));
    if (on) template.append(glyph('check'));
    template.classList.toggle('on', on);
    template.setAttribute('aria-checked', String(on));
  }
  template.addEventListener('click', () => { plainFile = !plainFile; exportMode(); });
  // "Save to exports/ without asking": the quick export, remembered on this device.
  const quickExport = () => { try { return localStorage.getItem('nexus-quick-export') === '1'; } catch { return false; } };
  const quick = el('button', 'menu-extra');
  quick.type = 'button'; quick.setAttribute('role', 'menuitemcheckbox');
  function syncQuick() {
    const on = quickExport();
    quick.replaceChildren(el('span', 'menu-label', 'Save to exports/ without asking'));
    if (on) quick.append(glyph('check'));
    quick.classList.toggle('on', on);
    quick.setAttribute('aria-checked', String(on));
  }
  quick.addEventListener('click', () => { try { localStorage.setItem('nexus-quick-export', quickExport() ? '0' : '1'); } catch { /* storage may be unavailable */ } syncQuick(); });
  syncQuick();
  exportPop.append(templateRule, template, quick);

  const morePop = el('div', 'popup menu more-pop');
  morePop.hidden = true;
  morePop.setAttribute('role', 'menu');
  morePop.setAttribute('aria-label', 'More');
  const moreItem = (label, hint, run, className = '') => {
    const row = el('button', `menu-row compact ${className}`.trim());
    row.type = 'button'; row.setAttribute('role', 'menuitem');
    row.append(el('span', 'menu-label', label));
    if (hint) row.append(el('span', 'menu-hint', hint));
    // Focus goes back to the button, so the doc's keys (Esc, ⌘\\) keep working.
    row.addEventListener('click', () => { closePops(); moreButton.focus(); run(); });
    morePop.append(row);
    return row;
  };
  const undoItem = moreItem('Undo', '⌘Z', undo);
  const redoItem = moreItem('Redo', '⇧⌘Z', redo);
  morePop.append(el('div', 'menu-rule'));
  moreItem('Version history', '', () => openSidebar('history'), 'narrow-only');
  moreItem('Design', '', () => openSidebar('design'), 'narrow-only');
  const pagesItem = moreItem('Page preview', '', () => setPreview(!state.preview));
  moreItem('Save a copy…', '.doc.json', () => command({ command: 'SaveCopy' }));
  moreItem('Library', '⌘O', () => shared.library.show());
  overlay.append(exportPop, morePop);

  function closePops() {
    for (const [pop, button] of [[exportPop, exportButton], [morePop, moreButton]]) { pop.hidden = true; button.setAttribute('aria-expanded', 'false'); }
  }
  function openPop(pop, button) {
    const show = pop.hidden;
    closePops();
    if (!show) return;
    if (pop === exportPop) exportMode();
    if (pop === morePop) {
      undoItem.disabled = !state.canUndo; redoItem.disabled = !state.canRedo;
      pagesItem.querySelector('.menu-label').textContent = state.preview ? 'Back to editing' : 'Page preview';
    }
    pop.hidden = false;
    button.setAttribute('aria-expanded', 'true');
    const rect = button.getBoundingClientRect();
    pop.style.top = `${Math.round(rect.bottom + 6)}px`;
    pop.style.left = `${Math.round(Math.max(8, Math.min(innerWidth - pop.offsetWidth - 8, rect.right - pop.offsetWidth)))}px`;
    pop.querySelector('button:not(:disabled):not([hidden])')?.focus();
  }
  exportButton.addEventListener('click', () => openPop(exportPop, exportButton));
  moreButton.addEventListener('click', () => openPop(morePop, moreButton));
  for (const [pop, button] of [[exportPop, exportButton], [morePop, moreButton]]) {
    pop.addEventListener('keydown', e => {
      const items = [...pop.querySelectorAll('button:not(:disabled)')].filter(b => b.offsetParent), at = items.indexOf(document.activeElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); items[(at + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length]?.focus(); }
      else if (e.key === 'Escape') { e.preventDefault(); closePops(); button.focus(); }
    });
  }
  document.addEventListener('pointerdown', e => {
    if (!e.target.closest('.export-pop, .more-pop, .top-icon')) closePops();
  }, { signal: lifetime.signal });

  templateDrop(command, toast, lifetime.signal);

  // -- page preview -------------------------------------------------------------------------------------------

  function requestPages() { command({ command: 'DocPages' }); }
  function setPreview(on) {
    if (on) leaveVersion();
    state.preview = on;
    page.hidden = on; previewPane.hidden = !on;
    api.toolbar.hide();
    if (on) { apply([]); previewPane.replaceChildren(el('p', 'preview-note', 'Laying out pages…')); requestPages(); }
    else surface.focus({ preventScroll: true });
  }
  function showPages(event) {
    if (!state.preview) return;
    state.pagesRevision = event.revision;
    previewPane.replaceChildren();
    const column = el('div', 'preview-column');
    const bar = el('div', 'preview-bar');
    bar.append(el('span', '', `${event.pages.length} page${event.pages.length === 1 ? '' : 's'}`), el('span', 'muted', 'Read-only · fit to width · Esc to edit'));
    const back = el('button', 'quiet', 'Back to editing');
    back.type = 'button';
    back.addEventListener('click', () => setPreview(false));
    bar.append(back);
    column.append(bar);
    for (const p of event.pages) {
      const figure = el('figure', 'preview-page');
      const img = el('img');
      img.src = p.src;
      img.alt = `Page ${p.n} of ${event.pages.length}`;
      img.style.aspectRatio = `${p.width} / ${p.height}`;
      img.draggable = false;
      figure.append(img, el('figcaption', '', `${p.n} / ${event.pages.length}`));
      column.append(figure);
    }
    previewPane.append(column);
  }

  // -- images and links -----------------------------------------------------------------------------------------

  const chooser = el('input');
  chooser.type = 'file'; chooser.accept = 'image/png,image/jpeg,image/webp'; chooser.multiple = true; chooser.hidden = true;
  app.append(chooser);
  let chooserAfter = null;
  chooser.addEventListener('change', () => { const files = [...chooser.files]; chooser.value = ''; insertImages(files, chooserAfter); });
  function chooseImage(afterId) { chooserAfter = afterId; chooser.click(); }
  const hex = bytes => { let out = ''; for (let i = 0; i < bytes.length; i += 8192) out += Array.from(bytes.subarray(i, i + 8192), b => b.toString(16).padStart(2, '0')).join(''); return out; };
  async function insertImages(files, afterId) {
    let after = afterId ?? state.doc.blocks.at(-1)?.id ?? null;
    for (const file of files) {
      if (file.size > 8 * 1024 * 1024) { toast(`${file.name || 'The image'} is larger than 8 MB.`); continue; }
      try {
        const data = hex(new Uint8Array(await file.arrayBuffer()));
        const asset = await ask({ command: 'DocImportAsset', name: file.name || 'pasted image.png', data });
        const result = E.insertBlock(state.doc, after, { kind: 'image', src: asset.src, alt: (file.name || '').replace(/\.[a-z]+$/i, ''), caption: '', width: 'full' });
        apply(result.ops, { select: result.select });
        after = result.id;
      } catch (error) { toast(error.message); }
    }
  }
  async function copyBlockLink(id) {
    const text = `${state.relative}#${id}`;
    try { await navigator.clipboard.writeText(text); }
    catch {
      const area = el('textarea'); area.value = text; document.body.append(area); area.select();
      try { document.execCommand('copy'); } catch { /* the toast still names the link */ }
      area.remove();
    }
    toast(`Copied link to block ${id}`);
  }

  // -- keyboard and focus -------------------------------------------------------------------------------------------

  // On the document, not the doc root: a click that re-renders the focused
  // control (a version row, a menu item) leaves focus on <body>.
  document.addEventListener('keydown', e => {
    if (e.target !== document.body && !app.contains(e.target)) return;
    if (document.querySelector('dialog[open]') || !document.querySelector('#library')?.hidden) return;
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === 's') { e.preventDefault(); toast('Every change is saved to the doc file as you make it.'); return; }
    if (mod && ['=', '+', '-', '0'].includes(e.key)) { e.preventDefault(); setZoom(e.key === '0' ? 1.2 : state.zoom + (e.key === '-' ? -.1 : .1)); return; }
    if (mod && e.key === '\\') { e.preventDefault(); toggleOutline(); return; }
    if (e.key === 'Escape') {
      if (state.preview) { e.preventDefault(); setPreview(false); return; }
      if (historyView) { e.preventDefault(); leaveVersion(); return; }
      if (!exportPop.hidden || !morePop.hidden) { closePops(); return; }
      if (state.activeThread || state.draft) comments.close();
    }
  }, { signal: lifetime.signal });
  let lastFocus = '', focusTimer = 0;
  document.addEventListener('selectionchange', () => {
    if (!state.doc) return;
    const sel = editor.selection();
    for (const node of surface.querySelectorAll('.has-caret')) node.classList.remove('has-caret');
    if (sel?.collapsed) surface.querySelector(`[data-block="${CSS.escape(sel.from.block)}"]`)?.classList.add('has-caret');
    if (!state.linked || !sel) return;
    const selected = sel.collapsed ? null : getSelection().toString().slice(0, 300) || null;
    const key = JSON.stringify([sel.from.block, selected]);
    if (key === lastFocus) return;
    lastFocus = key;
    clearTimeout(focusTimer);
    focusTimer = setTimeout(() => command({ command: 'DocFocus', block: sel.from.block, selection: selected }), 300);
  }, { signal: lifetime.signal });
  window.addEventListener('blur', () => { if (editor.hasDirty()) apply([], { typing: true }); }, { signal: lifetime.signal });
  new ResizeObserver(() => { comments.layout(); api.menus.reposition(); }).observe(page);

  // -- wiring ----------------------------------------------------------------------------------------------------------

  const api = {
    signal: lifetime.signal, state, surface, overlay, margin, page, apply, command, toast, undo, redo, offsetIn, copyBlockLink, chooseImage, insertImages,
    scrollTo, scrollToBlock, renderLink, lastCaret: null,
    afterRender() { renderSave(); },
    renderSave,
    openSidebar,
    get sidebarTab() { return sideTab; },
    setCommentCount(n) {
      commentCount.textContent = n ? String(n) : '';
      tabButtons.comments.querySelector('.tab-count').textContent = n ? String(n) : '';
      commentsButton.setAttribute('aria-label', n ? `Comments, ${n} open` : 'Comments');
      if (n && !sideChosen && !sideTab && matchMedia('(min-width: 1100px)').matches) { sideTab = 'comments'; renderSidebar(); }
    },
  };
  const editor = editorController(api);
  api.editor = editor;
  api.menus = menusController(api);
  api.toolbar = toolbarController(api);
  api.comments = commentsController(api);
  const comments = api.comments;
  renderSidebar();
  renderOutlinePanel();

  return {
    handle,
    unmount() {
      lifetime.abort();
      app.hidden = true;
      app.replaceChildren();
      queue.length = 0;
    },
    api,
  };
}
