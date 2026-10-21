import { element, renderSlide } from './render.js';
import { button, icon } from './icons.js';

// Thumbnail-only projections. The Rust backend alone validates and applies edits.
export function project(deck, edit) {
  const next = structuredClone(deck);
  for (const op of edit.ops) {
    const slide = next.slides.find(s => s.id === op.slide);
    if (op.op === 'update_block') Object.assign(slide?.blocks.find(b => b.id === op.id) || {}, op.fields);
    else if (op.op === 'restyle') { for (const s of next.slides) for (const b of s.blocks) if (['deck', s.id, b.id].includes(op.target)) b.style = op.style; }
    else if (op.op === 'delete_block' && slide) slide.blocks = slide.blocks.filter(b => b.id !== op.id);
    else if (op.op === 'insert_block' && slide) slide.blocks.push(op.block);
    else if (op.op === 'set_theme') next.theme = { ...next.theme, ...op.theme, colors:{ ...next.theme.colors, ...op.theme.colors }, fonts:{ ...next.theme.fonts, ...op.theme.fonts } };
    else if (op.op === 'add_slide') { const index = next.slides.findIndex(s => s.id === op.after); next.slides.splice(index < 0 ? next.slides.length : index + 1, 0, op.slide); }
    else if (op.op === 'delete_slide') next.slides = next.slides.filter(s => s.id !== op.id);
  }
  return next;
}
export function editSlide(deck, edit, fallback) {
  for (const op of edit.ops) {
    if (op.op === 'add_slide') return op.slide.id;
    if (op.slide && typeof op.slide === 'string') return op.slide;
    const slide = deck.slides.find(s => s.id === op.target || s.id === op.id || s.blocks.some(b => b.id === op.target || b.id === op.id));
    if (slide) return slide.id;
  }
  return fallback;
}
export function feedbackController(api) {
  const state = api.state, pins = document.querySelector('#pins'), popover = document.querySelector('#thread-popover');
  const chat = document.querySelector('#chat-messages'), comments = document.querySelector('#comments-list');
  let busy = false, filter = 'open';
  const selection = () => ({ slides: state.slideId ? [state.slideId] : [], blocks: [...state.selected].map(id => [state.slideId, id]) });
  const time = created => new Date(created * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  function author(message) {
    const row = element('div', 'message-author'); const avatar = element('span', 'avatar');
    if (message.role === 'model') avatar.append(icon('sparkle')); else avatar.textContent = 'Y';
    const date = element('time', '', time(message.created)); date.dateTime = new Date(message.created * 1000).toISOString();
    row.append(avatar, element('span', '', message.role === 'model' ? 'nexus' : 'You'), date); return row;
  }
  function preview(owner, edit) {
    state.previewTarget = { owner, edit: edit.id, slideId: editSlide(state.deck, edit, state.slideId) };
    api.command({ command: 'PreviewEdit', owner, edit: edit.id });
  }
  function apply(owner, edit, option = false) {
    api.command(option ? { command: 'ChooseOption', message: owner, edit: edit.id } : { command: 'ApplyEdit', owner, edit: edit.id });
  }
  function card(owner, edit, option = false) {
    const card = element('div', `edit-card ${edit.status}`);
    if (option || !state.threads.some(t => t.id === owner)) {
      const thumbnail = element('div', 'slide edit-thumbnail'); const next = project(state.deck, edit);
      renderSlide(thumbnail, next.slides.find(s => s.id === editSlide(state.deck, edit, state.slideId)) || next.slides[0], next.theme, state.images); card.append(thumbnail);
    }
    const info = element('div', 'edit-info'), kicker = element('div', 'edit-kicker'); kicker.append(icon('sparkle'), element('span', '', option ? 'A different direction' : 'Suggested edit'));
    info.append(kicker, element('p', 'edit-summary', edit.why));
    const actions = element('div', 'edit-actions');
    if (edit.status === 'pending') {
      const see = element('button', '', 'Preview'); see.onclick = () => preview(owner, edit);
      const accept = element('button', 'primary', option ? 'Use this option' : 'Apply'); accept.onclick = () => apply(owner, edit, option);
      const dismiss = button('x', 'Dismiss edit', () => api.command({ command: 'DismissEdit', owner, edit: edit.id }), 'dismiss');
      actions.append(see, accept, dismiss);
    } else if (edit.status === 'outdated') {
      actions.append(element('span', 'edit-status', 'Outdated — ask again'));
      const ask = element('button', '', 'Ask nexus'); ask.onclick = () => {
        if (state.threads.some(t => t.id === owner)) api.command({ command: 'CommentReply', thread: owner, text: 'Please update this suggested edit for the current slide.', send: true });
        else api.command({ command: 'Chat', text: `Please revisit this edit for the current deck: ${edit.why}`, selection: selection() });
      }; actions.append(ask);
    } else { const label = element('span', 'edit-status', edit.status === 'applied' ? 'Applied · in your deck' : 'Dismissed'); actions.append(label); }
    info.append(actions); card.append(info); return card;
  }
  function renderChat() {
    const scrollBottom = chat.scrollHeight - chat.scrollTop - chat.clientHeight < 80;
    chat.replaceChildren();
    if (!state.chat.length) {
      const intro = element('div', 'chat-intro'), mark = element('div', 'intro-mark'); mark.append(icon('sparkle'));
      intro.append(mark, element('h1', '', 'Good ideas deserve\na little room.'), element('p', '', 'A space to shape your story, explore directions, and make every slide feel intentional.')); chat.append(intro);
    }
    document.querySelector('#chat-input').placeholder = state.chat.length ? 'Ask for changes…' : 'Think big in chat. Get specific with comments.';
    for (const message of state.chat) {
      const row = element('article', `chat-message ${message.role}`); row.append(author(message), element('p', 'message-text', message.text));
      const options = element('div', message.edits.length > 1 ? 'options-grid' : ''); message.edits.forEach(edit => options.append(card(message.id, edit, message.edits.length > 1))); row.append(options); chat.append(row);
    }
    if (busy) chat.append(element('div', 'waiting', 'nexus is thinking…'));
    if (scrollBottom) chat.scrollTop = chat.scrollHeight;
    document.querySelector('#chat-scope').textContent = state.selected.size ? `${state.selected.size} selected layer${state.selected.size > 1 ? 's' : ''}` : state.slideId ? 'Current slide · deck context' : 'Whole deck';
    document.querySelector('#chat-send').disabled = busy;
  }
  function openThread(thread) {
    clearPreview(); state.openThread = thread.id; state.draftAnchor = null; state.slideId = thread.anchor.slide; state.selected.clear(); api.render();
  }
  function renderPins() {
    pins.replaceChildren();
    let number = 0;
    for (const thread of state.threads) {
      number++;
      if (thread.anchor.slide !== state.slideId || (thread.resolved && !state.showResolved)) continue;
      const block = api.slide()?.blocks.find(b => b.id === thread.anchor.block);
      const pin = element('button', `pin ${thread.severity}${state.openThread === thread.id ? ' active' : ''}`, String(number));
      pin.title = thread.messages[0]?.text || 'Comment thread'; pin.setAttribute('aria-label', `Comment ${number}: ${pin.title}`);
      const x = block ? Math.max(block.x, Math.min(block.x + block.w, thread.anchor.x)) : thread.anchor.x;
      const y = block ? Math.max(block.y, Math.min(block.y + block.h, thread.anchor.y)) : thread.anchor.y;
      pin.style.left = `${x / 32 * 100}%`; pin.style.top = `${y / 18 * 100}%`;
      pin.onclick = () => openThread(thread); pins.append(pin);
    }
    if (state.draftAnchor) {
      const pin = element('span', 'pin active', '+'); pin.style.left = `${state.draftAnchor.x / 32 * 100}%`; pin.style.top = `${state.draftAnchor.y / 18 * 100}%`; pins.append(pin);
    }
    requestAnimationFrame(() => {
      const active = pins.querySelector('.pin.active');
      if (!active || popover.hidden) return;
      const space = document.querySelector('#canvas-space').getBoundingClientRect();
      const center = active.getBoundingClientRect().top - space.top;
      popover.style.top = `${Math.max(110, Math.min(space.height - popover.offsetHeight - 24, center - popover.offsetHeight / 2))}px`;
    });
  }
  function closeThread() { state.openThread = null; state.draftAnchor = null; api.render(); }
  function renderThread() {
    const thread = state.threads.find(t => t.id === state.openThread);
    popover.hidden = !thread && !state.draftAnchor;
    document.querySelector('#workspace').classList.toggle('thread-open', !popover.hidden);
    popover.replaceChildren(); if (popover.hidden) return;
    const header = element('div', 'thread-heading');
    header.append(icon('comment'), element('strong', '', thread ? `Comment ${state.threads.indexOf(thread) + 1}` : 'Leave a comment'));
    if (thread) header.append(element('span', `severity ${thread.severity}`, thread.severity));
    header.append(button('x', 'Close thread', closeThread)); popover.append(header);
    if (thread) for (const message of thread.messages) {
      const row = element('article', 'thread-message'); row.append(author(message), element('p', 'message-text', message.text));
      message.edits.forEach(edit => row.append(card(thread.id, edit))); popover.append(row);
    }
    if (thread?.resolved) popover.append(element('p', 'hint', 'Resolved. This conversation is kept with your deck.'));
    const reply = element('textarea'); reply.maxLength = 2000; reply.placeholder = thread ? 'Reply to this thread…' : 'What could be better here?'; reply.setAttribute('aria-label', thread ? 'Reply to thread' : 'Comment text');
    // Keep an unsent draft when an unrelated event re-renders the view.
    const draftKey = thread?.id || JSON.stringify(state.draftAnchor); reply.value = state.drafts[draftKey] || ''; reply.oninput = () => state.drafts[draftKey] = reply.value;
    popover.append(reply); const actions = element('div', 'thread-actions');
    function send(toModel) {
      if (!reply.value.trim()) { reply.focus(); return; }
      const text = reply.value; delete state.drafts[draftKey];
      api.command(thread ? { command: 'CommentReply', thread: thread.id, text, send: toModel } : { command: 'CommentCreate', anchor: state.draftAnchor, text, send: toModel }, event => {
        if (event.event === 'Error') { state.drafts[draftKey] = text; return; }
        if (!thread) state.openNewest = true;
        state.draftAnchor = null;
        if (event.event === 'Feedback') { if (!thread) state.openThread = state.threads.at(-1)?.id; api.render(); }
      });
    }
    const note = element('button', '', 'Comment'); note.onclick = () => send(false);
    const ask = element('button', 'primary', 'Ask nexus'); ask.prepend(icon('send')); ask.disabled = busy; ask.onclick = () => send(true);
    actions.append(note, ask); popover.append(actions);
    if (thread) {
      const resolve = element('button', '', thread.resolved ? 'Reopen thread' : 'Resolve thread'); resolve.prepend(icon('check')); resolve.style.marginTop = '16px'; resolve.style.fontSize = '11px';
      resolve.onclick = () => api.command({ command: thread.resolved ? 'CommentReopen' : 'CommentResolve', thread: thread.id }); popover.append(resolve);
    }
  }
  function renderComments() {
    const open = state.threads.filter(t => !t.resolved).length;
    document.querySelector('#comment-count').textContent = open; document.querySelector('#review-count').textContent = open;
    comments.replaceChildren();
    for (const [i, slide] of state.deck.slides.entries()) {
      const threads = state.threads.filter(t => t.anchor.slide === slide.id && Boolean(t.resolved) === (filter === 'resolved'));
      if (!threads.length) continue;
      comments.append(element('div', 'comments-slide', `Slide ${String(i + 1).padStart(2, '0')}`));
      for (const thread of threads) {
        const row = element('button', 'comment-row'); row.append(element('span', `severity ${thread.severity}`, thread.severity), element('p', '', thread.messages[0]?.text || ''), element('small', '', `${thread.messages.length} message${thread.messages.length > 1 ? 's' : ''} · ${thread.messages[0]?.role === 'model' ? 'nexus' : 'You'}`)); row.onclick = () => openThread(thread); comments.append(row);
      }
    }
    if (!comments.childNodes.length) comments.append(element('p', 'hint', filter === 'open' ? 'Nothing to resolve. Add a comment on the canvas, or ask nexus for a review.' : 'Resolved conversations will appear here.'));
    document.querySelector('#review').disabled = busy; document.querySelector('#review-panel').disabled = busy;
  }
  function render() { renderChat(); renderThread(); renderPins(); renderComments(); }
  function clearPreview() {
    state.preview = null; state.previewTarget = null; document.querySelector('#preview-bar').hidden = true;
  }
  function event(event) {
    if (event.event === 'HostStatus') {
      const host = event.status, node = document.querySelector('#host-status'); node.className = `host-pill ${host.state}`; node.textContent = host.state === 'connected' ? (host.model?.split('/').at(-1) || 'Host connected') : host.message;
      node.title = host.message; document.querySelector('#start-host').hidden = !host.can_start;
    } else if (event.event === 'Suggesting') { busy = true; api.command({ command: 'Load' }); renderChat(); }
    else if (event.event === 'Feedback') {
      busy = false; state.threads = event.threads; state.chat = event.chat;
      if (state.openNewest) { state.openThread = state.threads.at(-1)?.id; state.openNewest = false; }
      render(); if (event.notes.length) api.toast(event.notes.join('; '));
    } else if (event.event === 'Error') { busy = false; renderChat(); }
    else if (event.event === 'Preview' && state.previewTarget) {
      const target = state.previewTarget; state.preview = { deck: event.deck, slideId: target.slideId, before: false }; state.slideId = target.slideId;
      document.querySelector('#preview-bar').hidden = false; document.querySelector('#preview-apply').onclick = () => api.command({ command: 'ApplyEdit', owner: target.owner, edit: target.edit });
      api.render();
    }
  }
  document.querySelector('#host-status').onclick = () => api.command({ command: 'HostStatus' });
  document.querySelector('#start-host').onclick = () => api.command({ command: 'StartHost' });
  for (const id of ['review', 'review-panel']) document.querySelector(`#${id}`).onclick = () => api.command({ command: 'Review', selection: selection() });
  document.querySelector('#ask-options').onclick = () => { document.querySelector('#chat-input').value = 'Give me 2–3 options for this slide, keeping my facts and voice.'; document.querySelector('#chat-input').focus(); };
  document.querySelector('#chat-form').onsubmit = e => { e.preventDefault(); const input = document.querySelector('#chat-input'); if (!input.value.trim() || busy) return; api.command({ command: 'Chat', text: input.value, selection: selection() }); input.value = ''; };
  document.querySelector('#chat-input').onkeydown = e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); document.querySelector('#chat-form').requestSubmit(); } };
  function togglePreview() { if (state.preview) { state.preview.before = !state.preview.before; document.querySelector('#preview-toggle').textContent = state.preview.before ? 'Before · B for after' : 'After · B for before'; api.renderCanvas(); } }
  document.querySelector('#preview-toggle').onclick = togglePreview;
  document.querySelector('#close-preview').onclick = () => { clearPreview(); api.render(); };
  for (const name of ['open','resolved']) document.querySelector(`#filter-${name}`).onclick = () => { filter = name; for (const f of ['open','resolved']) document.querySelector(`#filter-${f}`).setAttribute('aria-pressed', String(f === filter)); renderComments(); };
  document.querySelector('#show-resolved').onchange = e => { state.showResolved = e.target.checked; renderPins(); };
  return { render, event, clearPreview, togglePreview, closeThread, renderThread, renderPins };
}
