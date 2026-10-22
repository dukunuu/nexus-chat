import { allNodes, parentOf } from './helpers.js';

// Structural equality independent of Rust/JS object key insertion order.
export const signature = value => JSON.stringify(value, (_, v) => v && typeof v === 'object' && !Array.isArray(v) ? Object.fromEntries(Object.keys(v).sort().map(k => [k, v[k]])) : v);

/** Only assets used by this slide (and the shared imported fonts) affect it. */
export function slideAssets(slide, theme, images) {
  const keys = [...allNodes(slide).filter(n => n.kind === 'image').flatMap(n => [n.src, `/asset/${images[n.src]}`]), ...(theme.font_files || []).map(f => f.file)];
  return Object.fromEntries(keys.map(k => [k, images[k]]));
}

/** Pure, all-or-nothing preview. Unsupported operations stay backend-driven. */
export function applyLocal(deck, ops) {
  const next = structuredClone(deck);
  const slideFor = id => next.slides.find(s => s.id === id || allNodes(s).some(n => n.id === id));
  const target = id => next.slides.find(s => s.id === id) || next.slides.flatMap(allNodes).find(n => n.id === id);
  const merge = (before, patch) => {
    if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return structuredClone(patch);
    const result = before && typeof before === 'object' && !Array.isArray(before) ? before : {};
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) delete result[key]; else result[key] = merge(result[key], value);
    }
    return result;
  };
  for (const [i, op] of ops.entries()) {
    if (op.op === 'update_node' || op.op === 'update_slide') {
      const node = target(op.id); if (!node) return null;
      merge(node, op.fields);
    } else if (op.op === 'set_notes') {
      const slide = next.slides.find(s => s.id === op.slide); if (!slide) return null;
      slide.notes = op.notes;
    } else if (op.op === 'insert_node') {
      const parent = target(op.parent); if (!parent?.children || (op.index ?? parent.children.length) > parent.children.length) return null;
      parent.children.splice(op.index ?? parent.children.length, 0, structuredClone(op.node));
    } else if (op.op === 'delete_node' || op.op === 'move_node') {
      const slide = slideFor(op.id), at = slide && parentOf(slide, op.id); if (!at) return null;
      const node = at.parent.children[at.index];
      if (op.op === 'move_node') {
        const parent = target(op.parent); if (!parent?.children || parent === node || allNodes(node).includes(parent)) return null;
        const from = at.parent.layout?.mode || 'none', to = parent.layout?.mode || 'none';
        // Cross-container free moves need layout geometry. Direct drops supply
        // the final position in this transaction; other such moves fall back.
        const positioned = ops.slice(i + 1).some(o => o.op === 'update_node' && o.id === op.id && o.fields?.position != null);
        if (to === 'none' && (at.parent !== parent || typeof node.position !== 'object') && !positioned) return null;
        if (to !== 'none' && from === 'none' && typeof node.position === 'object') node.position = 'auto';
        at.parent.children.splice(at.index, 1);
        if ((op.index ?? parent.children.length) > parent.children.length) return null;
        parent.children.splice(op.index ?? parent.children.length, 0, node);
      } else at.parent.children.splice(at.index, 1);
    } else if (op.op === 'set_transition') {
      const slide = target(op.slide); if (!slide) return null;
      if (op.transition) slide.transition = structuredClone(op.transition); else delete slide.transition;
    } else if (op.op === 'set_animation') {
      const node = target(op.id); if (!node) return null;
      node.animation ||= {};
      if (op.build) node.animation[op.slot] = structuredClone(op.build); else delete node.animation[op.slot];
      if (!Object.keys(node.animation).length) delete node.animation;
    } else if (op.op === 'reorder_builds') {
      const slide = target(op.slide); if (!slide) return null;
      slide.builds = [...op.ids];
    } else return null;
  }
  return next;
}
