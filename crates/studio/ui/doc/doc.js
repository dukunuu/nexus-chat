// Doc mode: a calm writing surface with the document as the hero. The chrome is
// a slim bar, an outline on the left and comments in the margin; formatting
// appears over selected text only. The page content follows the doc's own design
// (fonts, colors, point sizes); the chrome follows the terminal theme.
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
  const outlineToggle = el('button', 'icon-button quiet outline-toggle');
  outlineToggle.type = 'button'; outlineToggle.append(glyph('outline'));
  outlineToggle.setAttribute('aria-label', 'Toggle outline'); outlineToggle.title = 'Outline';
  const brand = el('div', 'doc-brand');
  brand.append(el('span', 'brand-word', 'nexus'), el('span', 'brand-sub', 'docs'));
  const titleInput = el('input', 'doc-title-input');
  titleInput.type = 'text'; titleInput.placeholder = 'Untitled'; titleInput.maxLength = 200; titleInput.spellcheck = false;
  titleInput.setAttribute('aria-label', 'Document title');
  const saveState = el('span', 'save-state', 'Saved');
  const pill = el('span', 'link-pill', 'not linked');
  function barButton(name, label, iconOnly = false, chevron = false) {
    const b = el('button', iconOnly ? 'icon-button quiet' : 'bar-button');
    b.type = 'button';
    b.append(glyph(name));
    if (!iconOnly) b.append(el('span', '', label));
    if (chevron) b.append(glyph('down'));
    b.setAttribute('aria-label', label);
    if (iconOnly) b.title = label;
    return b;
  }
  const undoButton = barButton('undo', 'Undo', true), redoButton = barButton('redo', 'Redo', true);
  const designButton = barButton('design', 'Design'), previewButton = barButton('pages', 'Pages'), exportButton = barButton('export', 'Export', false, true);
  designButton.setAttribute('aria-haspopup', 'dialog'); exportButton.setAttribute('aria-haspopup', 'menu');
  previewButton.setAttribute('aria-pressed', 'false');
  top.append(outlineToggle, brand, titleInput, saveState, el('span', 'spacer'), undoButton, redoButton, pill, designButton, previewButton, exportButton);

  const body = el('div', 'doc-body');
  const outline = el('nav', 'doc-outline');
  outline.setAttribute('aria-label', 'Outline');
  const page = el('div', 'page-scroll');
  const grid = el('div', 'page-grid');
  const sheetColumn = el('div', 'sheet-column');
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
  sheet.addEventListener('mousedown', e => { if (e.target === sheet) { e.preventDefault(); editor.focusEnd(); } });
  const margin = el('aside', 'doc-margin');
  margin.setAttribute('aria-label', 'Comments');
  grid.append(sheetColumn, margin);
  page.append(grid);
  const previewPane = el('div', 'preview-pane');
  previewPane.hidden = true;
  body.append(outline, page, previewPane);
  const overlay = el('div', 'doc-overlay');
  const toastNode = el('div', 'doc-toast');
  toastNode.setAttribute('role', 'status');
  toastNode.hidden = true;
  app.append(top, body, overlay, toastNode);

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
  function renderSave() { saveState.textContent = pending?.ops || editor.hasDirty() || queue.some(q => q.ops) ? 'Saving' : 'Saved'; }

  function toast(text, actions = []) {
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
      case 'TemplateImported':
        toast(`Imported ${event.name}`, [['Use for this doc', () => command({ command: 'ApplyDesign', design: event.slug })], ...(event.warnings?.length ? [['Warnings', () => toast(event.warnings.join(' · '))]] : [])]);
        command({ command: 'DocDesigns' });
        break;
      case 'Pages': showPages(event); break;
      case 'Theme': shared.applyTheme(event.colors); break;
      case 'Exported': {
        const actions = [['Reveal', () => command({ command: 'Reveal', path: event.path })]];
        if (event.warnings?.length) actions.push([`${event.warnings.length} approximations`, () => toast(event.warnings.join(' · '))]);
        toast(`Exported ${event.path.split(/[\\/]/).pop()}`, actions);
        break;
      }
      case 'Error':
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
    if (first) surface.focus({ preventScroll: true });
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
    const theme = state.doc.theme, set = (name, value) => sheet.style.setProperty(name, value);
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
    sheet.style.setProperty('--z', String(state.zoom));
    try { localStorage.setItem('nexus-doc-zoom', String(state.zoom)); } catch { /* ignore */ }
    comments.layout();
  }

  // -- the chrome ------------------------------------------------------------------------------------

  function renderChrome() {
    if (document.activeElement !== titleInput) titleInput.value = state.doc.title;
    sizeTitle();
    document.title = `${state.doc.title || 'Untitled'} — nexus studio`;
    undoButton.disabled = !state.canUndo; redoButton.disabled = !state.canRedo;
    renderLink();
    renderDesign();
  }
  function renderLink() {
    const activities = Object.values(state.activities);
    const working = activities.some(a => ['working', 'progress'].includes(a.state)), waiting = activities.filter(a => a.state === 'queued').length;
    pill.textContent = !state.linked ? 'not linked' : working ? 'linked · nexus working' : waiting ? `linked · ${waiting} waiting` : 'linked to nexus';
    pill.classList.toggle('connected', state.linked);
    pill.classList.toggle('working', state.linked && working);
    pill.title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  }

  const sizeTitle = () => { titleInput.style.width = `${Math.min(60, Math.max(10, titleInput.value.length + 2))}ch`; };
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
  undoButton.addEventListener('click', undo);
  redoButton.addEventListener('click', redo);

  // -- outline ------------------------------------------------------------------------------------------

  function renderOutline() {
    const entries = M.outline(state.doc);
    outline.replaceChildren();
    outline.append(el('div', 'outline-heading', 'Outline'));
    if (!entries.length) { outline.append(el('p', 'outline-empty', 'Headings appear here.')); return; }
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
    outline.append(list);
    markCurrent();
  }
  function markCurrent() {
    const links = [...outline.querySelectorAll('[data-goto]')];
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
  outlineToggle.addEventListener('click', () => outline.classList.toggle('open'));

  function scrollTo(node) { node.scrollIntoView({ block: 'center', behavior: reduced() ? 'auto' : 'smooth' }); }
  function scrollToBlock(id, flashIt = false) {
    const node = surface.querySelector(`[data-block="${CSS.escape(id)}"]`);
    if (!node) return;
    scrollTo(node);
    if (flashIt) flash([id]);
  }

  // -- design panel ---------------------------------------------------------------------------------------

  const designs = { available: null };
  const designPop = el('div', 'popup design-pop');
  designPop.hidden = true;
  designPop.setAttribute('role', 'dialog');
  designPop.setAttribute('aria-label', 'Design');
  overlay.append(designPop);
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
    if (designPop.hidden || !state.design) return;
    designPop.replaceChildren();
    const current = el('div', 'design-current');
    current.append(el('div', 'design-name', state.design.name), swatchRow(state.design.colors));
    const fonts = el('div', 'design-fonts');
    for (const role of ['heading', 'body', 'mono']) if (state.design.fonts[role]) fonts.append(el('span', '', `${role[0].toUpperCase()}${role.slice(1)} · ${state.design.fonts[role]}`));
    current.append(fonts);
    designPop.append(el('div', 'pop-eyebrow', 'This document follows'), current);
    if (state.design.update_available) {
      const update = el('div', 'design-update');
      update.append(el('span', '', 'Update available'));
      const apply1 = el('button', 'primary small', 'Apply update');
      apply1.type = 'button';
      apply1.addEventListener('click', () => command({ command: 'DocApplyDesign', design: null }));
      update.append(apply1);
      designPop.append(update);
    }
    designPop.append(el('div', 'pop-eyebrow', 'Switch design'));
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
    designPop.append(list);
    const add = el('button', 'menu-row compact', 'Add template…');
    add.type = 'button'; add.addEventListener('click', () => chooseTemplate(command, toast));
    designPop.append(add);
    place(designPop, designButton);
  }
  function togglePop(pop, button, open, onOpen) {
    const show = open ?? pop.hidden;
    for (const [other, b] of [[designPop, designButton], [exportPop, exportButton]]) if (other !== pop) { other.hidden = true; b.setAttribute('aria-expanded', 'false'); }
    pop.hidden = !show;
    button.setAttribute('aria-expanded', String(show));
    if (show) onOpen?.();
  }
  function place(pop, button) {
    const rect = button.getBoundingClientRect();
    pop.style.top = `${Math.round(rect.bottom + 8)}px`;
    pop.style.left = `${Math.round(Math.max(8, Math.min(innerWidth - pop.offsetWidth - 8, rect.right - pop.offsetWidth)))}px`;
  }
  designButton.addEventListener('click', () => togglePop(designPop, designButton, undefined, () => { command({ command: 'DocDesigns' }); renderDesign(); }));

  // -- export -----------------------------------------------------------------------------------------------

  const exportPop = el('div', 'popup menu export-pop');
  exportPop.hidden = true;
  exportPop.setAttribute('role', 'menu');
  exportPop.setAttribute('aria-label', 'Export');
  for (const [format, label, hint] of [['docx', 'Word document', '.docx'], ['pdf', 'PDF', '.pdf'], ['md', 'Markdown', '.md'], ['html', 'Web page', '.html']]) {
    const row = el('button', 'menu-row compact');
    row.type = 'button'; row.setAttribute('role', 'menuitem');
    row.append(el('span', 'menu-label', label), el('span', 'menu-hint', hint));
    row.addEventListener('click', () => { togglePop(exportPop, exportButton, false); command({ command: 'DocExport', format, ...(plainFile ? { template: 'none' } : {}) }); });
    exportPop.append(row);
  }
  const template = el('button', 'menu-row compact');
  template.type = 'button'; template.setAttribute('role', 'menuitemcheckbox');
  let plainFile = false;
  function exportMode() {
    template.disabled = !state.design?.template;
    template.textContent = `Into template${!plainFile && state.design?.template ? ' · On' : ''}`;
    template.setAttribute('aria-checked', String(!plainFile && !!state.design?.template));
  }
  template.addEventListener('click', () => { plainFile = !plainFile; exportMode(); });
  const plain = el('button', 'menu-row compact', 'Plain file');
  plain.type = 'button'; plain.setAttribute('role', 'menuitem');
  plain.addEventListener('click', () => { togglePop(exportPop, exportButton, false); command({ command: 'DocExport', format: 'docx', template: 'none' }); });
  exportPop.append(el('div', 'menu-rule'), template, plain);
  overlay.append(exportPop);
  exportButton.addEventListener('click', () => togglePop(exportPop, exportButton, undefined, () => { exportMode(); place(exportPop, exportButton); exportPop.querySelector('button')?.focus(); }));
  exportPop.addEventListener('keydown', e => {
    const items = [...exportPop.querySelectorAll('button:not(:disabled)')], at = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); items[(at + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus(); }
    else if (e.key === 'Escape') { e.preventDefault(); togglePop(exportPop, exportButton, false); exportButton.focus(); }
  });
  document.addEventListener('pointerdown', e => {
    if (!e.target.closest('.design-pop, .export-pop, .bar-button')) {
      designPop.hidden = true; exportPop.hidden = true;
      designButton.setAttribute('aria-expanded', 'false'); exportButton.setAttribute('aria-expanded', 'false');
    }
  }, { signal: lifetime.signal });

  templateDrop(command, toast, lifetime.signal);

  // -- page preview -------------------------------------------------------------------------------------------

  function requestPages() { command({ command: 'DocPages' }); }
  function setPreview(on) {
    state.preview = on;
    previewButton.setAttribute('aria-pressed', String(on));
    previewButton.classList.toggle('on', on);
    page.hidden = on; previewPane.hidden = !on;
    api.toolbar.hide();
    if (on) { apply([]); previewPane.replaceChildren(el('p', 'preview-note', 'Laying out pages…')); requestPages(); }
    else surface.focus({ preventScroll: true });
  }
  previewButton.addEventListener('click', () => setPreview(!state.preview));
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

  app.addEventListener('keydown', e => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === 's') { e.preventDefault(); toast('Every change is saved to the doc file as you make it.'); return; }
    if (mod && ['=', '+', '-', '0'].includes(e.key)) { e.preventDefault(); setZoom(e.key === '0' ? 1.2 : state.zoom + (e.key === '-' ? -.1 : .1)); return; }
    if (e.key === 'Escape') {
      if (state.preview) { e.preventDefault(); setPreview(false); return; }
      if (!designPop.hidden || !exportPop.hidden) { designPop.hidden = true; exportPop.hidden = true; return; }
      if (state.activeThread || state.draft) comments.close();
    }
  });
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
    setCommentCount() {},
  };
  const editor = editorController(api);
  api.editor = editor;
  api.menus = menusController(api);
  api.toolbar = toolbarController(api);
  api.comments = commentsController(api);
  const comments = api.comments;

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
