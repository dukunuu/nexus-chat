// Dev-only IPC fixture. Injected by shot.mjs, never embedded or loaded by studio.
// It plays the backend's part: the deck is the single source of truth, every
// edit is saved at once, and the model (the `deck` tool) can change the deck
// from outside, which arrives as one undo step with the changed blocks flashing.
(() => {
  const fixture = structuredClone(window.__studioFixture);
  let deck = fixture.deck, threads = fixture.threads, sequence = 0;
  const undo = [], redo = [], sent = [], activities = {};
  const emit = event => window.studioEvent(structuredClone(event));
  const options = window.__studioMockOptions || {};
  const commands = [];
  const feedback = () => ({ event:'Feedback', threads, activities, notes:[] });
  function current(extra = {}) {
    return { event:'Deck', deck, revision:`r${sequence}`, hashes:Object.fromEntries(deck.slides.flatMap(s => nodesOf(s).map(b => [b.id, 'mock']))), images:{}, threads, activities, linked:true, changed:[], notice:null, ...extra };
  }
  const nodesOf = slide => { const out = []; const walk = list => { for (const n of list) { out.push(n); walk(n.children || []); } }; walk(slide.children || []); return out; };
  const containerOf = (next, id) => { for (const s of next.slides) { if (s.id === id) return s.children; const f = nodesOf(s).find(n => n.id === id); if (f) return f.children; } };
  function merge(target, patch) { for (const [k, v] of Object.entries(patch)) { if (v === null) delete target[k]; else if (v && typeof v === 'object' && !Array.isArray(v) && target[k] && typeof target[k] === 'object') merge(target[k], v); else target[k] = structuredClone(v); } return target; }
  function apply(ops) {
    const next = structuredClone(deck);
    for (const op of ops) {
      if (op.op === 'update_node') { for (const s of next.slides) { const n = nodesOf(s).find(n => n.id === op.id); if (n) merge(n, op.fields); } }
      else if (op.op === 'set_notes') next.slides.find(s => s.id === op.slide).notes = op.notes;
      else if (op.op === 'update_slide') merge(next.slides.find(s => s.id === op.id), op.fields);
      else if (op.op === 'set_theme') next.theme = op.theme;
      else if (op.op === 'set_layout') { const t = next.slides.find(s => s.id === op.target) || next.slides.flatMap(nodesOf).find(n => n.id === op.target); t.layout = op.layout; }
      else if (op.op === 'delete_node') { for (const s of next.slides) { const list = containerOf(next, s.id); const walk = l => { const i = l.findIndex(n => n.id === op.id); if (i >= 0) l.splice(i, 1); else l.forEach(n => n.children && walk(n.children)); }; walk(list); } }
      else if (op.op === 'insert_node') { const list = containerOf(next, op.parent); list.splice(op.index ?? list.length, 0, op.node); }
      else if (op.op === 'move_node') { for (const s of next.slides) { const walk = l => { const i = l.findIndex(n => n.id === op.id); if (i >= 0) return l.splice(i, 1)[0]; for (const n of l) if (n.children) { const f = walk(n.children); if (f) return f; } }; const node = walk(s.children); if (node) { const list = containerOf(next, op.parent); list.splice(op.index ?? list.length, 0, node); break; } } }
      else if (op.op === 'delete_slide') next.slides = next.slides.filter(s => s.id !== op.id);
      else if (op.op === 'add_slide') { const index = next.slides.findIndex(s => s.id === op.after); next.slides.splice(index < 0 ? next.slides.length : index + 1, 0, op.slide); }
      else if (op.op === 'move_slide') { const index = next.slides.findIndex(s => s.id === op.id); next.slides.splice(op.to, 0, ...next.slides.splice(index, 1)); }
    }
    return next;
  }
  const edits = () => threads.flatMap(t => t.messages).flatMap(m => m.edits);
  // What the model does from outside: change the deck file. One undo step.
  window.__mock = {
    sent, commands, latency:options.latency ?? 15, rejectNextApply:false,
    get deck() { return structuredClone(deck); },
    activity(thread, state, detail, reason) {
      activities[thread] = { state, detail, reason };
      window.studioAsyncEvent(feedback());
    },
    async activitySequence(thread, sequence) {
      for (const activity of sequence) {
        this.activity(thread, activity.state, activity.detail, activity.reason);
        await new Promise(resolve => setTimeout(resolve, activity.delay || 50));
      }
    },
    emitCurrent: extra => window.studioAsyncEvent(current(extra)),
    externalEdit(ops, focusSlide) {
      const before = deck, next = apply(ops);
      const shallow = n => JSON.stringify({ ...n, children: undefined }); const old = before.slides.flatMap(nodesOf).map(shallow);
      const changed = next.slides.flatMap(nodesOf).filter(b => !old.includes(shallow(b))).map(b => b.id);
      undo.push(before); redo.length = 0; deck = next; sequence++;
      window.studioAsyncEvent(current({ changed, notice:'The deck changed on disk and was reloaded. Undo takes the change back.' }));
      if (focusSlide) window.studioAsyncEvent({ event:'Focus', slide:focusSlide });
    },
  };
  window.ipc = { postMessage(source) {
    const m = JSON.parse(source); commands.push(m);
    setTimeout(() => {
      if (m.command === 'Apply' && window.__mock.rejectNextApply) { window.__mock.rejectNextApply = false; return emit({ event:'Error', message:'Mock rejected apply' }); }
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
    }, window.__mock.latency);
  } };
})();
