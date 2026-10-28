// Dev-only IPC fixture for the Library. Injected after a mode's own mock (doc-mock.js
// or mock.js) by the harnesses, never embedded or loaded by studio. It answers the
// library and dialog commands the way the backend does and passes the rest down.
(() => {
  const inner = window.ipc;
  const now = Date.now();
  const ago = seconds => new Date(now - seconds * 1000).toISOString();
  const defaults = () => [
    { kind: 'doc', relative: 'docs/q3.doc.json', title: 'Q3 strategy', modified: ago(240), count: 21, design: 'Nexus', thumb: '/library/doc-1.png', current: true },
    { kind: 'deck', relative: 'decks/launch.deck.json', title: 'Launch review', modified: ago(3 * 3600), count: 12, design: 'Nexus', thumb: '/library/deck-1.png', current: false },
    { kind: 'deck', relative: 'decks/board.deck.json', title: 'Board update, September', modified: ago(26 * 3600), count: 18, design: 'Acme', thumb: '/library/deck-2.png', current: false },
    { kind: 'doc', relative: 'docs/research-notes.doc.json', title: 'Research notes on retention and onboarding for the winter cohort', modified: ago(2 * 86400), count: 64, design: 'Nexus', thumb: '/library/doc-2.png', current: false },
    { kind: 'deck', relative: 'decks/all-hands.deck.json', title: 'All hands', modified: ago(5 * 86400), count: 1, design: 'Nexus', thumb: '/library/deck-3.png', current: false },
    { kind: 'doc', relative: 'docs/onboarding.doc.json', title: 'Onboarding guide', modified: ago(9 * 86400), count: 33, design: 'Acme', thumb: '/library/doc-3.png', current: false },
    { kind: 'deck', relative: 'decks/roadmap.deck.json', title: 'Roadmap 2027', modified: ago(14 * 86400), count: 24, design: 'Acme', thumb: '/library/deck-4.png', current: false },
    { kind: 'deck', relative: 'decks/untitled.deck.json', title: 'Untitled', modified: ago(40 * 86400), count: 2, design: null, thumb: null, current: false },
    { kind: 'doc', relative: 'docs/changelog.doc.json', title: 'Changelog', modified: ago(70 * 86400), count: 9, design: 'Nexus', thumb: '/library/doc-4.png', current: false },
  ];
  let items = structuredClone(window.__libraryItems ?? defaults());
  const state = { home: Boolean(window.__libraryHome), current: window.__libraryCurrent === undefined ? 'docs/q3.doc.json' : window.__libraryCurrent, cancel: false, commands: [] };
  window.__library = {
    get items() { return structuredClone(items); },
    set items(value) { items = structuredClone(value); },
    state,
    get commands() { return state.commands; },
  };

  // Deck and doc events carry the start-page flag, as the backend's do.
  const wrap = name => {
    let real;
    Object.defineProperty(window, name, {
      configurable: true,
      set(fn) { real = fn; },
      get() { return event => real(event && (event.event === 'Deck' || event.event === 'Doc') ? { ...event, home: state.home } : event); },
    });
  };
  wrap('studioEvent'); wrap('studioAsyncEvent');

  const listing = (extra = {}) => ({ event: 'Library', items: items.map(i => ({ ...i, current: i.relative === state.current })), current: state.current, home: state.home, selected: null, notice: null, ...extra });
  const reply = event => window.studioEvent(structuredClone(event));
  const find = path => items.find(i => i.relative === path);
  const extension = { pptx: 'pptx', pdf: 'pdf', png: 'zip', html: 'html', json: 'deck.json', docx: 'docx', md: 'md' };
  const LIBRARY = new Set(['Library', 'LibraryOpen', 'LibraryReveal', 'LibraryRename', 'LibraryDuplicate', 'LibraryDelete', 'LibraryExport', 'OpenDialog', 'ExportAs', 'SaveCopy', 'PickTemplate', 'NewDeck', 'NewDoc']);
  window.ipc = {
    postMessage(source) {
      const m = JSON.parse(source);
      if (!LIBRARY.has(m.command)) return inner.postMessage(source);
      state.commands.push(m);
      setTimeout(() => {
        const dialog = (event) => (state.cancel ? reply({ event: 'Idle' }) : reply(event));
        switch (m.command) {
          case 'Library': return reply(listing());
          case 'LibraryOpen': state.current = m.path; state.home = false; return inner.postMessage(JSON.stringify({ command: 'Load' }));
          case 'NewDeck': case 'NewDoc': state.home = false; return inner.postMessage(JSON.stringify({ command: 'Load' }));
          case 'LibraryReveal': return reply({ event: 'Revealed' });
          case 'LibraryRename': { const item = find(m.path); if (!item) return reply({ event: 'Error', message: 'no such deck or doc' }); item.title = m.title.trim(); item.modified = new Date().toISOString(); return reply(listing({ selected: m.path })); }
          case 'LibraryDuplicate': {
            const item = find(m.path); if (!item) return reply({ event: 'Error', message: 'no such deck or doc' });
            const copy = { ...structuredClone(item), relative: m.path.replace(/\.(deck|doc)\.json$/, '-copy.$1.json'), title: `${item.title} copy`, modified: new Date().toISOString(), current: false };
            items.unshift(copy); return reply(listing({ selected: copy.relative, notice: 'Duplicated' }));
          }
          case 'LibraryDelete': { items = items.filter(i => i.relative !== m.path); const was = state.current === m.path; if (was) { state.current = null; state.home = true; } return reply(listing({ notice: 'Moved the file to the trash folder (.trash)' })); }
          case 'LibraryExport': { const item = find(m.path); return dialog({ event: 'Exported', path: `/Users/me/Desktop/${item.title}.${extension[m.format]}`, warnings: [] }); }
          case 'ExportAs': return dialog({ event: 'Exported', path: `/Users/me/Desktop/Export.${extension[m.format]}`, warnings: [] });
          case 'SaveCopy': return dialog({ event: 'Saved', path: '/Users/me/Backups/copy.deck.json', assets: 2 });
          case 'PickTemplate': return dialog({ event: 'TemplateImported', slug: 'brand', name: 'Brand', warnings: [] });
          case 'OpenDialog': return reply({ event: 'Idle' });
          default: return reply({ event: 'Idle' });
        }
      }, 12);
    },
  };
})();
