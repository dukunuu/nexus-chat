// Pure deck and Markdown helpers. No DOM, filesystem or network access.
// Deck format v2: slides are 1920 x 1080 px frames holding a tree of nodes;
// every length is a pixel.
export const SIZE = { w: 1920, h: 1080 };
export const slideSize = theme => theme?.size || SIZE;
export const snap = value => Math.round(value);
export function inlineTokens(text) {
  const pattern = /(\*\*([^*]+)\*\*|\*([^*]+)\*|`([^`]+)`|\[([^\]]+)\]\(([^\s)]+)\))/g;
  const tokens = [];
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > start) tokens.push({ kind: 'text', text: text.slice(start, match.index) });
    tokens.push(match[2] ? { kind: 'strong', text: match[2] } : match[3] ? { kind: 'em', text: match[3] } : match[4] ? { kind: 'code', text: match[4] } : { kind: 'link', text: match[5], href: match[6] });
    start = match.index + match[0].length;
  }
  if (start < text.length) tokens.push({ kind: 'text', text: text.slice(start) });
  return tokens;
}
export function markdownTokens(text) {
  return text.split('\n').map(line => {
    const heading = /^(#{1,3})\s+(.*)$/.exec(line);
    const bullet = /^\s*[-*]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+\.\s+(.*)$/.exec(line);
    return heading ? { kind: `h${heading[1].length}`, text: heading[2] }
      : bullet ? { kind: 'ul', text: bullet[1] }
      : numbered ? { kind: 'ol', text: numbered[1] } : { kind: 'p', text: line };
  });
}
let sequence = 0;
export const id = prefix => `${prefix}-${Date.now().toString(36)}-${++sequence}`;

// -- the node tree ---------------------------------------------------------------
export const childrenOf = node => (node?.kind === 'frame' || node?.children ? node.children || [] : []);
/** Every node of a slide, depth first. */
export function allNodes(slide) {
  const out = [];
  const walk = nodes => { for (const node of nodes) { out.push(node); walk(childrenOf(node)); } };
  walk(slide?.children || []);
  return out;
}
export const findNode = (slide, nodeId) => allNodes(slide).find(n => n.id === nodeId);
/** The slide or frame that holds a node, and the node's index in it. */
export function parentOf(slide, nodeId) {
  const search = (parent, nodes) => {
    const index = nodes.findIndex(n => n.id === nodeId);
    if (index >= 0) return { parent, index };
    for (const node of nodes) { const found = search(node, childrenOf(node)); if (found) return found; }
    return null;
  };
  return search(slide, slide?.children || []);
}
/** The layout mode of the container that holds a node: none, row or column. */
export const parentMode = (slide, nodeId) => parentOf(slide, nodeId)?.parent.layout?.mode || 'none';
/** A node with an explicit place: absolute, or the child of a free container. */
export const isAbsolute = (slide, node) => typeof node.position === 'object' || parentMode(slide, node.id) === 'none';
export function withNewIds(node, prefix = 'n') {
  const copy = structuredClone(node);
  const walk = n => { n.id = id(prefix); for (const child of childrenOf(n)) walk(child); };
  walk(copy);
  return copy;
}
export const nodeLabel = node => node.name || (node.kind === 'text' || node.kind === 'code' ? (node.text || node.source || '').replace(/[#*`]/g, '').split('\n')[0].slice(0, 28) : '') || node.kind;

// -- text styles -------------------------------------------------------------------
const HARD = { family:'body', size:42, weight:400, italic:false, line_height:1.35, letter_spacing:0, color:'fg', align:'left', valign:'top', transform:'none' };
/** A text style by name with a node's overrides on top, every field decided. */
export function resolveFont(theme, style, overrides = {}) {
  const base = theme.text_styles?.[style] || theme.text_styles?.body || {};
  return { ...HARD, ...base, ...Object.fromEntries(Object.entries(overrides || {}).filter(([, v]) => v !== null && v !== undefined)) };
}

// -- new slides and nodes ----------------------------------------------------------
export const layouts = ['title', 'section', 'content', 'two-column', 'image-text', 'quote', 'blank'];
export function textNode(text = 'Double-click to edit', extra = {}) {
  return { id: id('b'), kind: 'text', text, style: 'body', ...extra };
}
const frame = (children, layout, extra = {}) => ({ id: id('f'), kind: 'frame', layout, children, ...extra });
export function layoutSlide(layout) {
  const slide = { id: id('s'), layout: { mode: 'none' }, children: [], notes: '' };
  if (layout === 'blank') return slide;
  const page = { mode: 'column', gap: 56, padding: [120, 160, 120, 160], cross: 'stretch' };
  if (layout === 'title' || layout === 'section') {
    slide.layout = { mode: 'column', gap: 32, padding: [160, 160, 160, 160], main: 'center', cross: 'stretch' };
    slide.children.push(textNode('Your headline', { style: 'title' }));
  } else if (layout === 'quote') {
    slide.layout = { mode: 'column', padding: [240, 240, 240, 240], main: 'center', cross: 'stretch' };
    slide.children.push(textNode('*A thought worth sharing.*', { style: 'heading' }));
  } else {
    slide.layout = page;
    slide.children.push(textNode('Slide title', { style: 'heading' }));
    const body = textNode('- Start your story here', { sizing: { w: 'fill' } });
    if (layout === 'content') { body.sizing = { w: 'fill', h: 'fill' }; slide.children.push(body); }
    else {
      const second = layout === 'two-column' ? textNode('Another perspective', { sizing: { w: 'fill' } })
        : { id: id('b'), kind: 'shape', shape: 'rect', fill: 'muted', sizing: { w: 'fill', h: 'fill' } };
      slide.children.push(frame([body, second], { mode: 'row', gap: 96, cross: 'stretch' }, { sizing: { h: 'fill' } }));
    }
  }
  return slide;
}
