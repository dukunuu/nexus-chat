import { allNodes, findNode, parentOf, isAbsolute, withNewIds, slideSize } from './helpers.js';
import { renderSlide, renderNode, element } from './render.js';

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
  const percent = (value, total) => `${value / total * 100}%`;
  function decorate() {
    canvas.querySelectorAll('.resize-handle').forEach(n => n.remove());
    if (api.state.preview || api.state.mode === 'comment') return;
    for (const node of canvas.querySelectorAll('.block')) node.classList.toggle('selected', api.state.selected.has(node.dataset.id));
    if (api.state.selected.size !== 1) return;
    const node = selectedNodes()[0], rect = node && rectOf(node.id);
    if (!rect) return;
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
    if (!handle) {
      if (event.shiftKey) {
        if (api.state.selected.has(nodeId)) api.state.selected.delete(nodeId);
        else api.state.selected.add(nodeId);
      } else if (!api.state.selected.has(nodeId)) api.state.selected = new Set([nodeId]);
      api.selectionChanged();
    }
    if (!api.state.selected.has(nodeId)) return;
    event.preventDefault(); canvas.focus();
    const slide = api.slide(), nodes = selectedNodes();
    drag = { startX: event.clientX, startY: event.clientY, direction: handle?.dataset.direction, slide: slide.id,
      nodes: nodes.map(n => ({ node: n, rect: rectOf(n.id), free: isAbsolute(slide, n) })), changes: new Map(), moved: false, scale: slideScale(), told: false };
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener('pointermove', event => {
    if (!drag) return;
    const dx = Math.round((event.clientX - drag.startX) / drag.scale), dy = Math.round((event.clientY - drag.startY) / drag.scale);
    drag.moved ||= dx !== 0 || dy !== 0;
    canvas.classList.toggle('dragging', drag.moved);
    canvas.querySelectorAll('.resize-handle').forEach(n => n.hidden = drag.moved);
    const size = slideSize(api.state.deck.theme), unit = px => `${px / size.w * 100}cqw`;
    for (const { node, rect, free } of drag.nodes) {
      const element = nodeFor(node.id); if (!element) continue;
      const at = typeof node.position === 'object' ? node.position : { x: 0, y: 0 };
      if (!drag.direction) {
        if (!free) { if (drag.moved && !drag.told) { drag.told = true; api.toast('This layer follows auto-layout, so it cannot be dragged. Change its order in Layers, or set its position to absolute.'); } continue; }
        const position = { x: at.x + dx, y: at.y + dy };
        Object.assign(element.style, { left: unit(position.x), top: unit(position.y), position: 'absolute' });
        drag.changes.set(node.id, { position });
      } else {
        const west = drag.direction.includes('w'), east = drag.direction.includes('e'), north = drag.direction.includes('n'), south = drag.direction.includes('s');
        const w = Math.max(1, rect.w + (east ? dx : west ? -dx : 0)), h = Math.max(1, rect.h + (south ? dy : north ? -dy : 0));
        const sizing = {}, fields = {};
        if (east || west) { sizing.w = Math.round(w); element.style.width = unit(w); }
        if (north || south) { sizing.h = Math.round(h); element.style.height = unit(h); }
        fields.sizing = sizing;
        if (free && (west || north)) {
          const position = { x: west ? at.x + dx : at.x, y: north ? at.y + dy : at.y };
          if (west) element.style.left = unit(position.x);
          if (north) element.style.top = unit(position.y);
          fields.position = position;
        }
        drag.changes.set(node.id, fields);
      }
    }
  });
  function finish(event, cancel = false) {
    if (!drag) return;
    const finished = drag; drag = null; canvas.classList.remove('dragging');
    if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    if (finished.moved) render(); else decorate();
    if (!cancel && finished.changes.size) api.updateMany(finished.slide, [...finished.changes].map(([id, fields]) => ({ id, fields })));
  }
  canvas.addEventListener('pointerup', event => finish(event));
  canvas.addEventListener('pointercancel', event => finish(event, true));
  canvas.addEventListener('dblclick', event => {
    if (api.state.preview || api.state.mode === 'comment') return;
    const element = event.target.closest('.block-text');
    if (!element || element.isContentEditable) return;
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
  function nudge(dx, dy) {
    if (api.state.preview) return;
    const slide = api.slide();
    const ids = new Set(selectedNodes().map(n => n.id));
    if (!slide || !ids.size) return;
    // Resolve relative edits when they leave the command queue, so fast key
    // repeats accumulate against the last authoritative deck, not a preview.
    api.apply(() => {
      const current = api.state.deck.slides.find(s => s.id === slide.id);
      const nodes = current ? allNodes(current).filter(n => ids.has(n.id) && isAbsolute(current, n)) : [];
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
