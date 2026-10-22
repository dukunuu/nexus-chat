import { element } from './render.js';
import { inlineTokens, allNodes, slideSize, resolveFont } from './helpers.js';
import { button, icon } from './icons.js';

function inlineMarkdown(text) {
  const fragment = document.createDocumentFragment();
  for (const token of inlineTokens(text)) {
    // Links remain text, including javascript: and other untrusted targets.
    fragment.append(element(['strong', 'em', 'code'].includes(token.kind) ? token.kind : 'span', '', token.text));
  }
  return fragment;
}
function editSummary(edit, deck) {
  const parts = [], labels = [];
    for (const op of edit.ops) {
    const before = edit.guards?.[op.id || op.target];
    const id = op.id || op.target;
    const label = before?.name || (id && !/^b\d|^b[-_]\d/.test(id) ? id.replaceAll(/[-_]/g, ' ').replace(/^./, c => c.toUpperCase()) : before?.text?.replace(/[#*`]/g, '').split('\n')[0]) || id;
    if (label && !labels.includes(label)) labels.push(label);
    const fields = op.fields;
    if (fields && before) {
      for (const [key, value] of Object.entries(fields)) {
        if (key === 'font') {
          for (const [field, next] of Object.entries(value)) {
            const previous = before[key]?.[field] ?? (deck ? resolveFont(deck.theme, before.style, {})[field] : undefined);
            if (JSON.stringify(previous) === JSON.stringify(next)) continue;
            if (field === 'size') parts.push(`size ${previous ?? 'default'} → ${next} px`);
            else parts.push(`${field.replaceAll('_', ' ')} ${previous ?? 'default'} → ${next}`);
          }
        } else if (key === 'text' || key === 'source') parts.push(`revise ${key === 'text' ? 'text' : 'code'}`);
        else if (value && typeof value === 'object' && !Array.isArray(value) && ['sizing', 'position', 'layout'].includes(key)) {
          for (const [field, next] of Object.entries(value)) {
            const previous = before[key]?.[field];
            if (JSON.stringify(previous) !== JSON.stringify(next)) parts.push(`${key === 'sizing' ? 'size' : key} ${field.replaceAll('_', ' ')} ${previous ?? 'auto'} → ${Array.isArray(next) ? next.join(' ') : next}`);
          }
        } else if (typeof value !== 'object') parts.push(`${key} ${before[key] ?? 'default'} → ${value}`);
      }
    } else if (fields) parts.push(`update ${Object.keys(fields).join(', ')}`);
    else parts.push(({ insert_block:'Add layer', insert_node:'Add layer', delete_block:'Delete layer', delete_node:'Delete layer', add_slide:'Add slide', delete_slide:'Delete slide', move_slide:'Reorder slide', move_node:'Move layer', set_layout:'Change layout', set_theme:'Update deck theme', set_notes:'Update speaker notes', set_text_style:'Update text style', wrap:'Create auto-layout frame', unwrap:'Remove frame' })[op.op] || 'Update layer');
  }
  const label = labels.length === 1 ? `${labels[0].length > 28 ? `${labels[0].slice(0, 28)}…` : labels[0]} · ` : '';
  return `${label}${[...new Set(parts)].slice(0, 3).join(', ') || 'Update selection'}`;
}
export function editSlide(deck, edit, fallback) {
  for (const op of edit.ops) {
    if (op.op === 'add_slide') return op.slide.id;
    if (typeof op.slide === 'string') return op.slide;
    const slide = deck.slides.find(s => [op.target, op.id, op.parent, op.slide].includes(s.id) || allNodes(s).some(b => b.id === op.target || b.id === op.id || b.id === op.parent));
    if (slide) return slide.id;
  }
  return fallback;
}
export function feedbackController(api) {
  const state = api.state, pins = document.querySelector('#pins'), popover = document.querySelector('#thread-popover'), comments = document.querySelector('#comments-list');
  let filter = 'open';
  function author(message) {
    const row = element('div', `message-author ${message.role}`), avatar = element('span', 'avatar', message.role === 'model' ? '✦' : '❯');
    row.append(avatar, element('span', '', message.role === 'model' ? 'nexus' : 'You'), element('time', '', new Date(message.created * 1000).toLocaleTimeString([], { hour:'2-digit', minute:'2-digit' }))); return row;
  }
  function card(owner, edit) {
    const node = element('div', `edit-card ${edit.status}`), info = element('div', 'edit-info');
    info.append(element('div', 'edit-kicker', editSummary(edit, state.deck)));
    if (edit.why && !/^suggested edit\.?$/i.test(edit.why.trim())) {
      const reason = element('p', 'edit-summary'); reason.append(inlineMarkdown(edit.why)); info.append(reason);
    }
    const actions = element('div', 'edit-actions');
    if (edit.status === 'pending') {
      const preview = element('button', '', 'Preview'); preview.onclick = () => { state.previewTarget = { owner, edit:edit.id, slideId:editSlide(state.deck, edit, state.slideId) }; api.command({ command:'PreviewEdit', owner, edit:edit.id }); };
      const apply = element('button', 'primary', 'Apply'); apply.onclick = () => api.command({ command:'ApplyEdit', owner, edit:edit.id });
      const dismiss = element('button', 'dismiss', 'Dismiss'); dismiss.onclick = () => api.command({ command:'DismissEdit', owner, edit:edit.id });
      actions.append(preview, apply, dismiss);
    } else actions.append(element('span', 'edit-status', edit.status === 'outdated' ? 'Outdated — ask again in nexus' : edit.status === 'applied' ? 'Applied · undo with Cmd+Z' : 'Dismissed'));
    info.append(actions); node.append(info); return node;
  }
  function openThread(thread) { clearPreview(); state.openThread = thread.id; state.draftAnchor = null; state.slideId = thread.anchor.slide; state.selected.clear(); api.render(); }
  function renderPins() {
    pins.replaceChildren();
    const canvas = document.querySelector('#canvas'), bounds = canvas.getBoundingClientRect();
    if (!bounds.width || !bounds.height || !api.slide()) return;
    const scale = bounds.width / canvas.clientWidth;
    pins.style.setProperty('--pin-scale', 1 / scale);
    const occupied = [...canvas.querySelectorAll('.block:not(.block-frame)')].map(node => {
      const box = node.getBoundingClientRect();
      const range = document.createRange(); range.selectNodeContents(node);
      const ink = [...range.getClientRects()];
      return { left:Math.max(0, Math.min(box.left, ...ink.map(r => r.left)) - bounds.left), right:Math.min(bounds.width, Math.max(box.right, ...ink.map(r => r.right)) - bounds.left), top:Math.max(0, Math.min(box.top, ...ink.map(r => r.top)) - bounds.top), bottom:Math.min(bounds.height, Math.max(box.bottom, ...ink.map(r => r.bottom)) - bounds.top) };
    });
    const placed = [];
    function add(anchor, label, severity, preview, active, click) {
      const size = slideSize(state.deck.theme), at = id => { const rect = id && api.rectOf(id); return rect || null; };
      const block = at(anchor.block);
      const ax = anchor.x / size.w * bounds.width, ay = anchor.y / size.h * bounds.height;
      const box = block ? { left:block.x / size.w * bounds.width, right:(block.x + block.w) / size.w * bounds.width, top:block.y / size.h * bounds.height, bottom:(block.y + block.h) / size.h * bounds.height } : { left:ax, right:ax, top:ay, bottom:ay };
      const corners = block ? [[box.left, box.top, -1, -1], [box.right, box.top, 1, -1], [box.left, box.bottom, -1, 1], [box.right, box.bottom, 1, 1]] : [[ax, ay, 1, -1]];
      corners.sort((a, b) => Math.hypot(a[0] - ax, a[1] - ay) - Math.hypot(b[0] - ax, b[1] - ay));
      const clear = (x, y) => ![...occupied, ...placed].some(r => x + 17 > r.left && x - 17 < r.right && y + 17 > r.top && y - 17 < r.bottom);
      let x, y;
      search: for (let offset = 22; offset <= Math.max(bounds.width, bounds.height) + 22; offset += 28) {
        for (const [cx, cy, dx, dy] of corners) {
          const px = cx + dx * offset, py = cy + dy * offset;
          if (clear(px, py)) { x = px; y = py; break search; }
        }
      }
      placed.push({ left:x - 14, right:x + 14, top:y - 14, bottom:y + 14 });
      const line = element('span', `pin-leader ${severity}`), length = Math.hypot(x - ax, y - ay);
      Object.assign(line.style, { left:`${ax / bounds.width * 100}%`, top:`${ay / bounds.height * 100}%`, width:`${Math.max(0, length - 12) / scale}px`, height:`${1 / scale}px`, transform:`rotate(${Math.atan2(y - ay, x - ax)}rad)` });
      const pin = element(click ? 'button' : 'span', `pin ${severity}${active ? ' active' : ''}`, label);
      pin.dataset.preview = preview; pin.setAttribute('aria-label', `Comment ${label}: ${preview}`);
      pin.style.left = `${x / bounds.width * 100}%`; pin.style.top = `${y / bounds.height * 100}%`;
      if (click) pin.onclick = click;
      pins.append(line, pin);
    }
    state.threads.forEach((thread, i) => {
      if (thread.anchor.slide !== state.slideId || (thread.resolved && !state.showResolved)) return;
      const preview = inlineTokens(thread.messages[0]?.text || 'Comment thread').map(token => token.text).join('').replace(/\s+/g, ' ');
      add(thread.anchor, String(i + 1), thread.severity, preview, state.openThread === thread.id, () => openThread(thread));
    });
    if (state.draftAnchor) add(state.draftAnchor, '+', 'consider', 'New comment', true);
  }
  function closeThread() { state.openThread = null; state.draftAnchor = null; api.render(); }
  function renderThread() {
    const thread = state.threads.find(t => t.id === state.openThread);
    popover.hidden = !thread && !state.draftAnchor; document.querySelector('#workspace').classList.toggle('thread-open', !popover.hidden);
    popover.replaceChildren(); if (popover.hidden) return;
    const header = element('div', 'thread-heading'); header.append(icon('comment'), element('strong', '', thread ? `Comment ${state.threads.indexOf(thread) + 1}` : 'Leave a comment'));
    if (thread) header.append(element('span', `severity ${thread.severity}`, thread.severity)); header.append(button('x', 'Close thread', closeThread)); popover.append(header);
    if (thread) for (const message of thread.messages) {
      const row = element('article', 'thread-message'), text = element('p', 'message-text'); text.append(inlineMarkdown(message.text));
      row.append(author(message), text); message.edits.forEach(edit => row.append(card(thread.id, edit))); popover.append(row);
    }
    if (thread?.resolved) popover.append(element('p', 'hint', 'Resolved. Kept with your deck.'));
    const reply = element('textarea'); reply.maxLength = 2000; reply.placeholder = thread ? 'Reply to this thread…' : 'Add a note or request a change…'; reply.setAttribute('aria-label', thread ? 'Reply to thread' : 'Comment text');
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
      for (const thread of threads) { const row = element('button', 'comment-row'), text = element('p'); text.append(inlineMarkdown(thread.messages[0]?.text || '')); row.append(element('span', `severity ${thread.severity}`, thread.severity), text, element('small', '', `${thread.messages.length} ${thread.messages.length === 1 ? 'message' : 'messages'} · ${thread.messages[0]?.role === 'model' ? 'nexus' : 'You'}`)); row.onclick = () => openThread(thread); comments.append(row); }
    }
    if (!comments.childNodes.length) comments.append(element('p', 'hint', filter === 'open' ? 'No open comments. Pin a note or ask for a review in nexus.' : 'Resolved threads appear here.'));
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
  function togglePreview() { if (state.preview) { state.preview.before = !state.preview.before; document.querySelector('#preview-toggle').textContent = state.preview.before ? 'Before · B for after' : 'After · B for before'; api.renderCanvas(); renderPins(); } }
  document.querySelector('#preview-toggle').onclick = togglePreview; document.querySelector('#close-preview').onclick = () => { clearPreview(); api.render(); };
  for (const name of ['open','resolved']) document.querySelector(`#filter-${name}`).onclick = () => { filter = name; for (const f of ['open','resolved']) document.querySelector(`#filter-${f}`).setAttribute('aria-pressed', String(f === filter)); renderComments(); };
  document.querySelector('#show-resolved').onchange = e => { state.showResolved = e.target.checked; renderPins(); };
  return { render, event, clearPreview, togglePreview, closeThread, renderThread, renderPins };
}
