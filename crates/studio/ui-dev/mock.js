// Dev-only IPC fixture. Injected by shot.mjs, never embedded or loaded by studio.
// It plays the backend's part: the deck is the single source of truth, every
// edit is saved at once, and the model (the `deck` tool) can change the deck
// from outside, which arrives as one undo step with the changed blocks flashing.
(() => {
  const fixture = structuredClone(window.__studioFixture);
  let deck = fixture.deck, threads = fixture.threads, sequence = 0;
  const undo = [], redo = [], sent = [];
  const emit = event => window.studioEvent(event);
  const feedback = () => ({ event:'Feedback', threads, notes:[] });
  function current(extra = {}) {
    return { event:'Deck', deck, revision:`r${sequence}`, hashes:Object.fromEntries(deck.slides.flatMap(s => s.blocks.map(b => [b.id, 'mock']))), images:{}, threads, linked:true, changed:[], notice:null, ...extra };
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
  const edits = () => threads.flatMap(t => t.messages).flatMap(m => m.edits);
  // What the model does from outside: change the deck file. One undo step.
  window.__mock = {
    sent,
    emitCurrent: extra => window.studioAsyncEvent(current(extra)),
    externalEdit(ops, focusSlide) {
      const before = deck, next = apply(ops);
      const old = before.slides.flatMap(s => s.blocks);
      const changed = next.slides.flatMap(s => s.blocks).filter(b => !old.some(o => JSON.stringify(o) === JSON.stringify(b))).map(b => b.id);
      undo.push(before); redo.length = 0; deck = next; sequence++;
      window.studioAsyncEvent(current({ changed, notice:'The deck changed on disk and was reloaded. Undo takes the change back.' }));
      if (focusSlide) window.studioAsyncEvent({ event:'Focus', slide:focusSlide });
    },
  };
  window.ipc = { postMessage(source) {
    const m = JSON.parse(source);
    setTimeout(() => {
      if (m.command === 'Focus') { sent.push({ type:'Focus', slide:m.slide, blocks:m.blocks }); return emit(feedback()); }
      if (m.command === 'PreviewEdit') return emit({ event:'Preview', deck:apply(edits().find(e => e.id === m.edit).ops) });
      if (m.command === 'DismissEdit') { edits().find(e => e.id === m.edit).status = 'dismissed'; return emit(feedback()); }
      if (m.command === 'ApplyEdit') {
        const edit = edits().find(e => e.id === m.edit); undo.push(deck); redo.length = 0; deck = apply(edit.ops); edit.status = 'applied'; sequence++; return emit(current());
      }
      if (m.command === 'CommentResolve' || m.command === 'CommentReopen') { threads.find(t => t.id === m.thread).resolved = m.command === 'CommentResolve' ? Date.now()/1000 : null; return emit(feedback()); }
      if (m.command === 'CommentCreate') {
        threads.push({ id:`new-${++sequence}`, anchor:m.anchor, severity:'consider', resolved:null, created:Date.now()/1000, messages:[{ id:`msg-${sequence}`, role:'you', text:m.text, created:Date.now()/1000, edits:[] }] });
        if (m.send) sent.push({ type:'Comment', text:m.text, slide:m.anchor.slide, block:m.anchor.block });
        return emit(feedback());
      }
      if (m.command === 'CommentReply') {
        threads.find(t => t.id === m.thread).messages.push({ id:`msg-${++sequence}`, role:'you', text:m.text, created:Date.now()/1000, edits:[] });
        if (m.send) sent.push({ type:'Reply', text:m.text, thread:m.thread });
        return emit(feedback());
      }
      if (m.command === 'Undo' && undo.length) { redo.push(deck); deck = undo.pop(); sequence++; }
      else if (m.command === 'Redo' && redo.length) { undo.push(deck); deck = redo.pop(); sequence++; }
      else if (m.command === 'Apply') { undo.push(deck); redo.length = 0; deck = apply(m.ops); sequence++; }
      else if (m.command === 'Save') deck = m.deck;
      emit(current());
    }, 15);
  } };
})();
