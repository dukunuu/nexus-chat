// Comments in the margin, the Claude Docs way: each thread is a card beside the
// text it is about, aligned with its anchor; the text carries a soft highlight;
// a suggestion made of replace_text operations is drawn in the text as a tracked
// change with Accept and Reject on its card.
import { inlineTokens, markdownTokens } from '../helpers.js';
import * as M from './model.js';
import { parseInline, plainText, toCodePoints, fromCodePoints } from './inline.js';
import { glyph } from './glyphs.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const KNOWN = new Set(['queued', 'working', 'progress', 'done', 'failed', 'stopped']);
const busy = activity => ['queued', 'working', 'progress'].includes(activity?.state);
const SEVERITY = { must: 'Must fix', should: 'Should fix', consider: 'Consider' };

/** Inline Markdown as DOM nodes. Links stay text; nothing is interpreted as HTML. */
function rich(text) {
  const fragment = document.createDocumentFragment();
  for (const token of inlineTokens(text)) fragment.append(el(['strong', 'em', 'code'].includes(token.kind) ? token.kind : 'span', '', token.text));
  return fragment;
}
function paragraphs(text) {
  const out = el('div', 'rich');
  for (const line of markdownTokens(text)) {
    const p = el(['h1', 'h2', 'h3'].includes(line.kind) ? 'strong' : 'p');
    p.append(rich(line.text));
    out.append(p);
  }
  return out;
}

