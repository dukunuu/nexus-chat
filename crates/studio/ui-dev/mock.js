// Dev-only IPC fixture. Injected by shot.mjs, never embedded or loaded by studio.
(() => {
  const fixture = structuredClone(window.__studioFixture);
  let deck = fixture.deck, threads = fixture.threads, chat = fixture.chat, sequence = 0;
  const undo = [], redo = [];
  const emit = event => window.studioEvent(event);
  const feedback = () => ({ event:'Feedback', threads, chat, notes:[] });
  function current() {
    return { event:'Deck', deck, revision:`r${sequence}`, hashes:Object.fromEntries(deck.slides.flatMap(s => s.blocks.map(b => [b.id, 'mock']))), dirty:undo.length > 0, images:{}, threads, chat };
  }
  function apply(ops) {
    const next = structuredClone(deck);
    for (const op of ops) {
      const slide = next.slides.find(s => s.id === op.slide);
      if (op.op === 'update_block') Object.assign(slide.blocks.find(b => b.id === op.id), op.fields);
      else if (op.op === 'set_notes') slide.notes = op.notes;
      else if (op.op === 'set_layout') slide.layout = op.layout;
      else if (op.op === 'restyle') { for (const s of next.slides) for (const b of s.blocks) if (['deck', s.id, b.id].includes(op.target)) b.style = op.style; }
      else if (op.op === 'set_theme') next.theme = op.theme;
      else if (op.op === 'delete_block') slide.blocks = slide.blocks.filter(b => b.id !== op.id);
      else if (op.op === 'insert_block') slide.blocks.push(op.block);
      else if (op.op === 'delete_slide') next.slides = next.slides.filter(s => s.id !== op.id);
      else if (op.op === 'add_slide') { const index = next.slides.findIndex(s => s.id === op.after); next.slides.splice(index < 0 ? next.slides.length : index + 1, 0, op.slide); }
      else if (op.op === 'move_slide') { const index = next.slides.findIndex(s => s.id === op.id); next.slides.splice(op.to, 0, ...next.slides.splice(index, 1)); }
    }
    return next;
  }
  const edits = () => [...threads.flatMap(t => t.messages), ...chat].flatMap(m => m.edits);
  window.ipc = { postMessage(source) {
    const m = JSON.parse(source);
    setTimeout(() => {
      if (m.command === 'HostStatus' || m.command === 'StartHost') return emit({ event:'HostStatus', status:{ state:'connected', model:'local / nexus', can_start:false, message:'Connected to your local host' } });
      if (m.command === 'PreviewEdit') return emit({ event:'Preview', deck:apply(edits().find(e => e.id === m.edit).ops) });
      if (m.command === 'DismissEdit') { edits().find(e => e.id === m.edit).status = 'dismissed'; return emit(feedback()); }
      if (m.command === 'ApplyEdit' || m.command === 'ChooseOption') {
        const edit = edits().find(e => e.id === m.edit); undo.push(deck); deck = apply(edit.ops); edit.status = 'applied'; sequence++; return emit(current());
      }
      if (m.command === 'CommentResolve' || m.command === 'CommentReopen') { threads.find(t => t.id === m.thread).resolved = m.command === 'CommentResolve' ? Date.now()/1000 : null; return emit(feedback()); }
      if (m.command === 'CommentCreate') {
        threads.push({ id:`new-${++sequence}`, anchor:m.anchor, severity:'consider', resolved:null, created:Date.now()/1000, messages:[{ id:`msg-${sequence}`, role:'you', text:m.text, created:Date.now()/1000, edits:[] }] });
        if (!m.send) return emit(feedback());
      }
      if (m.command === 'CommentReply') {
        threads.find(t => t.id === m.thread).messages.push({ id:`msg-${++sequence}`, role:'you', text:m.text, created:Date.now()/1000, edits:[] });
        if (!m.send) return emit(feedback());
      }
      if (['Review','Chat','CommentCreate','CommentReply'].includes(m.command)) {
        if (m.command === 'Chat') chat.push({ id:`user-${++sequence}`, role:'you', text:m.text, created:Date.now()/1000, edits:[] });
        emit({ event:'Suggesting' });
        setTimeout(() => {
          if (m.command === 'Review') { threads.push(...structuredClone(fixture.review).filter(t => !threads.some(old => old.id === t.id))); chat.push({ id:`review-${++sequence}`, role:'model', text:'The visual direction is cohesive. I’ve left two focused comments on clarity and the closing invitation, in priority order.', created:Date.now()/1000, edits:[] }); }
          else if (m.command === 'Chat') chat.push(structuredClone(fixture.options));
          else { const thread = m.thread ? threads.find(t => t.id === m.thread) : threads.at(-1); thread.messages.push({ id:`model-${++sequence}`, role:'model', text:'Let’s keep the quiet tone and make this one detail clearer. What should the audience take away from this part?', created:Date.now()/1000, edits:[] }); }
          window.studioAsyncEvent(feedback());
        }, 150); return;
      }
      if (m.command === 'Undo' && undo.length) { redo.push(deck); deck = undo.pop(); }
      else if (m.command === 'Redo' && redo.length) { undo.push(deck); deck = redo.pop(); }
      else if (m.command === 'Apply') { undo.push(deck); deck = apply(m.ops); sequence++; }
      else if (m.command === 'Save') deck = m.deck;
      emit(current());
    }, 15);
  } };
})();
