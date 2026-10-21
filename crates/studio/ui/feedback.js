import { element } from './render.js';
import { button, icon } from './icons.js';

export function editSlide(deck, edit, fallback) {
  for (const op of edit.ops) {
    if (op.op === 'add_slide') return op.slide.id;
    if (typeof op.slide === 'string') return op.slide;
    const slide = deck.slides.find(s => s.id === op.target || s.id === op.id || s.blocks.some(b => b.id === op.target || b.id === op.id));
    if (slide) return slide.id;
  }
  return fallback;
}
export function feedbackController(api) {
  const state = api.state, pins = document.querySelector('#pins'), popover = document.querySelector('#thread-popover'), comments = document.querySelector('#comments-list');
  let filter = 'open';
  function author(message) {
    const row = element('div', 'message-author'), avatar = element('span', 'avatar');
    if (message.role === 'model') avatar.append(icon('sparkle')); else avatar.textContent = 'Y';
    row.append(avatar, element('span', '', message.role === 'model' ? 'nexus' : 'You'), element('time', '', new Date(message.created * 1000).toLocaleTimeString([], { hour:'2-digit', minute:'2-digit' }))); return row;
  }
  function card(owner, edit) {
    const node = element('div', `edit-card ${edit.status}`), info = element('div', 'edit-info'), kicker = element('div', 'edit-kicker');
    kicker.append(icon('sparkle'), element('span', '', 'Suggested edit')); info.append(kicker, element('p', 'edit-summary', edit.why));
    const actions = element('div', 'edit-actions');
    if (edit.status === 'pending') {
      const preview = element('button', '', 'Preview'); preview.onclick = () => { state.previewTarget = { owner, edit:edit.id, slideId:editSlide(state.deck, edit, state.slideId) }; api.command({ command:'PreviewEdit', owner, edit:edit.id }); };
      const apply = element('button', 'primary', 'Apply'); apply.onclick = () => api.command({ command:'ApplyEdit', owner, edit:edit.id });
      actions.append(preview, apply, button('x', 'Dismiss edit', () => api.command({ command:'DismissEdit', owner, edit:edit.id }), 'dismiss'));
    } else actions.append(element('span', 'edit-status', edit.status === 'outdated' ? 'Outdated — ask again in nexus' : edit.status === 'applied' ? 'Applied · in your deck' : 'Dismissed'));
    info.append(actions); node.append(info); return node;
  }
  function openThread(thread) { clearPreview(); state.openThread = thread.id; state.draftAnchor = null; state.slideId = thread.anchor.slide; state.selected.clear(); api.render(); }
  function renderPins() {
    pins.replaceChildren();
    state.threads.forEach((thread, i) => {
      if (thread.anchor.slide !== state.slideId || (thread.resolved && !state.showResolved)) return;
      const block = api.slide()?.blocks.find(b => b.id === thread.anchor.block);
      const pin = element('button', `pin ${thread.severity}${state.openThread === thread.id ? ' active' : ''}`, String(i + 1));
      pin.title = thread.messages[0]?.text || 'Comment thread'; pin.setAttribute('aria-label', `Comment ${i + 1}: ${pin.title}`);
      const x = block ? Math.max(block.x, Math.min(block.x + block.w, thread.anchor.x)) : thread.anchor.x;
      const y = block ? Math.max(block.y, Math.min(block.y + block.h, thread.anchor.y)) : thread.anchor.y;
      pin.style.left = `${x / 32 * 100}%`; pin.style.top = `${y / 18 * 100}%`; pin.onclick = () => openThread(thread); pins.append(pin);
    });
    if (state.draftAnchor) { const pin = element('span', 'pin active', '+'); pin.style.left = `${state.draftAnchor.x / 32 * 100}%`; pin.style.top = `${state.draftAnchor.y / 18 * 100}%`; pins.append(pin); }
  }
  function closeThread() { state.openThread = null; state.draftAnchor = null; api.render(); }
  function renderThread() {
    const thread = state.threads.find(t => t.id === state.openThread);
    popover.hidden = !thread && !state.draftAnchor; document.querySelector('#workspace').classList.toggle('thread-open', !popover.hidden);
    popover.replaceChildren(); if (popover.hidden) return;
    const header = element('div', 'thread-heading'); header.append(icon('comment'), element('strong', '', thread ? `Comment ${state.threads.indexOf(thread) + 1}` : 'Leave a comment'));
    if (thread) header.append(element('span', `severity ${thread.severity}`, thread.severity)); header.append(button('x', 'Close thread', closeThread)); popover.append(header);
    if (thread) for (const message of thread.messages) { const row = element('article', 'thread-message'); row.append(author(message), element('p', 'message-text', message.text)); message.edits.forEach(edit => row.append(card(thread.id, edit))); popover.append(row); }
    if (thread?.resolved) popover.append(element('p', 'hint', 'Resolved. Kept with your deck.'));
    const reply = element('textarea'); reply.maxLength = 2000; reply.placeholder = thread ? 'Reply to this thread…' : 'What could be better here?'; reply.setAttribute('aria-label', thread ? 'Reply to thread' : 'Comment text');
    const draftKey = thread?.id || JSON.stringify(state.draftAnchor); reply.value = state.drafts[draftKey] || ''; reply.oninput = () => state.drafts[draftKey] = reply.value; popover.append(reply);
    function send(toNexus) {
      if (!reply.value.trim()) { reply.focus(); return; }
      const text = reply.value; delete state.drafts[draftKey];
      api.command(thread ? { command:'CommentReply', thread:thread.id, text, send:toNexus } : { command:'CommentCreate', anchor:state.draftAnchor, text, send:toNexus }, event => {
        if (event.event === 'Error') { state.drafts[draftKey] = text; return; }
        state.draftAnchor = null; if (!thread) state.openThread = state.threads.at(-1)?.id; api.render();
      });
    }
    const actions = element('div', 'thread-actions'), note = element('button', '', 'Note'), ask = element('button', 'primary', 'Send to nexus');
    note.onclick = () => send(false); ask.prepend(icon('send')); ask.disabled = !state.linked; ask.onclick = () => send(true); actions.append(note, ask); popover.append(actions);
    if (!state.linked) popover.append(element('p', 'hint', 'Not linked to a conversation — open it from nexus with /studio.'));
    if (thread) { const resolve = element('button', '', thread.resolved ? 'Reopen thread' : 'Resolve thread'); resolve.prepend(icon('check')); resolve.style.marginTop = '16px'; resolve.style.fontSize = '11px'; resolve.onclick = () => api.command({ command:thread.resolved ? 'CommentReopen' : 'CommentResolve', thread:thread.id }); popover.append(resolve); }
  }
  function renderComments() {
    const count = state.threads.filter(t => !t.resolved).length; document.querySelector('#comment-count').textContent = count; document.querySelector('#review-count').textContent = count; comments.replaceChildren();
    for (const [i, slide] of state.deck.slides.entries()) {
      const threads = state.threads.filter(t => t.anchor.slide === slide.id && Boolean(t.resolved) === (filter === 'resolved')); if (!threads.length) continue;
      comments.append(element('div', 'comments-slide', `Slide ${String(i + 1).padStart(2, '0')}`));
      for (const thread of threads) { const row = element('button', 'comment-row'); row.append(element('span', `severity ${thread.severity}`, thread.severity), element('p', '', thread.messages[0]?.text || ''), element('small', '', `${thread.messages.length} ${thread.messages.length === 1 ? 'message' : 'messages'} · ${thread.messages[0]?.role === 'model' ? 'nexus' : 'You'}`)); row.onclick = () => openThread(thread); comments.append(row); }
    }
    if (!comments.childNodes.length) comments.append(element('p', 'hint', filter === 'open' ? 'Nothing to resolve. Pin a note here, or ask for a review in nexus.' : 'Resolved threads appear here.'));
  }
  function render() { renderThread(); renderPins(); renderComments(); }
  function clearPreview() { state.preview = null; state.previewTarget = null; document.querySelector('#preview-bar').hidden = true; }
  function event(event) {
    if (event.event === 'Feedback') { state.threads = event.threads; render(); if (event.notes.length) api.toast(event.notes.join('; ')); }
    else if (event.event === 'Preview' && state.previewTarget) {
      const target = state.previewTarget; state.preview = { deck:event.deck, slideId:target.slideId, before:false }; state.slideId = target.slideId;
      document.querySelector('#preview-bar').hidden = false; document.querySelector('#preview-apply').onclick = () => api.command({ command:'ApplyEdit', owner:target.owner, edit:target.edit }); api.render();
    }
  }
  function togglePreview() { if (state.preview) { state.preview.before = !state.preview.before; document.querySelector('#preview-toggle').textContent = state.preview.before ? 'Before · B for after' : 'After · B for before'; api.renderCanvas(); } }
  document.querySelector('#preview-toggle').onclick = togglePreview; document.querySelector('#close-preview').onclick = () => { clearPreview(); api.render(); };
  for (const name of ['open','resolved']) document.querySelector(`#filter-${name}`).onclick = () => { filter = name; for (const f of ['open','resolved']) document.querySelector(`#filter-${f}`).setAttribute('aria-pressed', String(f === filter)); renderComments(); };
  document.querySelector('#show-resolved').onchange = e => { state.showResolved = e.target.checked; renderPins(); };
  return { render, event, clearPreview, togglePreview, closeThread, renderThread, renderPins };
}