export function commentsController(api) {
  const { state, surface } = api;
  const root = api.margin;
  const progress = {}, doneUntil = {}, asked = new Set();
  let layoutFrame = 0, showResolved = false;
  state.activeThread = null;
  state.draft = null;
  state.previews = {};

  const threadOf = id => state.threads.find(t => t.id === id);
  const isSuggestion = edit => edit.ops.length > 0 && edit.ops.every(op => op.op === 'replace_text');

  // -- highlights and tracked changes ------------------------------------------------

  /** Decorations by block and part: comment ranges and the tracked changes of pending suggestions. */
  function computeDecos() {
    const decos = {};
    const at = (block, part) => ((decos[block] ||= {})[part] ||= { marks: [], del: [], ins: [] });
    const addMark = (anchor, id, extra) => {
      const b = anchor.block && M.block(state.doc, anchor.block);
      if (!b || !anchor.range) return;
      const text = M.blockPlainText(b);
      const start = fromCodePoints(text, anchor.range.start), end = fromCodePoints(text, anchor.range.end);
      for (const piece of M.rangeParts(b, start, end)) at(b.id, piece.path).marks.push({ start: piece.start, end: piece.end, id, ...extra });
    };
    for (const thread of state.threads) {
      if (thread.resolved) continue;
      addMark(thread.anchor, thread.id, { hot: state.activeThread === thread.id });
      for (const message of thread.messages) for (const edit of message.edits) {
        if (edit.status !== 'pending' || !isSuggestion(edit)) continue;
        for (const op of edit.ops) trackChange(at, op, edit.id);
      }
    }
    if (state.draft?.anchor.range) addMark(state.draft.anchor, 'draft', { hot: true });
    state.decos = decos;
  }

  /** Strike the text a replace_text would remove and show what would go in. */
  function trackChange(at, op, id) {
    const b = M.block(state.doc, op.id);
    if (!b) return;
    const path = op.item ? `item:${op.item}` : 'text';
    if (!M.partsOf(b).some(p => p.path === path)) return;
    const source = M.getPart(b, path), plain = M.partPlain(b, path);
    const findPlain = plainText(parseInline(op.find)), inserted = plainText(parseInline(op.replace));
    if (!findPlain) return;
    const sources = [];
    for (let i = source.indexOf(op.find); i >= 0; i = source.indexOf(op.find, i + op.find.length)) sources.push(i);
    const plains = [];
    for (let i = plain.indexOf(findPlain); i >= 0; i = plain.indexOf(findPlain, i + findPlain.length)) plains.push(i);
    const chosen = op.occurrence ? [op.occurrence - 1] : sources.map((_, i) => i);
    for (const k of chosen) {
      if (plains[k] === undefined || sources[k] === undefined) continue;
      const deco = at(op.id, path);
      deco.del.push({ start: plains[k], end: plains[k] + findPlain.length, id });
      if (inserted) deco.ins.push({ at: plains[k] + findPlain.length, text: inserted, id });
    }
  }

  // -- starting a thread ---------------------------------------------------------------

  /** The anchor for the selection: a range in one block, or the block itself. */
  function anchorFromSelection() {
    const sel = api.editor.selection();
    if (!sel) return null;
    const b = M.block(state.doc, sel.from.block);
    if (!b) return null;
    const text = M.blockPlainText(b), layout = M.plainLayout(b);
    if (sel.collapsed || !sel.from.part) return { block: b.id, quote: '' };
    let start = M.blockOffset(b, sel.from.part, sel.from.offset), end;
    if (sel.to.block === b.id && sel.to.part) end = M.blockOffset(b, sel.to.part, sel.to.offset);
    else end = layout.at(-1)?.end ?? text.length;
    if (end <= start) return { block: b.id, quote: '' };
    const quote = text.slice(start, end);
    return { block: b.id, range: { start: toCodePoints(text, start), end: toCodePoints(text, end) }, quote };
  }
  function begin() {
    const anchor = anchorFromSelection();
    if (!anchor) { api.toast('Select some text, then comment on it.'); return; }
    state.draft = { anchor, text: state.draft?.text || '' };
    state.activeThread = null;
    api.toolbar.hide();
    refresh();
    root.querySelector('.card.draft textarea')?.focus();
  }

  // -- activity (nexus working on a comment) -------------------------------------------------

  function updateActivities(next) {
    if (!next) return;
    const before = state.activities;
    state.activities = Object.fromEntries(Object.entries(next).flatMap(([id, a]) => KNOWN.has(a.state) ? [[id, a]] : before[id] ? [[id, before[id]]] : []));
    for (const [id, activity] of Object.entries(state.activities)) {
      const prior = before[id];
      if (['queued', 'working'].includes(activity.state) && prior?.state !== activity.state) { progress[id] = []; delete doneUntil[id]; }
      if (activity.state === 'progress' && activity.detail && !progress[id]?.includes(activity.detail)) (progress[id] ||= []).push(activity.detail);
      if (['done', 'failed', 'stopped'].includes(activity.state) && prior?.state !== activity.state) {
        if (activity.state === 'done') { doneUntil[id] = Date.now() + 4000; setTimeout(refresh, 4000); }
        const thread = threadOf(id);
        if (thread && state.activeThread !== id) api.toast(`nexus ${activity.detail || (activity.state === 'done' ? 'finished' : activity.state)}`, [['View', () => open(id)]]);
      }
    }
    api.renderLink();
  }
  function activityRow(thread) {
    const activity = state.activities[thread.id];
    if (!activity) return null;
    const row = el('div', `activity ${activity.state}`);
    row.setAttribute('role', 'status');
    row.setAttribute('aria-live', 'polite');
    const label = el('div', 'd-activity-label');
    if (busy(activity)) label.append(el('span', 'd-activity-dot'));
    if (activity.state === 'done') label.append(glyph('check'));
    const waiting = { draft: 'Waiting — you have a draft in nexus', streaming: 'Waiting — nexus is answering something else', no_model: 'Waiting — no model selected' };
    label.append(el('span', '', activity.state === 'queued' ? waiting[activity.reason] || 'Waiting for nexus' : ['working', 'progress'].includes(activity.state) ? 'nexus is working…' : activity.state === 'done' ? 'Done' : activity.state === 'stopped' ? 'Stopped' : 'Failed'));
    row.append(label);
    if (['working', 'progress'].includes(activity.state)) for (const line of progress[thread.id] || []) row.append(el('div', 'activity-line', line));
    if (!busy(activity) && activity.detail) row.append(paragraphs(activity.detail));
    if (activity.state === 'done' && /^edited (blocks?|slides?) /.test(activity.detail || '')) {
      const undo = el('button', 'quiet', 'Undo');
      undo.type = 'button';
      undo.addEventListener('click', () => api.undo());
      row.append(undo);
    }
    if (['failed', 'stopped'].includes(activity.state)) {
      const retry = el('button', 'quiet', 'Send again');
      retry.type = 'button';
      retry.disabled = !state.linked;
      retry.addEventListener('click', () => {
        const message = thread.messages.findLast(m => m.role === 'you');
        if (message) api.command({ command: 'CommentReply', thread: thread.id, text: message.text, send: true });
      });
      row.append(retry);
    }
    return row;
  }

  // -- cards ----------------------------------------------------------------------------------

  function author(message) {
    const row = el('div', `author ${message.role}`);
    const name = el('span', 'name', message.role === 'model' ? 'nexus' : 'You');
    name.title = new Date(message.created * 1000).toLocaleString();
    row.append(el('span', 'd-avatar', message.role === 'model' ? '✦' : '❯'), name);
    return row;
  }

  function editCard(thread, edit) {
    const card = el('div', `edit ${edit.status}`);
    if (edit.status !== 'pending') {
      card.append(el('span', 'edit-state', edit.status === 'applied' ? 'Accepted · undo with ⌘Z' : edit.status === 'outdated' ? 'Outdated — the text changed since' : 'Rejected'));
      return card;
    }
    if (isSuggestion(edit)) {
      const lines = edit.ops.map(op => el('div', 'change'));
      edit.ops.forEach((op, i) => {
        lines[i].append(el('del', '', op.find), el('ins', '', op.replace));
      });
      card.append(...lines);
    } else {
      card.append(diffPreview(thread, edit));
    }
    if (edit.why && !/^suggested edit\.?$/i.test(edit.why.trim())) { const why = el('p', 'edit-why'); why.append(rich(edit.why)); card.append(why); }
    const actions = el('div', 'd-edit-actions');
    const accept = el('button', 'primary', 'Accept'), reject = el('button', '', 'Reject');
    accept.type = reject.type = 'button';
    accept.addEventListener('click', () => api.command({ command: 'ApplyEdit', owner: thread.id, edit: edit.id }));
    reject.addEventListener('click', () => api.command({ command: 'DismissEdit', owner: thread.id, edit: edit.id }));
    actions.append(accept, reject);
    card.append(actions);
    return card;
  }

  /** Before and after for operations other than replace_text, from the backend's preview. */
  function diffPreview(thread, edit) {
    const box = el('div', 'diff');
    const preview = state.previews[edit.id];
    if (!preview) {
      box.append(el('div', 'diff-line muted', summarize(edit)));
      if (!asked.has(edit.id)) { asked.add(edit.id); api.command({ command: 'PreviewEdit', owner: thread.id, edit: edit.id }); }
      return box;
    }
    for (const id of preview.removed) {
      const old = M.block(state.doc, id);
      box.append(line('Removed', old ? snippet(old) : id, 'del'));
    }
    for (const next of preview.changed) {
      const old = M.block(state.doc, next.id);
      if (old) box.append(line('Before', snippet(old), 'del'));
      box.append(line(old ? 'After' : 'New', snippet(next), 'ins'));
    }
    if (preview.title != null) box.append(line('Title', preview.title, 'ins'));
    if (!box.childNodes.length) box.append(el('div', 'diff-line muted', summarize(edit)));
    return box;
  }
  const line = (label, text, kind) => {
    const row = el('div', `diff-line ${kind}`);
    row.append(el('span', 'diff-label', label), el(kind === 'del' ? 'del' : 'ins', '', text));
    return row;
  };
  const snippet = b => {
    const text = M.blockPlainText(b).replace(/\n/g, ' ');
    return text ? (text.length > 140 ? `${text.slice(0, 139)}…` : text) : M.blockLabel(b);
  };
  const summarize = edit => `${edit.ops.length} change${edit.ops.length === 1 ? '' : 's'}: ${[...new Set(edit.ops.map(op => op.op.replaceAll('_', ' ')))].join(', ')}`;

  function replyBox(thread) {
    const wrap = el('div', 'reply');
    const area = el('textarea');
    area.rows = 1; area.maxLength = 2000; area.placeholder = 'Reply…';
    area.setAttribute('aria-label', 'Reply to thread');
    area.value = state.drafts[thread.id] || '';
    area.addEventListener('input', () => { state.drafts[thread.id] = area.value; area.style.height = 'auto'; area.style.height = `${area.scrollHeight}px`; scheduleLayout(); });
    const send = (toNexus) => {
      if (!area.value.trim()) { area.focus(); return; }
      const text = area.value;
      delete state.drafts[thread.id];
      api.command({ command: 'CommentReply', thread: thread.id, text, send: toNexus }, e => { if (e.event === 'Error') state.drafts[thread.id] = text; });
    };
    area.addEventListener('keydown', e => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); send(state.linked); } });
    const note = el('button', '', 'Reply'), ask = el('button', 'primary', 'Send to nexus');
    note.type = ask.type = 'button';
    ask.prepend(glyph('send'));
    ask.disabled = !state.linked;
    ask.title = state.linked ? 'Send this reply to the conversation' : 'Not linked to a conversation — open it from nexus with /studio';
    note.addEventListener('click', () => send(false));
    ask.addEventListener('click', () => send(true));
    const actions = el('div', 'reply-actions');
    actions.append(note, ask);
    wrap.append(area, actions);
    return wrap;
  }

  function threadCard(thread) {
    const active = state.activeThread === thread.id;
    const card = el('article', `card${active ? ' active' : ''}${thread.resolved ? ' resolved' : ''}${thread.messages[0]?.role === 'model' ? ' model' : ''}`);
    card.dataset.thread = thread.id;
    card.tabIndex = 0;
    card.setAttribute('aria-label', `Comment thread: ${thread.messages[0]?.text.slice(0, 60) ?? ''}`);
    const activity = state.activities[thread.id];
    if (busy(activity)) card.classList.add('busy');
    if (doneUntil[thread.id] > Date.now()) card.classList.add('finished');
    const model = thread.messages.find(m => m.role === 'model');
    const head = el('div', 'card-head');
    head.append(author(thread.messages[0]));
    if (model && !thread.resolved) head.append(el('span', `d-severity ${thread.severity}`, SEVERITY[thread.severity] || thread.severity));
    const resolve = el('button', 'icon-button quiet resolve');
    resolve.type = 'button';
    resolve.append(glyph(thread.resolved ? 'redo' : 'check'));
    resolve.title = thread.resolved ? 'Reopen thread' : 'Resolve thread';
    resolve.setAttribute('aria-label', resolve.title);
    resolve.addEventListener('click', e => { e.stopPropagation(); api.command({ command: thread.resolved ? 'CommentReopen' : 'CommentResolve', thread: thread.id }); });
    head.append(resolve);
    card.append(head);
    if (thread.anchor.quote) card.append(el('blockquote', 'quote', thread.anchor.quote.length > 90 ? `${thread.anchor.quote.slice(0, 89)}…` : thread.anchor.quote));
    else if (thread.anchor.block == null) card.append(el('div', 'quote muted', 'The commented text was removed'));
    const shown = active ? thread.messages : thread.messages.slice(0, 1);
    shown.forEach((message, i) => {
      const body = el('div', 'message');
      if (i > 0) body.append(author(message));
      body.append(paragraphs(message.text));
      for (const edit of message.edits) body.append(editCard(thread, edit));
      card.append(body);
    });
    if (!active && thread.messages.length > 1) card.append(el('div', 'more', `${thread.messages.length - 1} ${thread.messages.length === 2 ? 'reply' : 'replies'}`));
    if (!active) {
      const pending = thread.messages.flatMap(m => m.edits).filter(e => e.status === 'pending');
      if (pending.length && shown[0].edits.length === 0) card.append(el('div', 'more', `${pending.length} suggestion${pending.length === 1 ? '' : 's'}`));
    }
    const status = activityRow(thread);
    if (status) card.append(status);
    if (active && !thread.resolved) card.append(replyBox(thread));
    card.addEventListener('mouseenter', () => hot(thread.id, true));
    card.addEventListener('mouseleave', () => hot(thread.id, false));
    card.addEventListener('click', e => { if (!e.target.closest('button, textarea')) open(thread.id); });
    return card;
  }

  function draftCard() {
    const card = el('article', 'card draft active');
    card.append(el('div', 'card-head', ''));
    card.querySelector('.card-head').append(Object.assign(el('div', 'author you'), {}));
    const author = card.querySelector('.author');
    author.append(el('span', 'd-avatar', '❯'), el('span', 'name', 'You'));
    if (state.draft.anchor.quote) card.append(el('blockquote', 'quote', state.draft.anchor.quote.length > 90 ? `${state.draft.anchor.quote.slice(0, 89)}…` : state.draft.anchor.quote));
    const area = el('textarea');
    area.rows = 2; area.maxLength = 2000; area.placeholder = 'Add a comment or request a change…';
    area.setAttribute('aria-label', 'Comment');
    area.value = state.draft.text;
    area.addEventListener('input', () => { state.draft.text = area.value; scheduleLayout(); });
    const submit = toNexus => {
      if (!area.value.trim()) { area.focus(); return; }
      const { anchor } = state.draft, text = area.value;
      api.command({ command: 'DocCommentCreate', anchor, text, send: toNexus }, e => {
        if (e.event === 'Error') return;
        state.draft = null;
        const created = state.threads.at(-1);
        state.activeThread = created?.id ?? null;
        refresh();
      });
    };
    area.addEventListener('keydown', e => {
      if (e.key === 'Escape') { e.preventDefault(); cancelDraft(); }
      else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(state.linked); }
    });
    const cancel = el('button', 'icon-button quiet resolve', ''), note = el('button', '', 'Comment'), ask = el('button', 'primary', 'Send to nexus');
    cancel.append(glyph('x'));
    cancel.title = 'Cancel (Esc)';
    cancel.setAttribute('aria-label', 'Cancel comment');
    cancel.type = note.type = ask.type = 'button';
    ask.prepend(glyph('send'));
    ask.disabled = !state.linked;
    ask.title = state.linked ? 'Send this comment to the conversation' : 'Not linked to a conversation — open it from nexus with /studio';
    cancel.addEventListener('click', cancelDraft);
    note.addEventListener('click', () => submit(false));
    ask.addEventListener('click', () => submit(true));
    const actions = el('div', 'reply-actions');
    actions.append(note, ask);
    card.querySelector('.card-head').append(cancel);
    card.append(area, actions);
    return card;
  }
  function cancelDraft() { state.draft = null; refresh(); surface.focus({ preventScroll: true }); }

  // -- rendering the margin ----------------------------------------------------------------------

  function hot(id, on) {
    for (const node of surface.querySelectorAll(`mark.cmt[data-thread="${CSS.escape(id)}"]`)) node.classList.toggle('hot', on || state.activeThread === id);
  }
  function open(id) {
    if (state.activeThread === id) return;
    state.activeThread = id; state.draft = null;
    refresh();
    scrollToAnchor(id);
  }
  function close() { if (state.activeThread || state.draft) { state.activeThread = null; refresh(); } }
  function scrollToAnchor(id) {
    const node = surface.querySelector(`mark.cmt[data-thread="${CSS.escape(id)}"]`) || surface.querySelector(`[data-block="${CSS.escape(threadOf(id)?.anchor.block ?? '')}"]`);
    if (node) api.scrollTo(node);
  }

  function pendingEdits() { return state.threads.filter(t => !t.resolved).flatMap(t => t.messages.flatMap(m => m.edits)).filter(e => e.status === 'pending'); }

  function render() {
    root.replaceChildren();
    const open = state.threads.filter(t => !t.resolved), done = state.threads.filter(t => t.resolved);
    const pending = pendingEdits();
    api.setCommentCount(open.length);
    const header = el('div', 'margin-head');
    if (pending.length) {
      const label = el('span', 'margin-title', `${pending.length} suggestion${pending.length === 1 ? '' : 's'}`);
      const accept = el('button', 'primary small', 'Accept all'), reject = el('button', 'small', 'Reject all');
      accept.type = reject.type = 'button';
      accept.addEventListener('click', () => api.command({ command: 'DocResolveEdits', accept: true }));
      reject.addEventListener('click', () => api.command({ command: 'DocResolveEdits', accept: false }));
      header.append(label, reject, accept);
      root.append(header);
    }
    const track = el('div', 'margin-track');
    root.append(track);
    for (const thread of open) track.append(threadCard(thread));
    if (state.draft) track.append(draftCard());
    if (done.length) {
      const toggle = el('button', 'resolved-fold quiet');
      toggle.type = 'button';
      toggle.setAttribute('aria-expanded', String(showResolved));
      toggle.append(glyph('down'), document.createTextNode(`Resolved (${done.length})`));
      toggle.addEventListener('click', () => { showResolved = !showResolved; render(); });
      const list = el('div', 'resolved-list');
      list.hidden = !showResolved;
      if (showResolved) for (const thread of done) list.append(threadCard(thread));
      track.append(toggle, list);
      track.dataset.hasResolved = '1';
    }
    layout();
  }

  /** Put each card beside its anchor; stack collisions; slide the active one to its anchor. */
  function layout() {
    layoutFrame = 0;
    const track = root.querySelector('.margin-track');
    if (!track) return;
    const base = track.getBoundingClientRect().top;
    const gap = 10;
    const cards = [...track.querySelectorAll(':scope > .card')].map(card => {
      const id = card.dataset.thread;
      const anchor = id ? (surface.querySelector(`mark.cmt[data-thread="${CSS.escape(id)}"]`) || surface.querySelector(`[data-block="${CSS.escape(threadOf(id)?.anchor.block ?? '')}"]`)) : (surface.querySelector('mark.cmt[data-thread="draft"]') || surface.querySelector(`[data-block="${CSS.escape(state.draft?.anchor.block ?? '')}"]`));
      const wanted = anchor ? anchor.getBoundingClientRect().top - base : 0;
      return { card, wanted, height: card.offsetHeight, y: wanted };
    }).sort((a, b) => a.wanted - b.wanted);
    const activeAt = cards.findIndex(c => c.card.classList.contains('active'));
    let bottom = -gap;
    cards.forEach((entry, i) => { if (i >= Math.max(activeAt, 0) || activeAt < 0) { entry.y = Math.max(entry.wanted, bottom + gap); bottom = entry.y + entry.height; } else bottom = Infinity; });
    if (activeAt >= 0) {
      cards[activeAt].y = cards[activeAt].wanted;
      let ceiling = cards[activeAt].y - gap;
      for (let i = activeAt - 1; i >= 0; i--) { cards[i].y = Math.min(cards[i].wanted, ceiling - cards[i].height); ceiling = cards[i].y - gap; }
      let floor = cards[activeAt].y + cards[activeAt].height + gap;
      for (let i = activeAt + 1; i < cards.length; i++) { cards[i].y = Math.max(cards[i].wanted, floor); floor = cards[i].y + cards[i].height + gap; }
    }
    // Cards pushed above the track's top by the active card are clamped, so
    // settle top-down once more: never above 0, never overlapping the card above.
    let floor = 0;
    for (const entry of cards) { entry.y = Math.max(entry.y, floor); floor = entry.y + entry.height + gap; }
    let end = 0;
    for (const { card, y, height } of cards) { card.style.top = `${Math.round(y)}px`; end = Math.max(end, y + height); }
    const toggle = track.querySelector('.resolved-fold'), list = track.querySelector('.resolved-list');
    if (toggle) { toggle.style.top = `${Math.round(end + 16)}px`; if (list) list.style.top = `${Math.round(end + 16 + toggle.offsetHeight + 6)}px`; end += 16 + toggle.offsetHeight + (list?.hidden ? 0 : list.offsetHeight + 6); }
    track.style.height = `${Math.round(end)}px`;
  }
  function scheduleLayout() { if (!layoutFrame) layoutFrame = requestAnimationFrame(layout); }

  /** Recompute highlights and cards after the threads, the doc or the selection of a thread changed. */
  function refresh() {
    computeDecos();
    api.editor.reconcile();
    render();
  }
  function onPreview(event) { state.previews[event.edit] = event; render(); }

  // The text and the cards point at each other.
  surface.addEventListener('mouseover', e => {
    const mark = e.target.closest?.('mark.cmt');
    if (mark) root.querySelector(`.card[data-thread="${CSS.escape(mark.dataset.thread)}"]`)?.classList.add('hot');
  });
  surface.addEventListener('mouseout', e => {
    const mark = e.target.closest?.('mark.cmt');
    if (mark) root.querySelector(`.card[data-thread="${CSS.escape(mark.dataset.thread)}"]`)?.classList.remove('hot');
  });
  surface.addEventListener('click', e => {
    const mark = e.target.closest?.('mark.cmt');
    if (mark && mark.dataset.thread !== 'draft') open(mark.dataset.thread);
    else if (!mark && state.activeThread && !getSelection().toString()) close();
  });
  new ResizeObserver(scheduleLayout).observe(surface);

  return { begin, refresh, render, layout: scheduleLayout, computeDecos, updateActivities, open, close, onPreview, anchorFromSelection, pendingEdits };
}
