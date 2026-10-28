// The Library: every deck and doc of the space as a grid of cards. It is a
// full-window layer above both modes (deck and doc), so it reaches the backend
// through whichever mode's own command queue is live. It holds no path of its
// own: a card names its file by the path inside the space that the backend
// listed, and the backend checks it again.
import { icon } from './icons.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const button = (className, label, iconName, text) => {
  const node = el('button', className);
  node.type = 'button';
  if (iconName) node.append(icon(iconName));
  if (text) node.append(el('span', '', text));
  if (label) node.setAttribute('aria-label', label);
  return node;
};

const DECK_FORMATS = [['pptx', 'PowerPoint', '.pptx'], ['pdf', 'PDF', '.pdf'], ['png', 'PNG images', '.zip'], ['html', 'Web presentation', '.html'], ['json', 'Deck JSON', '.json']];
const DOC_FORMATS = [['docx', 'Word', '.docx'], ['pdf', 'PDF', '.pdf'], ['md', 'Markdown', '.md'], ['html', 'Web page', '.html']];
const KIND_LABEL = { deck: 'Deck', doc: 'Doc' };

const platform = () => navigator.userAgentData?.platform || navigator.platform || '';
export const revealLabel = () => /mac/i.test(platform()) ? 'Reveal in Finder' : /win/i.test(platform()) ? 'Show in Explorer' : 'Show in folder';

/** "just now", "5 minutes ago", "yesterday", "3 days ago", then the date. */
export function ago(iso, now = Date.now()) {
  const seconds = Math.round((now - Date.parse(iso)) / 1000);
  const unit = (n, name) => `${n} ${name}${n === 1 ? '' : 's'} ago`;
  if (!Number.isFinite(seconds) || seconds < 60) return 'just now';
  if (seconds < 3600) return unit(Math.floor(seconds / 60), 'minute');
  if (seconds < 86400) return unit(Math.floor(seconds / 3600), 'hour');
  if (seconds < 172800) return 'yesterday';
  if (seconds < 2592000) return unit(Math.floor(seconds / 86400), 'day');
  return new Date(iso).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
}
const count = item => `${item.count} ${item.kind === 'deck' ? 'slide' : 'block'}${item.count === 1 ? '' : 's'}`;

/**
 * `command(cmd, complete)` sends one IPC command through the live mode's queue.
 * `toast(text, view, actions)` is the mode's own notice, used while the library is hidden.
 */
