// Images on the deck canvas: paste or drop one, pick one from disk, put one
// into a shape (the shape becomes a masked image in place), or use a shape to
// mask the image beneath it. Every change is one Apply, so one undo step.
import { id, parentOf, findNode, isAbsolute, slideSize } from './helpers.js';

const MAX_BYTES = 8 * 1024 * 1024;
const hex = bytes => { let out = ''; for (let i = 0; i < bytes.length; i += 8192) out += Array.from(bytes.subarray(i, i + 8192), b => b.toString(16).padStart(2, '0')).join(''); return out; };
const fillable = node => node?.kind === 'shape' && ['rect', 'ellipse', undefined].includes(node.shape);

/** The image mask a shape stands for: an ellipse is a circle when square. */
export function maskOf(shape, rect) {
  if (shape.shape === 'ellipse') return rect && Math.abs(rect.w - rect.h) < 1 ? { mask:'circle' } : { mask:'ellipse' };
  return shape.radius ? { radius:shape.radius } : {};
}

export function imagesController(api) {
  const state = api.state;
  const selected = () => { const slide = api.slide(); return slide ? [...state.selected].map(nodeId => findNode(slide, nodeId)).filter(Boolean) : []; };

  function upload(file) {
    if (!/^image\/(png|jpeg|webp)$/.test(file.type)) return Promise.reject(new Error('Paste a PNG, JPEG or WebP image.'));
    if (file.size > MAX_BYTES) return Promise.reject(new Error('That image is larger than 8 MB.'));
    return file.arrayBuffer().then(buffer => new Promise((resolve, reject) => {
      api.command({ command:'ImportAssetData', data:hex(new Uint8Array(buffer)) }, event => event.event === 'Asset' ? resolve(event.src) : reject(new Error(event.message || 'Could not import that image.')));
    }));
  }
  function pick() {
    return new Promise(resolve => api.command({ command:'PickImage' }, event => resolve(event.event === 'Asset' ? event.src : null)));
  }
  const ratioOf = file => (window.createImageBitmap ? createImageBitmap(file).then(b => { const r = b.width / b.height; b.close?.(); return r; }) : Promise.resolve(16 / 9)).catch(() => 16 / 9);

  /** Replace a shape with an image of the same box, masked to the shape. */
  function fillShape(shape, src) {
    const slide = api.slide(), at = parentOf(slide, shape.id); if (!at) return;
    const { kind:_kind, shape:_shape, fill:_fill, stroke:_stroke, radius:_radius, ...common } = shape;
    const node = { ...common, kind:'image', src, fit:'cover', ...maskOf(shape, api.rectOf(shape.id)) };
    state.selected = new Set([shape.id]);
    api.apply(() => [{ op:'delete_node', id:shape.id }, { op:'insert_node', parent:at.parent.id, index:at.index, node }]);
  }
  function replaceImage(image, src) {
    api.apply(() => [{ op:'update_node', id:image.id, fields:{ src }, base_hash:state.hashes[image.id] || '' }]);
  }
  function insertImage(src, ratio) {
    const slide = api.slide(); if (!slide) { api.toast('Add a slide first.'); return; }
    const size = slideSize(state.deck.theme), w = Math.round(Math.min(size.w * .6, size.h * .6 * ratio)), h = Math.round(w / ratio);
    const node = { id:id('b'), kind:'image', src, fit:'cover', position:{ x:Math.round((size.w - w) / 2), y:Math.round((size.h - h) / 2) }, sizing:{ w, h } };
    state.selected = new Set([node.id]); state.mode = 'edit';
    api.apply(() => [{ op:'insert_node', parent:slide.id, node }]);
  }

  /** The topmost image under a shape, drawn before it in the same parent. */
  function imageBelow(shape) {
    const slide = api.slide(), at = slide && parentOf(slide, shape.id); if (!at || !fillable(shape) || !isAbsolute(slide, shape)) return null;
    const box = api.rectOf(shape.id); if (!box) return null;
    for (let i = at.index - 1; i >= 0; i--) {
      const node = at.parent.children[i], rect = node.kind === 'image' && !node.hidden && isAbsolute(slide, node) && api.rectOf(node.id);
      if (rect && rect.x < box.x + box.w && box.x < rect.x + rect.w && rect.y < box.y + box.h && box.y < rect.y + rect.h) return node;
    }
    return null;
  }
  /** The image takes the shape's box and outline; the shape goes. */
  function maskBelow(shape) {
    const image = imageBelow(shape); if (!image) return;
    const { mask = null, radius = null } = maskOf(shape, api.rectOf(shape.id));
    state.selected = new Set([image.id]);
    api.apply(() => [
      { op:'update_node', id:image.id, fields:{ position:shape.position, sizing:shape.sizing, rotation:shape.rotation ?? null, fit:'cover', mask, radius }, base_hash:state.hashes[image.id] || '' },
      { op:'delete_node', id:shape.id },
    ]);
  }

  /** Put a file where it belongs: into the selected shape, over the selected
   *  image, or onto the slide. */
  async function place(file, target = selected().length === 1 ? selected()[0] : null) {
    try {
      const [src, ratio] = await Promise.all([upload(file), ratioOf(file)]);
      if (fillable(target)) fillShape(target, src);
      else if (target?.kind === 'image') replaceImage(target, src);
      else insertImage(src, ratio);
    } catch (error) { api.toast(error.message); }
  }
  async function pickInto(target) {
    const src = await pick(); if (!src) return;
    if (fillable(target)) fillShape(target, src); else if (target?.kind === 'image') replaceImage(target, src); else insertImage(src, 16 / 9);
  }
  /** "Paste image into shape" from a menu: the clipboard, when the webview lets us read it. */
  async function pasteInto(target) {
    try {
      for (const item of await navigator.clipboard.read()) {
        const type = item.types.find(t => /^image\/(png|jpeg|webp)$/.test(t));
        if (type) { const blob = await item.getType(type); return place(new File([blob], 'pasted image', { type }), target); }
      }
      api.toast('The clipboard has no image. Copy one, then paste it here.');
    } catch { api.toast('Press ⌘V (Ctrl+V) to paste an image into the selected shape.'); }
  }

  const typing = target => target?.closest?.('input,textarea,select,[contenteditable]');
  const deckMode = () => !document.body.classList.contains('doc-mode') && state.deck && state.mode === 'edit' && !state.preview;
  document.addEventListener('paste', event => {
    if (!deckMode() || typing(event.target)) return;
    const file = [...(event.clipboardData?.files || [])].find(f => f.type.startsWith('image/'));
    if (!file) return;
    event.preventDefault(); place(file);
  });
  const stage = document.querySelector('#stage');
  stage.addEventListener('dragover', event => { if (deckMode() && event.dataTransfer?.types.includes('Files')) { event.preventDefault(); event.dataTransfer.dropEffect = 'copy'; } });
  stage.addEventListener('drop', event => {
    const file = [...(event.dataTransfer?.files || [])].find(f => f.type.startsWith('image/'));
    if (!deckMode() || !file) return;
    event.preventDefault();
    const block = event.target.closest?.('#canvas .block'), slide = api.slide();
    place(file, block && slide ? findNode(slide, block.dataset.id) : null);
  });

  return { place, pickInto, pasteInto, fillShape, maskBelow, imageBelow, fillable };
}
