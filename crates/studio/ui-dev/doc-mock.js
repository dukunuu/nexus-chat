// Dev-only IPC fixture for the doc editor. Injected by doc-shot.mjs, never embedded.
// It plays the backend's part: the doc is the single source of truth, every
// DocApply is one undo step, and the model can edit from outside.
(() => {
  const fixture = structuredClone(window.__docFixture);
  let doc = fixture.doc, threads = fixture.threads, sequence = 1;
  const undo = [], redo = [], commands = [], sent = [], activities = {};
  const options = window.__docMockOptions || {};
  const emit = (event, async_ = false) => (async_ ? window.studioAsyncEvent : window.studioEvent)(structuredClone(event));
  const colors = window.__docColors;
  let model;
  const load = async () => (model ||= await import('/doc/model.js'));
  const revision = () => `r${sequence}`;
  const design = () => ({ slug: 'nexus', name: 'Nexus', colors: doc.theme.colors, fonts: doc.theme.fonts, update_available: Boolean(window.__mock?.designUpdate) });
  const current = (extra = {}) => ({
    event: 'Doc', doc, colors, revision: revision(), relative: 'docs/q3.doc.json', threads, linked: window.__mock?.linked ?? true, activities,
    changed: [], notice: null, assets: {}, design: design(), can_undo: undo.length > 0, can_redo: redo.length > 0, ...extra,
  });
  const feedback = () => ({ event: 'DocFeedback', threads, activities, notes: [] });
  const edits = () => threads.flatMap(t => t.messages).flatMap(m => m.edits);

  function applyOne(next, op) {
    if (op.op === 'replace_text') {
      const block = next.blocks.find(b => b.id === op.id);
      block.text = block.text.split(op.find).join(op.replace);
      return next;
    }
    if (op.op === 'insert_markdown') {
      const at = op.after == null ? next.blocks.length : next.blocks.findIndex(b => b.id === op.after) + 1;
      const blocks = op.markdown.split(/\n{2,}/).map((chunk, i) => {
        const heading = /^(#{1,3}) (.*)$/.exec(chunk);
        const id = `m${++sequence}x${i}`;
        return heading ? { id, kind: 'heading', level: heading[1].length, text: heading[2] } : { id, kind: 'paragraph', text: chunk.replace(/^- /gm, '') };
      });
      next.blocks.splice(at, 0, ...blocks);
      return next;
    }
    if (op.op === 'apply_design') return next;
    return model.applyOps(next, [op]);
  }
  function apply(ops) {
    let next = structuredClone(doc);
    for (const op of ops) {
      const done = applyOne(next, op);
      if (!done) throw new Error(`ops: cannot apply ${op.op}`);
      next = done;
    }
    return next;
  }
  const changedBlocks = (before, after) => after.blocks.filter(b => JSON.stringify(before.blocks.find(x => x.id === b.id)) !== JSON.stringify(b)).map(b => b.id);

  window.__mock = {
    sent, commands, linked: true, latency: options.latency ?? 10, rejectNext: false, designUpdate: false,
    get doc() { return structuredClone(doc); },
    get threads() { return structuredClone(threads); },
    get undoDepth() { return undo.length; },
    activity(thread, state, detail, reason) { activities[thread] = { state, detail, reason }; emit(feedback(), true); },
    emitCurrent: extra => emit(current(extra), true),
    async externalEdit(ops, focus) {
      await load();
      const before = doc, next = apply(ops);
      undo.push(before); redo.length = 0; doc = next; sequence++;
      emit(current({ changed: changedBlocks(before, next), notice: 'The doc changed on disk and was reloaded. Undo takes the change back.' }), true);
      if (focus) emit({ event: 'DocFocus', block: focus }, true);
    },
    addThread(thread) { threads.push(thread); emit(feedback(), true); },
  };
  window.ipc = {
    postMessage(source) {
      const m = JSON.parse(source);
      commands.push(m);
      setTimeout(async () => {
        await load();
        try {
          if (m.command === 'DocApply' && window.__mock.rejectNext) { window.__mock.rejectNext = false; return emit({ event: 'Error', message: 'Mock rejected apply' }); }
          if (m.command === 'DocFocus') { sent.push({ type: 'Focus', block: m.block, selection: m.selection }); return emit(feedback()); }
          if (m.command === 'DocApply') { const next = apply(m.ops); undo.push(doc); redo.length = 0; doc = next; sequence++; return emit(current()); }
          if (m.command === 'Undo') { if (undo.length) { redo.push(doc); doc = undo.pop(); sequence++; } return emit(current()); }
          if (m.command === 'Redo') { if (redo.length) { undo.push(doc); doc = redo.pop(); sequence++; } return emit(current()); }
          if (m.command === 'DocCommentCreate') {
            threads.push({ id: `new-${++sequence}`, anchor: m.anchor, severity: 'consider', resolved: null, created: Date.now() / 1000, messages: [{ id: `msg-${sequence}`, role: 'you', text: m.text, created: Date.now() / 1000, edits: [] }] });
            if (m.send) sent.push({ type: 'Comment', block: m.anchor.block, range: m.anchor.range, quote: m.anchor.quote, text: m.text });
            return emit(feedback());
          }
          if (m.command === 'CommentReply') {
            threads.find(t => t.id === m.thread).messages.push({ id: `msg-${++sequence}`, role: 'you', text: m.text, created: Date.now() / 1000, edits: [] });
            if (m.send) sent.push({ type: 'Reply', text: m.text, thread: m.thread });
            return emit(feedback());
          }
          if (m.command === 'CommentResolve' || m.command === 'CommentReopen') { threads.find(t => t.id === m.thread).resolved = m.command === 'CommentResolve' ? Date.now() / 1000 : null; return emit(feedback()); }
          if (m.command === 'PreviewEdit') {
            const edit = edits().find(e => e.id === m.edit), next = apply(edit.ops);
            return emit({ event: 'DocPreview', owner: m.owner, edit: m.edit, title: null, changed: next.blocks.filter(b => JSON.stringify(doc.blocks.find(x => x.id === b.id)) !== JSON.stringify(b)), removed: doc.blocks.filter(b => !next.blocks.some(x => x.id === b.id)).map(b => b.id) });
          }
          if (m.command === 'ApplyEdit') { const edit = edits().find(e => e.id === m.edit); const next = apply(edit.ops); undo.push(doc); redo.length = 0; doc = next; sequence++; edit.status = 'applied'; return emit(current()); }
          if (m.command === 'DismissEdit') { edits().find(e => e.id === m.edit).status = 'dismissed'; return emit(feedback()); }
          if (m.command === 'DocResolveEdits') {
            const pending = edits().filter(e => e.status === 'pending');
            if (m.accept) { const merged = pending.flatMap(e => e.ops); undo.push(doc); doc = apply(merged); sequence++; pending.forEach(e => { e.status = 'applied'; }); }
            else pending.forEach(e => { e.status = 'dismissed'; });
            return emit(current());
          }
          if (m.command === 'DocDesigns') return emit({ event: 'Designs', available: [{ slug: 'nexus', name: 'Nexus', colors: doc.theme.colors, fonts: doc.theme.fonts, current: true }, { slug: 'warm', name: 'Warm paper', colors: { bg: '#fffaf0', fg: '#3b2f2f', accent: '#c0392b', muted: '#8c7b75' }, fonts: doc.theme.fonts, current: false }] });
          if (m.command === 'DocApplyDesign') { sequence++; return emit(current()); }
          if (m.command === 'DocPages') return emit({ event: 'Pages', revision: revision(), pages: [1, 2, 3].map(n => ({ n, width: 595.28, height: 841.89, src: `/pages/${revision()}-${n}.png` })) });
          if (m.command === 'DocExport' || m.command === 'ExportAs') return emit({ event: 'Exported', path: `/tmp/exports/q3-strategy.${m.format}`, warnings: [] });
          if (m.command === 'DocImportAsset') return emit({ event: 'Asset', src: 'assets/pasted.png' });
          if (m.command === 'Reveal') return emit({ event: 'Revealed' });
          return emit(current());
        } catch (error) { return emit({ event: 'Error', message: error.message }); }
      }, window.__mock.latency);
    },
  };
})();