export function libraryController({ command, toast: modeToast }) {
  const root = document.querySelector('#library');
  const state = {
    items: [], current: null, filter: 'all', query: '', visible: false, loaded: false,
    awaiting: false, home: false, shownForHome: false, selected: null,
  };
  let menu = null, renaming = null, lastFocus = null, toastTimer = 0, freshTimer = 0;
  const inertTargets = () => ['#topbar', 'main', '#doc-app'].map(s => document.querySelector(s)).filter(Boolean);

  // -- the shell -------------------------------------------------------------------------------

  const top = el('header', 'lib-top');
  const brand = el('div', 'brand');
  brand.append('nexus ', el('span', '', 'studio'));
  const openFile = button('lib-action', 'Open a file from disk', 'folder', 'Open…');
  const newDoc = button('lib-action', 'New doc', 'file-doc', 'New doc');
  const newDeck = button('lib-action primary', 'New deck', 'plus', 'New deck');
  openFile.title = 'Open a deck, doc, PowerPoint, Word or Markdown file';
  newDoc.title = 'New doc'; newDeck.title = 'New deck';
  const close = button('icon-button lib-close', 'Close the library', 'x');
  close.title = 'Close (Esc)';
  top.append(brand, el('span', 'spacer'), openFile, newDoc, newDeck, close);

  const scroll = el('div', 'lib-scroll');
  const page = el('div', 'lib-page');
  const head = el('div', 'lib-head');
  const title = el('h1', 'lib-title-main', 'Library');
  const counter = el('span', 'lib-count');
  const search = el('input', 'lib-search');
  search.type = 'search'; search.placeholder = 'Search by title'; search.autocomplete = 'off'; search.spellcheck = false;
  search.setAttribute('aria-label', 'Search the library by title');
  const searchBox = el('label', 'lib-searchbox');
  searchBox.append(icon('search'), search);
  const tabs = el('div', 'lib-tabs');
  tabs.setAttribute('role', 'group'); tabs.setAttribute('aria-label', 'Show');
  const tabButtons = {};
  for (const [id, label] of [['all', 'All'], ['deck', 'Decks'], ['doc', 'Docs']]) {
    const tab = button('lib-tab', null, null);
    tab.dataset.filter = id;
    tab.append(el('span', '', label), el('span', 'lib-tab-count'));
    tab.addEventListener('click', () => { state.filter = id; renderTabs(); renderGrid(); });
    tabs.append(tab); tabButtons[id] = tab;
  }
  head.append(title, counter, el('span', 'spacer'), searchBox, tabs);
  const grid = el('div', 'lib-grid');
  grid.setAttribute('role', 'list');
  grid.setAttribute('aria-label', 'Decks and docs');
  const empty = el('div', 'lib-empty');
  page.append(head, grid, empty);
  scroll.append(page);

  const toastNode = el('div', 'lib-toast');
  toastNode.setAttribute('role', 'status'); toastNode.hidden = true;

  const confirm = document.createElement('dialog');
  confirm.className = 'lib-dialog';
  confirm.setAttribute('aria-labelledby', 'lib-confirm-heading');
  const confirmForm = el('form');
  confirmForm.method = 'dialog';
  const confirmHeading = el('h2'); confirmHeading.id = 'lib-confirm-heading';
  const confirmText = el('p');
  const confirmActions = el('div', 'dialog-actions');
  const cancelDelete = button('', null, null, 'Cancel'); cancelDelete.value = 'cancel'; cancelDelete.type = 'submit';
  const doDelete = button('danger', null, null, 'Delete'); doDelete.value = 'delete'; doDelete.type = 'submit';
  confirmActions.append(cancelDelete, doDelete);
  confirmForm.append(confirmHeading, confirmText, confirmActions);
  confirm.append(confirmForm);

  root.replaceChildren(top, scroll, toastNode, confirm);
  root.setAttribute('role', 'dialog'); root.setAttribute('aria-label', 'Library');

  // -- commands -----------------------------------------------------------------------------------

  /** Send a command; when it did not answer with a fresh listing, ask for one. */
  const run = (cmd, then) => command(cmd, event => {
    then?.(event);
    if (event.event !== 'Library') command({ command: 'Library' });
  });
  const open = item => { state.awaiting = true; command({ command: 'LibraryOpen', path: item.relative }); };
  const create = kind => { state.awaiting = true; command({ command: kind === 'deck' ? 'NewDeck' : 'NewDoc', title: 'Untitled' }); };
  newDeck.addEventListener('click', () => create('deck'));
  newDoc.addEventListener('click', () => create('doc'));
  openFile.addEventListener('click', () => { state.awaiting = true; command({ command: 'OpenDialog' }); });
  close.addEventListener('click', () => hide());

  const exportItem = (item, format) => command({ command: 'LibraryExport', path: item.relative, format });
  const reveal = item => command({ command: 'LibraryReveal', path: item.relative });
  const duplicate = item => command({ command: 'LibraryDuplicate', path: item.relative });
  function askDelete(item) {
    confirmHeading.textContent = `Delete “${item.title}”?`;
    confirmText.textContent = 'It moves to the .trash folder of your space files, with its comments and history, so you can still get it back.';
    confirm.returnValue = '';
    confirm.addEventListener('close', () => {
      if (confirm.returnValue !== 'delete') return;
      run({ command: 'LibraryDelete', path: item.relative }, () => {
        // The open file went: the window falls back to its start page.
        if (item.current) command({ command: 'Load' });
      });
    }, { once: true });
    confirm.showModal();
    cancelDelete.focus();
  }

  // -- the grid ---------------------------------------------------------------------------------------

  function visibleItems() {
    const q = state.query.trim().toLowerCase();
    return state.items.filter(i => (state.filter === 'all' || i.kind === state.filter) && (!q || i.title.toLowerCase().includes(q)));
  }
  function renderTabs() {
    const totals = { all: state.items.length, deck: state.items.filter(i => i.kind === 'deck').length, doc: state.items.filter(i => i.kind === 'doc').length };
    for (const [id, tab] of Object.entries(tabButtons)) {
      tab.setAttribute('aria-pressed', String(state.filter === id));
      tab.querySelector('.lib-tab-count').textContent = String(totals[id]);
    }
  }
  function card(item) {
    const node = el('article', 'lib-card');
    node.setAttribute('role', 'listitem');
    node.dataset.path = item.relative;
    node.classList.toggle('current', item.current);
    const opener = el('button', 'lib-open');
    opener.type = 'button';
    opener.setAttribute('aria-label', `Open ${item.title}, ${KIND_LABEL[item.kind].toLowerCase()}, ${count(item)}, ${ago(item.modified)}`);
    const thumb = el('span', `lib-thumb ${item.kind}`);
    const placeholder = el('span', 'lib-placeholder');
    placeholder.append(icon(item.kind === 'deck' ? 'file-deck' : 'file-doc'));
    thumb.append(placeholder);
    if (item.thumb) {
      const image = el('img');
      image.alt = ''; image.loading = 'lazy'; image.decoding = 'async'; image.draggable = false; image.src = item.thumb;
      image.addEventListener('load', () => node.classList.add('loaded'));
      image.addEventListener('error', () => image.remove());
      thumb.append(image);
    }
    thumb.append(el('span', 'lib-badge', KIND_LABEL[item.kind]));
    opener.append(thumb);
    opener.addEventListener('click', () => open(item));
    const more = button('lib-more', `More actions for ${item.title}`, 'more');
    more.setAttribute('aria-haspopup', 'menu'); more.setAttribute('aria-expanded', 'false');
    more.addEventListener('click', event => { event.stopPropagation(); toggleMenu(item, more); });
    const meta = el('div', 'lib-meta');
    const name = el('button', 'lib-title', item.title);
    name.type = 'button'; name.title = item.title;
    name.addEventListener('click', () => open(item));
    const sub = el('div', 'lib-sub');
    const time = el('time', '', ago(item.modified));
    time.dateTime = item.modified;
    sub.append(el('span', '', count(item)), el('span', 'lib-dot', '·'), time);
    if (item.current) sub.append(el('span', 'lib-dot', '·'), el('span', 'lib-now', 'Open now'));
    meta.append(name, sub);
    node.append(opener, more, meta);
    node.addEventListener('contextmenu', event => { event.preventDefault(); toggleMenu(item, more, { x: event.clientX, y: event.clientY }); });
    return node;
  }
  function renderGrid() {
    const items = visibleItems();
    grid.replaceChildren(...items.map(card));
    grid.hidden = !items.length;
    const total = state.items.length;
    head.classList.toggle('bare', state.loaded && !total);
    counter.textContent = !total ? '' : items.length === total ? `${total} ${total === 1 ? 'file' : 'files'}` : `${items.length} of ${total}`;
    empty.replaceChildren();
    empty.hidden = Boolean(items.length) || !state.loaded;
    if (!empty.hidden) {
      if (!total) {
        empty.append(icon('library'), el('h2', '', 'Nothing in your library yet'),
          el('p', '', 'Decks and docs you make here, or ask nexus to draft in the conversation, collect in this list.'));
        const actions = el('div', 'lib-empty-actions');
        const doc = button('', null, 'file-doc', 'New doc'), deck = button('primary', null, 'plus', 'New deck');
        doc.addEventListener('click', () => create('doc')); deck.addEventListener('click', () => create('deck'));
        actions.append(doc, deck); empty.append(actions);
      } else {
        const what = state.filter === 'all' ? 'decks or docs' : state.filter === 'deck' ? 'decks' : 'docs';
        const q = state.query.trim();
        empty.append(icon('search'), el('h2', '', q ? `No ${what} match “${q}”` : `No ${what} yet`));
        const reset = button('', null, null, q ? 'Clear search' : 'Show everything');
        reset.addEventListener('click', () => { state.query = ''; search.value = ''; state.filter = 'all'; renderTabs(); renderGrid(); search.focus(); });
        empty.append(reset);
      }
    }
    close.hidden = state.home && !state.current;
    if (state.selected) {
      const fresh = grid.querySelector(`[data-path="${CSS.escape(state.selected)}"]`);
      state.selected = null;
      if (fresh) {
        fresh.classList.add('fresh');
        fresh.scrollIntoView({ block: 'nearest' });
        clearTimeout(freshTimer);
        freshTimer = setTimeout(() => fresh.classList.remove('fresh'), 2200);
      }
    }
  }
  search.addEventListener('input', () => { state.query = search.value; renderGrid(); });
  search.addEventListener('keydown', event => {
    if (event.key === 'Escape' && search.value) { event.stopPropagation(); search.value = ''; state.query = ''; renderGrid(); }
    else if (event.key === 'ArrowDown') { event.preventDefault(); grid.querySelector('.lib-open')?.focus(); }
  });

  // Arrow keys move between cards; Enter opens the focused one.
  grid.addEventListener('keydown', event => {
    const cards = [...grid.querySelectorAll('.lib-open')], at = cards.indexOf(document.activeElement);
    if (at < 0 || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const first = cards[0].getBoundingClientRect().top;
    const columns = Math.max(1, cards.filter(c => Math.abs(c.getBoundingClientRect().top - first) < 4).length);
    const next = { ArrowLeft: at - 1, ArrowRight: at + 1, ArrowUp: at - columns, ArrowDown: at + columns, Home: 0, End: cards.length - 1 }[event.key];
    if (next >= 0 && next < cards.length) cards[next].focus();
    else if (event.key === 'ArrowUp') search.focus();
  });

  // -- the actions menu -------------------------------------------------------------------------------

  function closeMenu(refocus = false) {
    if (!menu) return;
    const { node, trigger } = menu;
    menu = null;
    node.remove();
    trigger.setAttribute('aria-expanded', 'false');
    trigger.closest('.lib-card')?.classList.remove('menu-open');
    if (refocus) trigger.focus();
  }
  function row(label, iconName, handler, extra = '') {
    const node = button(`lib-menu-row ${extra}`.trim(), null, iconName, label);
    node.setAttribute('role', 'menuitem'); node.tabIndex = -1;
    node.addEventListener('click', handler);
    return node;
  }
  function toggleMenu(item, trigger, at) {
    if (menu?.trigger === trigger && !at) { closeMenu(true); return; }
    closeMenu();
    const node = el('div', 'lib-menu');
    node.setAttribute('role', 'menu'); node.setAttribute('aria-label', `Actions for ${item.title}`);
    const act = fn => () => { closeMenu(); fn(); };
    const cardNode = trigger.closest('.lib-card');
    const sub = el('div', 'lib-submenu');
    sub.hidden = true; sub.setAttribute('role', 'group');
    const exportRow = row('Export', 'export', () => {
      const expanded = exportRow.getAttribute('aria-expanded') === 'true';
      exportRow.setAttribute('aria-expanded', String(!expanded));
      sub.hidden = expanded;
      place();
    });
    exportRow.setAttribute('aria-haspopup', 'true'); exportRow.setAttribute('aria-expanded', 'false');
    exportRow.append(icon('chevron'));
    for (const [format, label, hint] of item.kind === 'deck' ? DECK_FORMATS : DOC_FORMATS) {
      const r = row(label, null, act(() => exportItem(item, format)), 'sub');
      r.append(el('span', 'lib-hint', hint));
      sub.append(r);
    }
    node.append(
      row('Open', `file-${item.kind}`, act(() => open(item))),
      row(revealLabel(), 'reveal', act(() => reveal(item))),
      row('Duplicate', 'copy', act(() => duplicate(item))),
      row('Rename', 'rename', act(() => beginRename(item, cardNode))),
      exportRow, sub, el('div', 'lib-menu-rule'),
      row('Delete', 'trash', act(() => askDelete(item)), 'destructive'),
    );
    root.append(node);
    menu = { node, trigger, item };
    trigger.setAttribute('aria-expanded', 'true');
    cardNode?.classList.add('menu-open');
    function place() {
      const rect = at ? { left: at.x, right: at.x, top: at.y, bottom: at.y } : trigger.getBoundingClientRect();
      const width = node.offsetWidth, height = node.offsetHeight;
      let left = at ? rect.left : rect.right - width, topPx = rect.bottom + 6;
      if (topPx + height > innerHeight - 8) topPx = Math.max(8, rect.top - height - 6);
      left = Math.max(8, Math.min(innerWidth - width - 8, left));
      node.style.left = `${Math.round(left)}px`; node.style.top = `${Math.round(topPx)}px`;
    }
    place();
    node.querySelector('button').focus();
    node.addEventListener('keydown', event => {
      const rows = [...node.querySelectorAll('button')].filter(b => !b.closest('[hidden]')), i = rows.indexOf(document.activeElement);
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); rows[(i + (event.key === 'ArrowDown' ? 1 : -1) + rows.length) % rows.length].focus(); }
      else if (event.key === 'Home' || event.key === 'End') { event.preventDefault(); rows[event.key === 'Home' ? 0 : rows.length - 1].focus(); }
      else if (event.key === 'Tab') closeMenu();
    });
  }
  document.addEventListener('pointerdown', event => { if (menu && !event.target.closest('.lib-menu, .lib-more')) closeMenu(); });
  scroll.addEventListener('scroll', () => closeMenu());
  window.addEventListener('resize', () => closeMenu());

  // -- rename in place -----------------------------------------------------------------------------------

  function beginRename(item, cardNode) {
    const name = cardNode?.querySelector('.lib-title');
    if (!name) return;
    const input = el('input', 'lib-rename');
    input.value = item.title; input.maxLength = 200; input.spellcheck = false;
    input.setAttribute('aria-label', `New title for ${item.title}`);
    name.replaceWith(input);
    renaming = input;
    input.focus(); input.select();
    let finished = false;
    const finish = commit => {
      if (finished) return;
      finished = true; renaming = null;
      const value = input.value.trim();
      input.replaceWith(name);
      if (commit && value && value !== item.title) run({ command: 'LibraryRename', path: item.relative, title: value });
      else name.focus();
    };
    input.addEventListener('keydown', event => {
      event.stopPropagation();
      if (event.key === 'Enter') { event.preventDefault(); finish(true); }
      else if (event.key === 'Escape') { event.preventDefault(); finish(false); }
    });
    input.addEventListener('blur', () => finish(true));
  }

  // -- showing and hiding ---------------------------------------------------------------------------------

  function show() {
    if (state.visible) { search.focus(); return; }
    state.visible = true; state.awaiting = false;
    lastFocus = document.activeElement;
    root.hidden = false;
    for (const node of inertTargets()) node.inert = true;
    renderTabs(); renderGrid();
    command({ command: 'Library' });
    requestAnimationFrame(() => search.focus({ preventScroll: true }));
  }
  function hide() {
    if (!state.visible || (state.home && !state.current)) return;
    closeMenu();
    state.visible = false; state.awaiting = false;
    root.hidden = true;
    for (const node of inertTargets()) node.inert = false;
    try { lastFocus?.focus?.({ preventScroll: true }); } catch { /* the element may be gone */ }
  }
  const toggle = () => (state.visible ? hide() : show());

  // The layer owns the keyboard while it is open: nothing reaches the canvas or the editor.
  root.addEventListener('keydown', event => {
    event.stopPropagation();
    if (event.key === 'Escape' && !confirm.open && !renaming) {
      event.preventDefault();
      if (menu) closeMenu(true); else hide();
    }
  });
  document.addEventListener('keydown', event => {
    if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === 'o') {
      event.preventDefault(); event.stopPropagation();
      toggle();
    }
  }, true);

  // -- notices ------------------------------------------------------------------------------------------------------

  function notice(text, actions = []) {
    toastNode.replaceChildren(document.createTextNode(text));
    for (const [label, handler] of actions) {
      const action = el('button', 'quiet', label);
      action.type = 'button';
      action.addEventListener('click', () => { toastNode.hidden = true; handler(); });
      toastNode.append(action);
    }
    toastNode.hidden = !text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toastNode.hidden = true; }, actions.length ? 10000 : 5000);
  }

  return {
    get visible() { return state.visible; },
    show, hide, toggle,
    /** A notice from the mode, shown above the library while it is open. */
    toast(text, actions = []) { if (state.visible) notice(text, actions); else modeToast(text, null, actions); },
    /** A deck or doc event: a start page opens the library; a document ends the wait. */
    onDocument(event) {
      state.home = Boolean(event.home);
      if (event.home && !state.shownForHome) { state.shownForHome = true; show(); return; }
      if (!event.home) state.shownForHome = true;
      if (state.visible && state.awaiting) hide();
    },
    onEvent(event) {
      if (event.event === 'Library') {
        state.items = event.items; state.current = event.current; state.loaded = true; state.home = event.home;
        state.selected = event.selected || null;
        renderTabs(); renderGrid();
        if (event.notice && state.visible) notice(event.notice);
      } else if (event.event === 'Idle' || event.event === 'Error') state.awaiting = false;
    },
  };
}
