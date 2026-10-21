// Pure grid and Markdown helpers. No DOM, filesystem or network access.
export const snap = value => Math.round(value);
export function clampRect({ x, y, w, h }) {
  x = Math.max(0, Math.min(31, snap(x)));
  y = Math.max(0, Math.min(17, snap(y)));
  return { x, y, w: Math.max(1, Math.min(32 - x, snap(w))), h: Math.max(1, Math.min(18 - y, snap(h))) };
}
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
export const layouts = ['title', 'section', 'content', 'two-column', 'image-text', 'quote', 'blank'];
export function textBlock(text = 'Double-click to edit', rect = { x: 2, y: 3, w: 20, h: 4 }, size = 'l') {
  return { id: id('b'), kind: 'text', ...rect, text, style: { size, align: 'left' } };
}
export function layoutSlide(layout) {
  const slide = { id: id('s'), layout, notes: '', blocks: [] };
  if (layout === 'blank') return slide;
  if (layout === 'title' || layout === 'section') {
    slide.blocks.push(textBlock('Your headline', { x: 2, y: 6, w: 28, h: 5 }, 'xxl'));
  } else if (layout === 'quote') {
    slide.blocks.push(textBlock('*A thought worth sharing.*', { x: 4, y: 5, w: 24, h: 8 }, 'xl'));
  } else {
    slide.blocks.push(textBlock('Slide title', { x: 2, y: 1, w: 28, h: 3 }, 'xl'));
    slide.blocks.push(textBlock('- Start your story here', { x: 2, y: 5, w: layout === 'content' ? 28 : 13, h: 11 }));
    if (layout === 'two-column') slide.blocks.push(textBlock('Another perspective', { x: 17, y: 5, w: 13, h: 11 }));
    if (layout === 'image-text') slide.blocks.push({ id: id('b'), kind: 'shape', shape: 'rect', x: 17, y: 5, w: 13, h: 11, fill: 'muted', stroke: 'muted', style: {} });
  }
  return slide;
}
