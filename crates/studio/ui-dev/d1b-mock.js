// Window-free D1 editor IPC fixture. Only this harness loads it.
(() => {
  let deck = structuredClone(window.__studioFixture.deck), sequence = 0;
  const undo = [], redo = [], commands = [];
  const walk = parent => (parent.children || []).flatMap(n => [n, ...walk(n)]);
  const find = id => deck.slides.find(s => s.id === id) || deck.slides.flatMap(walk).find(n => n.id === id);
  const parent = id => [ ...deck.slides, ...deck.slides.flatMap(walk) ].find(n => n.children?.some(c => c.id === id));
  const rect = id => {
    const canvas = document.querySelector('#canvas'), el = canvas?.querySelector(`[data-id="${id}"]`);
    if (!el) return { x:0,y:0,w:0,h:0 };
    const b = canvas.getBoundingClientRect(), r = el.getBoundingClientRect(), scale = b.width / deck.theme.size.w;
    return { x:(r.x-b.x)/scale, y:(r.y-b.y)/scale, w:r.width/scale, h:r.height/scale };
  };
  const merge = (target, patch) => { for (const [k, v] of Object.entries(patch)) { if (v === null) delete target[k]; else if (v && typeof v === 'object' && !Array.isArray(v)) merge(target[k] ||= {}, v); else target[k] = structuredClone(v); } };
  const current = () => ({ event:'Deck', deck, revision:String(sequence), hashes:Object.fromEntries(deck.slides.flatMap(walk).map(n => [n.id,'mock'])), images:{}, threads:[], linked:false, changed:[], notice:null });
  function apply(ops) {
    const rects = Object.fromEntries(deck.slides.flatMap(walk).map(n => [n.id, rect(n.id)]));
    for (const op of ops) {
      if (op.op === 'update_node' || op.op === 'update_slide') { const n = find(op.id); merge(n, op.fields); if (['text','table','code'].includes(n.kind)) n.style ||= n.kind === 'code' ? 'code' : 'body'; }
      else if (op.op === 'set_theme') deck.theme = structuredClone(op.theme);
      else if (op.op === 'set_text_style') { if (!/^[a-z0-9_-]{1,32}$/.test(op.name)) throw new Error('Invalid style name'); deck.theme.text_styles[op.name] = { ...(deck.theme.text_styles[op.name] || deck.theme.text_styles.body), ...op.font }; }
      else if (op.op === 'set_layout') {
        const n = find(op.target), old = n.layout?.mode || 'none';
        n.layout = structuredClone(op.layout);
        if (old === 'none' && n.layout.mode !== 'none') {
          n.children.sort((a,b) => rects[a.id][n.layout.mode === 'row' ? 'x' : 'y'] - rects[b.id][n.layout.mode === 'row' ? 'x' : 'y']);
          n.children.forEach(c => c.position = 'auto');
        } else if (old !== 'none' && n.layout.mode === 'none') {
          const o = rects[n.id] || { x:0,y:0 };
          n.children.forEach(c => c.position = { x:rects[c.id].x-o.x, y:rects[c.id].y-o.y });
          if (n.kind === 'frame') { n.sizing ||= {}; for (const axis of ['w','h']) if (typeof n.sizing[axis] !== 'number') n.sizing[axis] = rects[n.id][axis]; }
        }
      } else if (op.op === 'move_node') {
        const p = parent(op.id), n = find(op.id), target = find(op.parent);
        p.children.splice(p.children.indexOf(n), 1);
        target.children.splice(op.index ?? target.children.length, 0, n);
      } else if (op.op === 'wrap') {
        const p = parent(op.ids[0]), nodes = p.children.filter(n => op.ids.includes(n.id));
        const first = Math.min(...nodes.map(n => p.children.indexOf(n)));
        const x = Math.min(...nodes.map(n => rects[n.id].x)), y = Math.min(...nodes.map(n => rects[n.id].y));
        const base = rects[p.id] || { x:0,y:0 };
        p.children = p.children.filter(n => !op.ids.includes(n.id));
        nodes.sort((a,b) => rects[a.id][op.layout.mode === 'row' ? 'x' : 'y'] - rects[b.id][op.layout.mode === 'row' ? 'x' : 'y']);
        const free = (p.layout?.mode || 'none') === 'none' || nodes[0].position !== 'auto';
        nodes.forEach(n => n.position = 'auto');
        p.children.splice(first,0,{ id:op.id, kind:'frame', layout:op.layout, position:free ? { x:x-base.x,y:y-base.y } : 'auto', children:nodes });
      } else if (op.op === 'unwrap') {
        const n = find(op.id), p = parent(op.id), at = p.children.indexOf(n), origin = rects[p.id] || { x:0,y:0 };
        n.children.forEach(c => { if ((p.layout?.mode || 'none') === 'none') c.position = { x:rects[c.id].x-origin.x,y:rects[c.id].y-origin.y }; });
        p.children.splice(at,1,...n.children);
      } else if (op.op === 'insert_node') find(op.parent).children.splice(op.index ?? find(op.parent).children.length,0,op.node);
      else if (op.op === 'delete_node') { const p = parent(op.id); p.children = p.children.filter(n => n.id !== op.id); }
      else throw new Error(`Unimplemented mock operation ${op.op}`);
    }
  }
  window.__d1 = { commands, get deck() { return structuredClone(deck); }, get undoDepth() { return undo.length; }, node:id => structuredClone(find(id)) };
  window.ipc = { postMessage(source) {
    const command = JSON.parse(source); commands.push(command);
    setTimeout(() => {
      try {
        if (command.command === 'ListFonts') return window.studioEvent({ event:'Fonts', families:[{ family:'Helvetica Neue',weights:[100,200,300,400,500,700,900],italic:true,variable:false }, { family:'Menlo',weights:[400,700],italic:true,variable:false }, { family:'Georgia',weights:[400,700],italic:true,variable:false }, { family:'Arial',weights:[400,700],italic:true,variable:false }] });
        if (command.command === 'Apply') { const before = structuredClone(deck); apply(command.ops); if (JSON.stringify(before) !== JSON.stringify(deck)) { undo.push(before); redo.length=0; sequence++; } }
        else if (command.command === 'Undo' && undo.length) { redo.push(deck); deck=undo.pop(); sequence++; }
        else if (command.command === 'Redo' && redo.length) { undo.push(deck); deck=redo.pop(); sequence++; }
        window.studioEvent(current());
      } catch (error) { window.studioEvent({ event:'Error', message:error.message }); throw error; }
    }, 10);
  } };
})();
