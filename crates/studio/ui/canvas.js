import { snap, clampRect, id } from './helpers.js';
import { renderSlide, renderBlock, positionBlock, element } from './render.js';

export function canvasController(api) {
  const canvas = document.querySelector('#canvas');
  const selectedBlocks = () => api.slide()?.blocks.filter(b => api.state.selected.has(b.id)) || [];
  const nodeFor = blockId => [...canvas.querySelectorAll('.block')].find(n => n.dataset.id === blockId);
  function decorate() {
    canvas.querySelectorAll('.resize-handle').forEach(n => n.remove());
    if (api.state.preview || api.state.mode === 'comment') return;
    for (const node of canvas.querySelectorAll('.block')) node.classList.toggle('selected', api.state.selected.has(node.dataset.id));
    if (api.state.selected.size !== 1) return;
    const block = selectedBlocks()[0];
    if (!block) return;
    for (const direction of ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w']) {
      const handle = element('div', `resize-handle handle-${direction}`);
      handle.dataset.direction = direction; handle.dataset.target = block.id;
      const x = block.x + (direction.includes('w') ? 0 : direction.includes('e') ? block.w : block.w / 2);
      const y = block.y + (direction.includes('n') ? 0 : direction.includes('s') ? block.h : block.h / 2);
      handle.style.left = `${x / 32 * 100}%`; handle.style.top = `${y / 18 * 100}%`;
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
        for (const node of canvas.querySelectorAll('.block')) {
          const old = before?.blocks.find(b => b.id === node.dataset.id);
          const block = after?.blocks.find(b => b.id === node.dataset.id);
          if (JSON.stringify(old) !== JSON.stringify(block) || JSON.stringify(api.state.deck.theme) !== JSON.stringify(deck.theme)) node.classList.add('ghost-changed');
        }
        for (const block of before?.blocks || []) if (!after?.blocks.some(b => b.id === block.id)) {
          const node = renderBlock(block, api.state.deck.theme, api.state.images); node.classList.add('ghost-removed'); canvas.append(node);
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
      const bounds = canvas.getBoundingClientRect();
      const node = event.target.closest('.block');
      api.commentAt({ slide: api.slide().id, block: node?.dataset.id || null, x: Math.max(0, Math.min(31, Math.floor((event.clientX - bounds.left) / bounds.width * 32))), y: Math.max(0, Math.min(17, Math.floor((event.clientY - bounds.top) / bounds.height * 18))) });
      event.preventDefault(); return;
    }
    api.state.scopeSlide = false;
    const handle = event.target.closest('.resize-handle');
    const blockNode = event.target.closest('.block');
    const blockId = handle?.dataset.target || blockNode?.dataset.id;
    if (!blockId) { api.state.selected.clear(); api.selectionChanged(); return; }
    if (!handle) {
      if (event.shiftKey) {
        if (api.state.selected.has(blockId)) api.state.selected.delete(blockId);
        else api.state.selected.add(blockId);
      } else if (!api.state.selected.has(blockId)) api.state.selected = new Set([blockId]);
      api.selectionChanged();
    }
    if (!api.state.selected.has(blockId)) return;
    event.preventDefault(); canvas.focus();
    drag = { startX: event.clientX, startY: event.clientY, direction: handle?.dataset.direction, slide: api.slide().id,
      blocks: selectedBlocks().map(b => ({ ...b })), rects: new Map(), moved: false, bounds: canvas.getBoundingClientRect() };
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener('pointermove', event => {
    if (!drag) return;
    let dx = snap((event.clientX - drag.startX) / drag.bounds.width * 32);
    let dy = snap((event.clientY - drag.startY) / drag.bounds.height * 18);
    if (!drag.direction) {
      dx = Math.max(-Math.min(...drag.blocks.map(b => b.x)), Math.min(32 - Math.max(...drag.blocks.map(b => b.x + b.w)), dx));
      dy = Math.max(-Math.min(...drag.blocks.map(b => b.y)), Math.min(18 - Math.max(...drag.blocks.map(b => b.y + b.h)), dy));
    }
    drag.moved ||= dx !== 0 || dy !== 0;
    canvas.classList.toggle('dragging', drag.moved);
    canvas.querySelectorAll('.resize-handle').forEach(n => n.hidden = drag.moved);
    for (const block of drag.blocks) {
      let rect = { x: block.x + dx, y: block.y + dy, w: block.w, h: block.h };
      if (drag.direction) {
        let left = block.x, top = block.y, right = block.x + block.w, bottom = block.y + block.h;
        if (drag.direction.includes('w')) left = Math.max(0, Math.min(right - 1, left + dx));
        if (drag.direction.includes('e')) right = Math.max(left + 1, Math.min(32, right + dx));
        if (drag.direction.includes('n')) top = Math.max(0, Math.min(bottom - 1, top + dy));
        if (drag.direction.includes('s')) bottom = Math.max(top + 1, Math.min(18, bottom + dy));
        rect = { x: left, y: top, w: right - left, h: bottom - top };
      }
      rect = clampRect(rect); drag.rects.set(block.id, rect);
      positionBlock(nodeFor(block.id), rect);
    }
  });
  function finish(event, cancel = false) {
    if (!drag) return;
    const finished = drag; drag = null; canvas.classList.remove('dragging');
    if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    if (finished.moved) render(); else decorate();
    if (!cancel && finished.moved) api.updateMany(finished.slide, [...finished.rects].map(([blockId, fields]) => ({ id: blockId, fields })));
  }
  canvas.addEventListener('pointerup', event => finish(event));
  canvas.addEventListener('pointercancel', event => finish(event, true));
  canvas.addEventListener('dblclick', event => {
    if (api.state.preview || api.state.mode === 'comment') return;
    const node = event.target.closest('.block-text');
    if (!node || node.isContentEditable) return;
    const slideId = api.slide().id;
    const block = api.slide().blocks.find(b => b.id === node.dataset.id);
    node.textContent = block.text;
    node.contentEditable = 'plaintext-only';
    node.classList.add('editing'); node.focus();
    node.addEventListener('paste', paste => {
      paste.preventDefault(); document.execCommand('insertText', false, paste.clipboardData.getData('text/plain'));
    });
    node.addEventListener('keydown', key => {
      if (key.key === 'Escape') { key.preventDefault(); key.stopPropagation(); node.blur(); }
    });
    node.addEventListener('blur', () => {
      const text = node.innerText;
      node.removeAttribute('contenteditable'); node.classList.remove('editing');
      if (text !== block.text) api.updateMany(slideId, [{ id: block.id, fields: { text } }]);
      else render();
    }, { once: true });
  });
  function nudge(dx, dy) {
    if (api.state.preview) return;
    const slide = api.slide();
    const ids = new Set(selectedBlocks().map(b => b.id));
    if (!slide || !ids.size) return;
    // Resolve relative edits when they leave the command queue, so fast key
    // repeats accumulate against the last authoritative deck, not a preview.
    api.apply(() => {
      const blocks = api.state.deck.slides.find(s => s.id === slide.id)?.blocks.filter(b => ids.has(b.id)) || [];
      if (!blocks.length) return [];
      const moveX = Math.max(-Math.min(...blocks.map(b => b.x)), Math.min(32 - Math.max(...blocks.map(b => b.x + b.w)), dx));
      const moveY = Math.max(-Math.min(...blocks.map(b => b.y)), Math.min(18 - Math.max(...blocks.map(b => b.y + b.h)), dy));
      return blocks.map(b => ({ op: 'update_block', slide: slide.id, id: b.id, fields: { x: b.x + moveX, y: b.y + moveY }, base_hash: api.state.hashes[b.id] }));
    });
  }
  function remove() {
    if (api.state.preview) return;
    const slide = api.slide();
    const ids = selectedBlocks().map(b => b.id);
    if (slide) api.apply(() => ids.map(id => ({ op: 'delete_block', slide: slide.id, id })));
  }
  function duplicate() {
    if (api.state.preview) return;
    const slide = api.slide();
    if (!slide) return;
    const blocks = selectedBlocks().map(b => ({ ...structuredClone(b), id: id('b'), ...clampRect({ ...b, x: b.x + 1, y: b.y + 1 }) }));
    api.apply(() => blocks.map(block => ({ op: 'insert_block', slide: slide.id, block })));
    api.state.selected = new Set(blocks.map(b => b.id));
  }
  return { render, decorate, nudge, remove, duplicate };
}
