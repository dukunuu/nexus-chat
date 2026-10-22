import { element } from './render.js';

export const edges = (r, axis) => axis === 'x' ? [r.x, r.x + r.w / 2, r.x + r.w] : [r.y, r.y + r.h / 2, r.y + r.h];
export const overlaps = (a, b, axis) => axis === 'x' ? a.y < b.y + b.h && a.y + a.h > b.y : a.x < b.x + b.w && a.x + a.w > b.x;

export function guideController(canvas, scale) {
  let overlay;
  const labels = [];
  function clear() { overlay?.remove(); overlay = null; labels.length = 0; }
  function root() { if (!overlay) { overlay = element('div', 'smart-guides'); document.body.append(overlay); } return overlay; }
  function line(x1, y1, x2, y2, label, className = '') {
    const bounds = canvas.getBoundingClientRect(), s = scale();
    const x = bounds.x + Math.min(x1, x2) * s, y = bounds.y + Math.min(y1, y2) * s;
    const node = element('div', `guide-line ${y1 !== y2 ? 'vertical-guide ' : ''}${className}`);
    Object.assign(node.style, { left:`${x}px`, top:`${y}px`, width:`${Math.max(1, Math.abs(x2 - x1) * s)}px`, height:`${Math.max(1, Math.abs(y2 - y1) * s)}px` });
    if (label !== undefined) { node.classList.add('distance-line'); const text = element('span', 'distance-label', String(Math.round(label * 10) / 10)); node.append(text); }
    root().append(node);
    const pill = node.querySelector('.distance-label');
    if (pill) {
      let box = pill.getBoundingClientRect(), offset = 0;
      const vertical = y1 !== y2;
      while (labels.some(r => box.left < r.right + 4 && box.right + 4 > r.left && box.top < r.bottom + 4 && box.bottom + 4 > r.top)) {
        offset += vertical ? box.width + 4 : box.height + 4;
        pill.style[vertical ? 'marginLeft' : 'marginTop'] = `${vertical ? offset : -offset}px`; box = pill.getBoundingClientRect();
      }
      labels.push(box);
    }
  }
  function snap(rect, siblings, bounds, direction, constrain) {
    const result = { ...rect }, lines = [], threshold = 6 / scale();
    for (const axis of ['x','y']) {
      if (constrain && constrain !== axis) continue;
      const dimension = axis === 'x' ? 'w' : 'h', low = axis === 'x' ? 'w' : 'n', high = axis === 'x' ? 'e' : 's';
      if (direction && !direction.includes(low) && !direction.includes(high)) continue;
      const moving = direction ? [direction.includes(low) ? result[axis] : result[axis] + result[dimension]] : edges(result, axis);
      let best;
      for (const other of [...siblings, bounds]) for (const target of edges(other, axis)) for (const value of moving) {
        const delta = target - value;
        const cross = axis === 'x' ? 'y' : 'x', size = axis === 'x' ? 'h' : 'w';
        const distance = Math.abs(other[cross] + other[size] / 2 - result[cross] - result[size] / 2);
        if (Math.abs(delta) <= threshold && (!best || Math.abs(delta) < Math.abs(best.delta) || Math.abs(delta) === Math.abs(best.delta) && distance < best.distance)) best = { delta, target, axis, other, boundary:other === bounds, distance };
      }
      if (best) {
        if (!direction) result[axis] += best.delta;
        else if (direction.includes(low)) { result[axis] += best.delta; result[dimension] -= best.delta; }
        else result[dimension] += best.delta;
        lines.push(best);
      } else if (!direction) {
        // Match the moving gap to a gap already present between neighbours.
        const sorted = siblings.filter(r => overlaps(result, r, axis)).sort((a, b) => a[axis] - b[axis]);
        const gaps = sorted.slice(1).map((r, i) => r[axis] - sorted[i][axis] - sorted[i][dimension]).filter(g => g >= 0);
        let match;
        for (const other of sorted) for (const gap of gaps) for (const target of [other[axis] + other[dimension] + gap, other[axis] - gap - result[dimension]]) {
          const delta = target - result[axis];
          if (Math.abs(delta) <= threshold && (!match || Math.abs(delta) < Math.abs(match.delta))) match = { delta };
        }
        if (match) result[axis] += match.delta;
      }
    }
    return { rect:result, lines };
  }
  function distances(rect, neighbours, equal = true) {
    for (const axis of ['x','y']) {
      const dimension = axis === 'x' ? 'w' : 'h', otherAxis = axis === 'x' ? 'y' : 'x', otherSize = axis === 'x' ? 'h' : 'w';
      const near = neighbours.filter(r => overlaps(rect, r, axis)), segments = new Map();
      const before = near.filter(r => r[axis] + r[dimension] <= rect[axis] + .1).sort((a, b) => b[axis] + b[dimension] - a[axis] - a[dimension])[0];
      const after = near.filter(r => r[axis] >= rect[axis] + rect[dimension] - .1).sort((a, b) => a[axis] - b[axis])[0];
      const add = (a, b, matched = false) => {
        const start = a[axis] + a[dimension], end = b[axis], gap = end - start;
        if (gap <= .1) return;
        // The measured faces, not the bracket's display offset, identify a gap.
        const crossStart = Math.max(a[otherAxis], b[otherAxis]), crossEnd = Math.min(a[otherAxis] + a[otherSize], b[otherAxis] + b[otherSize]);
        const key = [start, end, crossStart, crossEnd].map(n => Math.round(n * 1000)).join(':');
        const previous = segments.get(key);
        segments.set(key, { start, end, gap, cross:(crossStart + crossEnd) / 2, matched:matched || previous?.matched });
      };
      if (before) add(before, rect);
      if (after) add(rect, after);
      if (equal) {
        const gaps = [...segments.values()].map(s => s.gap), sorted = [...near, rect].sort((a, b) => a[axis] - b[axis]);
        const matches = sorted.slice(1).map((b, i) => ({ a:sorted[i], b, gap:b[axis] - sorted[i][axis] - sorted[i][dimension] })).filter(({ a, b, gap }) => gap > .1 && overlaps(a, b, axis));
        for (const gap of gaps) {
          const equalGaps = matches.filter(m => Math.abs(m.gap - gap) < 1);
          if (equalGaps.length > 1) for (const { a, b } of equalGaps) add(a, b, true);
        }
      }
      for (const { start, end, gap, cross, matched } of segments.values()) {
        const cls = matched ? 'equal-spacing' : 'nearest-distance';
        if (axis === 'x') line(start, cross, end, cross, gap, cls); else line(cross, start, cross, end, gap, cls);
        overlay.lastChild.dataset.segment = `${axis}:${start}:${end}:${cross}`;
      }
    }
  }
  function show(rect, neighbours, lines = []) {
    clear();
    const overshoot = 6 / scale();
    for (const guide of lines) {
      const other = guide.boundary ? rect : guide.other;
      if (guide.axis === 'x') line(guide.target, Math.min(rect.y, other.y) - overshoot, guide.target, Math.max(rect.y + rect.h, other.y + other.h) + overshoot);
      else line(Math.min(rect.x, other.x) - overshoot, guide.target, Math.max(rect.x + rect.w, other.x + other.w) + overshoot, guide.target);
    }
    distances(rect, neighbours);
  }
  function measure(rect, target, parent = false) {
    clear();
    if (!parent) {
      if (rect.x + rect.w <= target.x) line(rect.x + rect.w, rect.y + rect.h / 2, target.x, rect.y + rect.h / 2, target.x - rect.x - rect.w);
      else if (target.x + target.w <= rect.x) line(target.x + target.w, rect.y + rect.h / 2, rect.x, rect.y + rect.h / 2, rect.x - target.x - target.w);
      if (rect.y + rect.h <= target.y) line(rect.x + rect.w / 2, rect.y + rect.h, rect.x + rect.w / 2, target.y, target.y - rect.y - rect.h);
      else if (target.y + target.h <= rect.y) line(rect.x + rect.w / 2, target.y + target.h, rect.x + rect.w / 2, rect.y, rect.y - target.y - target.h);
      if (overlaps(rect, target, 'x') && overlaps(rect, target, 'y')) {
        line(rect.x + rect.w / 2, rect.y, rect.x + rect.w / 2, target.y, Math.abs(rect.y - target.y));
        line(rect.x, rect.y + rect.h / 2, target.x, rect.y + rect.h / 2, Math.abs(rect.x - target.x));
      }
      return;
    }
    const x = rect.x + rect.w / 2, y = rect.y + rect.h / 2;
    line(target.x, y, rect.x, y, rect.x - target.x);
    line(rect.x + rect.w, y, target.x + target.w, y, target.x + target.w - rect.x - rect.w);
    line(x, target.y, x, rect.y, rect.y - target.y);
    line(x, rect.y + rect.h, x, target.y + target.h, target.y + target.h - rect.y - rect.h);
  }
  return { snap, show, clear, measure, line };
}
