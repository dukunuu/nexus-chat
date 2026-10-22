import { allNodes, childrenOf, findNode, parentOf, isAbsolute, withNewIds, slideSize } from './helpers.js';
import { renderSlide, renderNode, element } from './render.js';
import { guideController } from './guides.js';

// Canvas editing on the version-2 node tree. Boxes come from the browser's own
// layout (the same flexbox mapping the core renderer follows), in slide pixels.
export function canvasController(api) {
  const canvas = document.querySelector('#canvas');
  const width = () => slideSize(api.state.deck?.theme).w;
  const slideScale = () => canvas.getBoundingClientRect().width / width();
  const selectedNodes = () => { const slide = api.slide(); return slide ? allNodes(slide).filter(n => api.state.selected.has(n.id)) : []; };
  const nodeFor = nodeId => [...canvas.querySelectorAll('.block')].find(n => n.dataset.id === nodeId);
  /** The node's box in slide px, from the laid-out page. */
  function rectOf(nodeId) {
    const element = nodeFor(nodeId); if (!element) return null;
    const bounds = canvas.getBoundingClientRect(), box = element.getBoundingClientRect(), scale = slideScale();
    return { x: (box.left - bounds.left) / scale, y: (box.top - bounds.top) / scale, w: box.width / scale, h: box.height / scale };
  }
  const guides = guideController(canvas, slideScale);
  const locked = nodeId => { for (let n = findNode(api.slide(), nodeId); n; n = parentOf(api.slide(), n.id)?.parent) if (n.locked) return true; return false; };
  const percent = (value, total) => `${value / total * 100}%`;
  function decorate() {
    canvas.querySelectorAll('.resize-handle').forEach(n => n.remove());
    if (api.state.preview || api.state.mode === 'comment') return;
    for (const node of canvas.querySelectorAll('.block')) node.classList.toggle('selected', api.state.selected.has(node.dataset.id));
    if (api.state.selected.size !== 1) return;
    const node = selectedNodes()[0], rect = node && rectOf(node.id);
    if (!rect || locked(node.id)) return;
    const size = slideSize(api.state.deck.theme);
    for (const direction of ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w']) {
      const handle = element('div', `resize-handle handle-${direction}`);
      handle.dataset.direction = direction; handle.dataset.target = node.id;
      const x = rect.x + (direction.includes('w') ? 0 : direction.includes('e') ? rect.w : rect.w / 2);
      const y = rect.y + (direction.includes('n') ? 0 : direction.includes('s') ? rect.h : rect.h / 2);
      handle.style.left = percent(x, size.w); handle.style.top = percent(y, size.h);
      canvas.append(handle);
    }
  }
  function render() {
    guides.clear();
    const preview = api.state.preview;
    canvas.classList.toggle('previewing', Boolean(preview));
    if (preview) {
      const before = api.state.deck.slides.find(s => s.id === preview.slideId);
      const after = preview.deck.slides.find(s => s.id === preview.slideId);
      const deck = preview.before ? api.state.deck : preview.deck;
      renderSlide(canvas, preview.before ? before : after, deck.theme, api.state.images);
      if (!preview.before) {
        const shallow = n => JSON.stringify({ ...n, children: undefined });
        const old = new Map(allNodes(before).map(n => [n.id, shallow(n)]));
        const themeChanged = JSON.stringify(api.state.deck.theme) !== JSON.stringify(deck.theme);
        for (const node of allNodes(after)) {
          if (themeChanged || old.get(node.id) !== shallow(node)) nodeFor(node.id)?.classList.add('ghost-changed');
        }
        // Removed nodes that had a place of their own are drawn faintly where they were.
        const root = canvas.querySelector('.slide-root');
        const kept = new Set(allNodes(after).map(n => n.id));
        for (const node of before?.children || []) if (!kept.has(node.id) && typeof node.position === 'object') {
          const ghost = renderNode(node, api.state.deck.theme, api.state.images, { mode: 'none' }); ghost.classList.add('ghost-removed'); root.append(ghost);
        }
      }
    } else renderSlide(canvas, api.slide(), api.state.deck.theme, api.state.images);
    decorate();
  }
  let drag;
  canvas.addEventListener('pointerdown', event => {
    if (api.state.preview || event.button !== 0 || event.target.closest('[contenteditable]')) return;
    if (event.target.closest('.pin')) return;
    if (api.state.mode === 'comment') {
      const bounds = canvas.getBoundingClientRect(), size = slideSize(api.state.deck.theme);
      const node = event.target.closest('.block');
      api.commentAt({ slide: api.slide().id, block: node?.dataset.id || null,
        x: Math.max(0, Math.min(size.w - 1, Math.floor((event.clientX - bounds.left) / bounds.width * size.w))),
        y: Math.max(0, Math.min(size.h - 1, Math.floor((event.clientY - bounds.top) / bounds.height * size.h))) });
      event.preventDefault(); return;
    }
    api.state.scopeSlide = false;
    const handle = event.target.closest('.resize-handle');
    const blockNode = event.target.closest('.block');
    const nodeId = handle?.dataset.target || blockNode?.dataset.id;
    if (!nodeId) { api.state.selected.clear(); api.selectionChanged(); return; }
    if (locked(nodeId)) return;
    if (!handle) {
      if (event.shiftKey) {
        if (api.state.selected.has(nodeId)) api.state.selected.delete(nodeId);
        else api.state.selected.add(nodeId);
      } else if (!api.state.selected.has(nodeId)) api.state.selected = new Set([nodeId]);
      api.selectionChanged();
    }
    if (!api.state.selected.has(nodeId)) return;
    event.preventDefault(); canvas.focus();
    const slide = api.slide(), nodes = selectedNodes().filter(n => !locked(n.id) && !hasSelectedAncestor(n.id));
    drag = { pointerId:event.pointerId, startX: event.clientX, startY: event.clientY, direction: handle?.dataset.direction, slide: slide.id,
      nodes: nodes.map(n => ({ node:n, rect:rectOf(n.id), free:isAbsolute(slide, n), parent:parentOf(slide, n.id).parent })), changes:new Map(), moved:false, scale:slideScale() };
    canvas.setPointerCapture(event.pointerId);
  });
  function hasSelectedAncestor(nodeId) {
    for (let p = parentOf(api.slide(), nodeId)?.parent; p && p !== api.slide(); p = parentOf(api.slide(), p.id)?.parent) if (api.state.selected.has(p.id)) return true;
    return false;
  }
  function parentRect(parent) { return parent === api.slide() ? { x:0, y:0, ...slideSize(api.state.deck.theme) } : rectOf(parent.id); }
  function targetAt(event, absolute) {
    const excluded = new Set(drag.nodes.flatMap(({ node }) => [node.id, ...allNodes(node).map(n => n.id)]));
    const frames = document.elementsFromPoint(event.clientX, event.clientY).filter(el => el.matches('#canvas .block-frame') && !excluded.has(el.dataset.id) && !locked(el.dataset.id));
    return frames.map(el => findNode(api.slide(), el.dataset.id)).find(n => absolute ? (n.layout?.mode || 'none') === 'none' : (n.layout?.mode || 'none') !== 'none') || (absolute || (api.slide().layout?.mode || 'none') !== 'none' ? api.slide() : drag.nodes.find(n => !n.free)?.parent || api.slide());
  }
  function insertion(parent, event) {
    const row = parent.layout?.mode !== 'column', axis = row ? 'x' : 'y', dimension = row ? 'w' : 'h', cross = row ? 'y' : 'x', crossSize = row ? 'h' : 'w';
    const bounds = canvas.getBoundingClientRect(), point = { x:(event.clientX - bounds.x) / drag.scale, y:(event.clientY - bounds.y) / drag.scale };
    const remaining = childrenOf(parent).filter(n => !drag.nodes.some(i => i.node.id === n.id));
    const items = remaining.map((n, index) => ({ n, index, r:rectOf(n.id) })).filter(i => !i.n.hidden && typeof i.n.position !== 'object' && i.r);
    let nearest;
    for (const item of items) {
      const distance = Math.max(item.r[cross] - point[cross], 0, point[cross] - item.r[cross] - item.r[crossSize]) * 10000 + Math.abs(point[axis] - item.r[axis] - item.r[dimension] / 2);
      if (!nearest || distance < nearest.distance) nearest = { ...item, distance };
    }
    const after = nearest && point[axis] > nearest.r[axis] + nearest.r[dimension] / 2;
    const index = nearest ? nearest.index + (after ? 1 : 0) : remaining.length;
    const r = nearest?.r || parentRect(parent), position = nearest ? r[axis] + (after ? r[dimension] : 0) : r[axis] + (parent.layout?.padding?.[row ? 3 : 0] || 0);
    const line = element('div', 'insertion-line');
    const b = canvas.getBoundingClientRect();
    Object.assign(line.style, { position:'fixed', left:`${b.x + (row ? position : r.x) * drag.scale}px`, top:`${b.y + (row ? r.y : position) * drag.scale}px`, width:row ? '2px' : `${r.w * drag.scale}px`, height:row ? `${r.h * drag.scale}px` : '2px' }); document.body.append(line); drag.line = line;
    return index;
  }
  canvas.addEventListener('pointermove', event => {
    if (!drag) { if (event.altKey) measureHover(event); else guides.clear(); return; }
    let dx = Math.round((event.clientX - drag.startX) / drag.scale), dy = Math.round((event.clientY - drag.startY) / drag.scale);
    const constrained = event.shiftKey && !drag.direction ? Math.abs(dx) >= Math.abs(dy) ? 'x' : 'y' : null;
    if (constrained === 'x') dy = 0; if (constrained === 'y') dx = 0;
    drag.moved ||= Math.abs(event.clientX - drag.startX) + Math.abs(event.clientY - drag.startY) > 2;
    if (!drag.moved) return;
    canvas.classList.add('dragging'); canvas.querySelectorAll('.resize-handle').forEach(n => n.hidden = true);
    drag.line?.remove(); drag.line = null; drag.drop = null;
    const size = slideSize(api.state.deck.theme), unit = px => `${px / size.w * 100}cqw`, absolute = event.ctrlKey || event.metaKey;
    const target = !drag.direction ? targetAt(event, absolute) : null;
    const flow = target && !absolute && (target.layout?.mode || 'none') !== 'none' && (drag.nodes.some(n => !n.free) || target !== drag.nodes[0]?.parent);
    if (flow) { drag.drop = { parent:target, index:insertion(target, event) }; guides.clear(); }
    else if (!drag.direction && (absolute || drag.nodes.some(n => !n.free))) drag.drop = { parent:absolute ? target : api.slide(), absolute:true };
    let group;
    if (!drag.direction && !drag.drop && drag.nodes.length > 1 && drag.nodes.every(n => n.free && n.rect)) {
      const x = Math.min(...drag.nodes.map(n => n.rect.x)), y = Math.min(...drag.nodes.map(n => n.rect.y));
      const box = { x:x + dx, y:y + dy, w:Math.max(...drag.nodes.map(n => n.rect.x + n.rect.w)) - x, h:Math.max(...drag.nodes.map(n => n.rect.y + n.rect.h)) - y };
      const siblings = [...new Set(drag.nodes.flatMap(n => childrenOf(n.parent)))].filter(n => !n.hidden && !api.state.selected.has(n.id)).map(n => rectOf(n.id)).filter(Boolean);
      const snapped = guides.snap(box, siblings, { x:0,y:0,...size }, null, constrained);
      dx += snapped.rect.x - box.x; dy += snapped.rect.y - box.y; group = { ...snapped, siblings };
    }
    for (const { node, rect, free, parent } of drag.nodes) {
      const el = nodeFor(node.id); if (!el || !rect) continue;
      const origin = parentRect(parent), siblings = childrenOf(parent).filter(n => !n.hidden && !api.state.selected.has(n.id)).map(n => rectOf(n.id)).filter(Boolean);
      let box = { ...rect, x:rect.x + dx, y:rect.y + dy }, fields = {};
      if (drag.direction) {
        const west = drag.direction.includes('w'), east = drag.direction.includes('e'), north = drag.direction.includes('n'), south = drag.direction.includes('s'), factor = event.altKey ? 2 : 1;
        let w = Math.max(1, rect.w + (east ? dx : west ? -dx : 0) * factor), h = Math.max(1, rect.h + (south ? dy : north ? -dy : 0) * factor);
        if (event.shiftKey) { const ratio = rect.w / Math.max(1, rect.h); if (east || west) h = w / ratio; else w = h * ratio; }
        box = { x:rect.x + (event.altKey ? (rect.w - w) / 2 : west ? rect.w - w : 0), y:rect.y + (event.altKey ? (rect.h - h) / 2 : north ? rect.h - h : 0), w, h };
        if (free) {
          const snapped = guides.snap(box, siblings, { x:0,y:0,...size }, drag.direction);
          if (event.altKey) {
            if (west || east) box.w = Math.max(1, box.w + 2 * (snapped.rect.w - box.w));
            if (north || south) box.h = Math.max(1, box.h + 2 * (snapped.rect.h - box.h));
          } else box = snapped.rect;
          if (event.shiftKey) { if (west || east) box.h = box.w * rect.h / Math.max(1, rect.w); else box.w = box.h * rect.w / Math.max(1, rect.h); }
          box.x = event.altKey ? rect.x + (rect.w - box.w) / 2 : west ? rect.x + rect.w - box.w : rect.x;
          box.y = event.altKey ? rect.y + (rect.h - box.h) / 2 : north ? rect.y + rect.h - box.h : rect.y;
          guides.show(box, siblings, snapped.lines.filter(line => !event.shiftKey || line.axis === (west || east ? 'x' : 'y')));
        } else guides.show(box, siblings);
        fields.sizing = {};
        if (west || east || event.shiftKey) fields.sizing.w = Math.max(1, Math.round(box.w));
        if (north || south || event.shiftKey) fields.sizing.h = Math.max(1, Math.round(box.h));
        // A fixed size must not keep flex-grow or absolute fill's far-edge pin.
        if (parent.layout?.mode === 'row' ? 'w' in fields.sizing : 'h' in fields.sizing) el.style.flex = '0 0 auto';
        if ('w' in fields.sizing) el.style.right = '';
        if ('h' in fields.sizing) el.style.bottom = '';
        if ('w' in fields.sizing) el.style.width = unit(fields.sizing.w);
        if ('h' in fields.sizing) el.style.height = unit(fields.sizing.h);
        if (free) { fields.position = { x:box.x - origin.x, y:box.y - origin.y }; el.style.left = unit(fields.position.x); el.style.top = unit(fields.position.y); }
      } else if (drag.drop) {
        el.classList.add('drag-source');
        let ghost = drag.ghosts?.get(node.id);
        if (!ghost) {
          ghost = renderNode(node, api.state.deck.theme, api.state.images); ghost.classList.add('drag-ghost'); ghost.removeAttribute('data-id');
          (drag.ghosts ||= new Map()).set(node.id, ghost); canvas.append(ghost);
        }
        Object.assign(ghost.style, { position:'absolute', left:unit(box.x), top:unit(box.y), width:unit(rect.w), height:unit(rect.h), margin:'0', opacity:'.65' });
        if (drag.drop.absolute) { const o = parentRect(drag.drop.parent); fields = { position:{ x:box.x - o.x, y:box.y - o.y }, sizing:{ w:Math.round(rect.w), h:Math.round(rect.h) } }; }
      } else if (free) {
        drag.ghosts?.get(node.id)?.remove(); drag.ghosts?.delete(node.id); el.classList.remove('drag-source');
        if (!group) { const snapped = guides.snap(box, siblings, { x:0,y:0,...size }, null, constrained); box = snapped.rect; guides.show(box, siblings, snapped.lines); }
        fields.position = { x:box.x - origin.x, y:box.y - origin.y };
        Object.assign(el.style, { left:unit(fields.position.x), top:unit(fields.position.y), position:'absolute' });
      }
      drag.changes.set(node.id, fields);
    }
    if (group) guides.show(group.rect, group.siblings, group.lines);
  });
  function finish(event, cancel = false) {
    if (!drag) return;
    const finished = drag; drag = null; canvas.classList.remove('dragging'); finished.line?.remove(); guides.clear();
    if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    if (finished.moved) render(); else decorate();
    if (cancel || !finished.moved) return;
    const ops = [];
    if (finished.drop) {
      // Append first so insertion indices are computed after all dragged nodes
      // leave their old slots, including a multi-selection in the same parent.
      for (const { node } of finished.nodes) ops.push({ op:'move_node', id:node.id, parent:finished.drop.parent.id });
      if (!finished.drop.absolute) finished.nodes.forEach(({ node }, i) => ops.push({ op:'move_node', id:node.id, parent:finished.drop.parent.id, index:finished.drop.index + i }));
    }
    for (const [id, change] of finished.changes) {
      const fields = finished.drop && !finished.drop.absolute ? { position:'auto' } : change;
      if (Object.keys(fields).length) ops.push({ op:'update_node', id, fields, base_hash:api.state.hashes[id] });
    }
    if (ops.length) api.apply(() => ops);
  }
  function measureHover(event) {
    const nodes = selectedNodes(), boxes = nodes.map(n => rectOf(n.id)).filter(Boolean); if (!boxes.length) return;
    const x = Math.min(...boxes.map(r => r.x)), y = Math.min(...boxes.map(r => r.y));
    const rect = { x, y, w:Math.max(...boxes.map(r => r.x + r.w)) - x, h:Math.max(...boxes.map(r => r.y + r.h)) - y };
    const hover = document.elementsFromPoint(event.clientX, event.clientY).find(el => el.matches('#canvas .block') && !api.state.selected.has(el.dataset.id) && !el.closest('.drag-ghost'));
    const parents = nodes.map(n => parentOf(api.slide(), n.id)?.parent), parent = parents.every(p => p === parents[0]) ? parents[0] : api.slide(), target = hover && rectOf(hover.dataset.id);
    if (target) guides.measure(rect, target, hover.dataset.id === parent?.id); else if (parent) guides.measure(rect, parentRect(parent), true);
  }
  canvas.addEventListener('pointerleave', () => { if (!drag) guides.clear(); });
  document.addEventListener('keyup', event => { if (event.key === 'Alt' && !drag) guides.clear(); });
  document.addEventListener('keydown', event => { if (event.key === 'Escape' && drag) { event.preventDefault(); event.stopPropagation(); finish({ pointerId:drag.pointerId }, true); } }, true);
  canvas.addEventListener('lostpointercapture', event => finish(event, true));
  canvas.addEventListener('pointerup', event => finish(event));
  canvas.addEventListener('pointercancel', event => finish(event, true));
  canvas.addEventListener('dblclick', event => {
    if (api.state.preview || api.state.mode === 'comment') return;
    const element = event.target.closest('.block-text');
    if (!element || element.isContentEditable || locked(element.dataset.id)) return;
    const slideId = api.slide().id;
    const node = findNode(api.slide(), element.dataset.id);
    element.textContent = node.text;
    element.contentEditable = 'plaintext-only';
    element.classList.add('editing'); element.focus();
    element.addEventListener('paste', paste => {
      paste.preventDefault(); document.execCommand('insertText', false, paste.clipboardData.getData('text/plain'));
    });
    element.addEventListener('keydown', key => {
      if (key.key === 'Escape') { key.preventDefault(); key.stopPropagation(); element.blur(); }
    });
    element.addEventListener('blur', () => {
      const text = element.innerText;
      element.removeAttribute('contenteditable'); element.classList.remove('editing');
      if (text !== node.text) api.updateMany(slideId, [{ id: node.id, fields: { text } }]);
      else render();
    }, { once: true });
  });
  let menu;
  document.addEventListener('pointerdown', event => { if (menu && !menu.contains(event.target)) { menu.remove(); menu = null; } });
  document.addEventListener('keydown', event => { if (event.key === 'Escape') { menu?.remove(); menu = null; } });
  canvas.addEventListener('contextmenu', event => {
    if (api.state.preview || api.state.mode !== 'edit') return;
    event.preventDefault(); menu?.remove();
    const target = event.target.closest('.block');
    if (target && !locked(target.dataset.id) && !api.state.selected.has(target.dataset.id)) { api.state.selected = new Set([target.dataset.id]); api.selectionChanged(); }
    menu = element('div', 'canvas-menu popover');
    Object.assign(menu.style, { position:'fixed', left:`${Math.min(event.clientX, window.innerWidth - 250)}px`, top:`${Math.min(event.clientY, window.innerHeight - 180)}px` });
    for (const [title, action] of [['Add auto-layout · Shift+A', () => api.wrap()], ['Remove auto-layout · Alt+Shift+A', () => api.wrap(true)], ['Duplicate', api.duplicate], ['Delete', api.remove]]) {
      const control = element('button', '', title); control.onclick = () => { action(); menu.remove(); menu = null; }; menu.append(control);
    }
    document.body.append(menu);
  });
  function nudge(dx, dy) {
    if (api.state.preview) return;
    const slide = api.slide();
    const ids = new Set(selectedNodes().map(n => n.id));
    if (!slide || !ids.size) return;
    // Resolve relative edits when they leave the command queue, so fast key
    // repeats accumulate against the last authoritative deck, not a preview.
    api.apply(() => {
      const current = api.state.deck.slides.find(s => s.id === slide.id);
      const nodes = current ? allNodes(current).filter(n => ids.has(n.id) && !locked(n.id) && !hasSelectedAncestor(n.id) && isAbsolute(current, n)) : [];
      return nodes.map(n => {
        const at = typeof n.position === 'object' ? n.position : { x: 0, y: 0 };
        return { op: 'update_node', id: n.id, fields: { position: { x: at.x + dx, y: at.y + dy } }, base_hash: api.state.hashes[n.id] };
      });
    });
  }
  function remove() {
    if (api.state.preview) return;
    const slide = api.slide();
    // A selected frame already takes its children with it.
    const inside = nodeId => { for (let at = parentOf(slide, nodeId); at && at.parent !== slide; at = parentOf(slide, at.parent.id)) if (api.state.selected.has(at.parent.id)) return true; return false; };
    const ids = selectedNodes().map(n => n.id).filter(nodeId => !inside(nodeId));
    if (slide) api.apply(() => ids.map(id => ({ op: 'delete_node', id })));
  }
  function duplicate() {
    if (api.state.preview) return;
    const slide = api.slide();
    if (!slide) return;
    const copies = selectedNodes().map(node => {
      const copy = withNewIds(node, 'b'), at = parentOf(slide, node.id);
      if (typeof copy.position === 'object') copy.position = { x: copy.position.x + 24, y: copy.position.y + 24 };
      return { copy, parent: at.parent.id, index: at.index + 1 };
    });
    api.apply(() => copies.map(({ copy, parent, index }) => ({ op: 'insert_node', parent, index, node: copy })));
    api.state.selected = new Set(copies.map(c => c.copy.id));
  }
  return { render, decorate, nudge, remove, duplicate, rectOf };
}
