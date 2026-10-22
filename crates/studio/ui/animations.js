import { allNodes } from './helpers.js';

export const slots = ['build_in', 'emphasis', 'build_out'];
export const easings = ['linear','ease','ease_in','ease_out','ease_in_out','spring'];
export const effects = ['appear','fade','rise','fly','scale','wipe','blur','typewriter','pulse','shake'];
export const timing = build => ({ duration:build.duration_ms ?? 400, delay:build.delay_ms ?? 0, easing:({ ease_in:'ease-in', ease_out:'ease-out', ease_in_out:'ease-in-out', spring:'cubic-bezier(.2,.9,.3,1.25)' })[build.easing || 'ease_out'] || build.easing || 'ease-out' });
export const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Explicit node order, then missing nodes in tree order; slots use lifecycle order. */
export function buildPlan(slide) {
  const nodes = allNodes(slide), byId = new Map(nodes.map(n => [n.id,n]));
  const ids = [...new Set([...(slide?.builds || []), ...nodes.filter(n => slots.some(s => n.animation?.[s])).map(n => n.id)])];
  const entries = ids.flatMap(id => slots.filter(slot => byId.get(id)?.animation?.[slot]).map(slot => ({ id, slot, build:byId.get(id).animation[slot] })));
  const automatic = [], groups = []; let group = automatic;
  for (const entry of entries) { if ((entry.build.trigger || 'click') === 'click') { group = []; groups.push(group); } group.push(entry); }
  return { entries, automatic, groups };
}

/** Deterministic click cursor. Moving backwards and jumping never animate. */
export function stepEngine(slides, index = 0, step = 0) {
  const cursor = { index, step };
  const total = () => buildPlan(slides[cursor.index]).groups.length;
  const jump = (index, step = 0) => { cursor.index = Math.max(0, Math.min(slides.length - 1, index)); cursor.step = Math.max(0, Math.min(total(), step)); return cursor; };
  return {
    cursor, total, jump,
    next() { if (cursor.step < total()) return { group:buildPlan(slides[cursor.index]).groups[cursor.step++] }; if (cursor.index < slides.length - 1) { jump(cursor.index + 1); return { transition:true }; } return {}; },
    back() { if (cursor.step) cursor.step--; else if (cursor.index) { cursor.index--; cursor.step = total(); } return cursor; },
  };
}

/** Match ids first, then nonempty names, one-to-one and in tree order. */
export function smartMatches(before, after) {
  const old = allNodes(before), used = new Set(), matches = [];
  for (const node of allNodes(after)) {
    const match = old.find(n => !used.has(n) && n.id === node.id) || (node.name && old.find(n => !used.has(n) && n.name === node.name));
    if (match) { used.add(match); matches.push([match,node]); }
  }
  return matches;
}

const nodeElement = (root, id) => [...root.querySelectorAll('.block')].find(n => n.dataset.id === id);
const finalOpacity = el => Number(el.dataset.playOpacity ?? getComputedStyle(el).opacity);
const finalTransform = el => el.dataset.playTransform ?? (el.style.transform || 'none');

/** Snapshot base styles once; split only the playback DOM, preserving Markdown. */
export function preparePlayback(root, slide) {
  for (const node of allNodes(slide)) {
    const el = nodeElement(root,node.id); if (!el) continue;
    el.dataset.playOpacity = getComputedStyle(el).opacity; el.dataset.playTransform = el.style.transform || 'none';
    const builds = slots.map(s => node.animation?.[s]).filter(Boolean);
    if (node.kind === 'text' && builds.some(b => b.by && b.by !== 'whole')) splitWords(el);
  }
}
function splitWords(el) {
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT), texts = [];
  while (walker.nextNode()) texts.push(walker.currentNode);
  for (const text of texts) {
    const fragment = document.createDocumentFragment();
    for (const word of text.textContent.split(/(\s+)/)) {
      if (!word) continue;
      if (/^\s+$/.test(word)) fragment.append(document.createTextNode(word));
      else { const span = document.createElement('span'); span.className = 'build-word'; span.textContent = word; fragment.append(span); }
    }
    text.replaceWith(fragment);
  }
  // Read all line positions together before adding grouping metadata.
  const words = [...el.querySelectorAll('.build-word')], tops = words.map(w => Math.round(w.getBoundingClientRect().top));
  const lines = [...new Set(tops)]; words.forEach((w,i) => w.dataset.line = lines.indexOf(tops[i]));
}
function segments(el, build) {
  if (build.by === 'item') return [...el.querySelectorAll('li')];
  if (build.by === 'word') return [...el.querySelectorAll('.build-word')];
  if (build.by === 'line') {
    const lines = new Map();
    for (const word of el.querySelectorAll('.build-word')) { if (!lines.has(word.dataset.line)) lines.set(word.dataset.line,[]); lines.get(word.dataset.line).push(word); }
    return [...lines.values()];
  }
  return [el];
}
const segmentCount = (el, build) => Math.max(1, segments(el,build).length);

/** Instant reconstruction, including completed exits, without mutating deck data. */
export function applyStep(root, slide, step) {
  const plan = buildPlan(slide), done = [...plan.automatic, ...plan.groups.slice(0,step).flat()];
  const visible = new Map(allNodes(slide).map(n => [n.id,!n.animation?.build_in]));
  for (const entry of done) if (entry.slot !== 'emphasis') visible.set(entry.id,entry.slot === 'build_in');
  for (const [id, show] of visible) { const el = nodeElement(root,id); if (el) el.style.opacity = show ? String(finalOpacity(el)) : '0'; }
}

function frames(el, build, slot, reduce) {
  const opacity = finalOpacity(el), transform = finalTransform(el), base = transform === 'none' ? '' : ` ${transform}`;
  const end = { opacity, transform };
  if (slot === 'emphasis') {
    // Reduced motion keeps emphasis as a short dip in opacity: nothing moves.
    if (reduce) return [end, { ...end, opacity:opacity * .55 }, end];
    if (build.effect === 'shake') return [end, ...[-8, 8, -5, 5, 0].map(x => ({ ...end, transform:`translateX(${x}px)${base}` }))];
    return [end, { ...end, transform:`scale(1.08)${base}` }, end];
  }
  // A direction names the side an effect comes from (fly, rise) or starts at (wipe).
  const effect = reduce ? 'fade' : build.effect || 'fade', side = build.direction || (effect === 'rise' ? 'down' : 'left'), start = { ...end, opacity:0 };
  const far = { left:'-100%,0', right:'100%,0', up:'0,-100%', down:'0,100%' }[side], near = { left:'-40px,0', right:'40px,0', up:'0,-40px', down:'0,40px' }[side];
  if (effect === 'rise') start.transform = `translate(${near})${base}`;
  if (effect === 'fly') start.transform = `translate(${far})${base}`;
  if (effect === 'scale') start.transform = `scale(.65)${base}`;
  if (effect === 'wipe' || effect === 'typewriter') { start.clipPath = ({ left:'inset(0 100% 0 0)', right:'inset(0 0 0 100%)', up:'inset(0 0 100% 0)', down:'inset(100% 0 0 0)' })[side]; end.clipPath = 'inset(0)'; start.opacity = opacity; }
  if (effect === 'blur') { start.filter = 'blur(16px)'; end.filter = 'blur(0px)'; }
  return slot === 'build_out' ? [end, start] : [start, end];
}

/** Schedule a whole click group. With-previous shares starts, after-previous shares ends. */
export function playGroup(root, entries, reduce = reduced()) {
  const animations = []; let previousStart = 0, previousEnd = 0;
  for (const { id, slot, build } of entries) {
    const el = nodeElement(root,id); if (!el) continue;
    const t = timing(build), duration = (build.effect === 'appear' && !reduce ? 0 : t.duration) * (reduce ? .5 : 1);
    const start = ((build.trigger || 'click') === 'after_previous' ? previousEnd : (build.trigger === 'with_previous' ? previousStart : 0)) + t.delay;
    const parts = segments(el,build), stagger = (build.stagger_ms ?? 60) * (reduce ? .5 : 1);
    el.style.opacity = String(finalOpacity(el));
    (parts.length ? parts : [el]).forEach((part,i) => {
      for (const target of Array.isArray(part) ? part : [part]) {
        const keyframes = frames(target,build,slot,reduce);
        const animation = target.animate(keyframes,{ duration,delay:start + i * stagger,easing:!reduce && build.effect === 'typewriter' ? `steps(${Math.max(1,target.textContent.length)},end)` : t.easing,fill:slot === 'build_in' ? 'both' : 'forwards' });
        animations.push(animation);
      }
    });
    previousStart = start; previousEnd = start + duration + (segmentCount(el,build)-1) * stagger;
  }
  return animations;
}

/** FLIP reads both rendered trees before any animation writes. Color/fill/font
 * differences crossfade; text never inherits an anisotropic scale. */
export function transitionSlides(oldRoot, newRoot, before, after, transition = {}, reduce = reduced()) {
  const t = timing(transition), duration = t.duration * (reduce ? .5 : 1), animations = [];
  const run = (el, frames) => { const a = el.animate(frames,{ duration,easing:t.easing,fill:'both' }); animations.push(a); };
  const fade = () => { run(oldRoot,[{opacity:1},{opacity:0}]); run(newRoot,[{opacity:0},{opacity:1}]); };
  const type = reduce ? 'fade' : transition.type || 'none';
  if (type === 'none') { oldRoot.style.opacity = '0'; return animations; }
  if (type === 'smart') {
    // Measure everything before writing anything: no layout thrash.
    const geometry = el => { const r = el.getBoundingClientRect(); return { left:r.left, top:r.top, cx:r.left + r.width / 2, cy:r.top + r.height / 2, w:Math.max(1, el.offsetWidth), h:Math.max(1, el.offsetHeight) }; };
    const look = el => { const c = getComputedStyle(el); return [el.textContent, c.backgroundColor, c.color, c.fontSize, c.fontWeight, c.borderTopColor, c.borderTopWidth].join('|'); };
    const pairs = smartMatches(before,after).map(([a,b]) => {
      const old = nodeElement(oldRoot,a.id), next = nodeElement(newRoot,b.id);
      return old && next ? { a,b,old,next,from:geometry(old),to:geometry(next),same:look(old) === look(next) } : null;
    }).filter(Boolean);
    const usedOld = new Set(pairs.map(p => p.old)), usedNew = new Set(pairs.map(p => p.next));
    // The incoming background fades in as its own layer, so the outgoing slide shows through.
    const fill = getComputedStyle(newRoot).backgroundColor, layer = document.createElement('div');
    layer.style.cssText = `position:absolute;inset:0;background:${fill}`; newRoot.prepend(layer);
    newRoot.style.backgroundColor = 'transparent'; for (const child of newRoot.children) if (child !== layer) child.style.backgroundColor = 'transparent';
    run(layer,[{opacity:0},{opacity:1}]);
    // Children of matched frames are independent layers: frames crossfade in
    // place, so transforms are never compounded down the tree.
    for (const { a,b,old,next,from,to,same } of pairs) {
      const opacity = finalOpacity(next), start = same ? finalOpacity(old) : 0;
      if (b.kind === 'frame') { run(old,[{opacity:finalOpacity(old)},{opacity:0}]); run(next,[{opacity:start},{opacity}]); continue; }
      // Text is moved, never scaled (a stretched glyph is a lie); shapes and images morph size too.
      const flowing = ['text','code','table'].includes(b.kind), rotate = n => `rotate(${n.rotation || 0}deg)`;
      const dx = flowing ? from.left - to.left : from.cx - to.cx, dy = flowing ? from.top - to.top : from.cy - to.cy;
      const scale = flowing ? '' : ` scale(${from.w / to.w},${from.h / to.h})`, rest = flowing ? '' : ' scale(1,1)';
      run(next,[{transform:`translate(${dx}px,${dy}px) ${rotate(a)}${scale}`,opacity:start},{transform:`translate(0px,0px) ${rotate(b)}${rest}`,opacity}]);
      if (same) old.style.opacity = '0';
      else run(old,[{transform:old.style.transform || 'none',opacity:finalOpacity(old)},{transform:`translate(${-dx}px,${-dy}px) ${rotate(b)}`,opacity:0}]);
    }
    for (const el of oldRoot.querySelectorAll('.block')) if (!usedOld.has(el)) run(el,[{opacity:finalOpacity(el)},{opacity:0}]);
    for (const el of newRoot.querySelectorAll('.block')) if (!usedNew.has(el)) run(el,[{opacity:0},{opacity:finalOpacity(el)}]);
    return animations;
  }
  if (type === 'fade') fade();
  else if (type === 'zoom') { run(oldRoot,[{transform:'scale(1)',opacity:1},{transform:'scale(1.15)',opacity:0}]); run(newRoot,[{transform:'scale(.85)',opacity:0},{transform:'scale(1)',opacity:1}]); }
  else {
    const [x,y] = ({ left:[-100,0],right:[100,0],up:[0,-100],down:[0,100] })[transition.direction || 'left'];
    const enter = `translate(${-x}%,${-y}%)`, exit = `translate(${x}%,${y}%)`;
    if (type === 'slide') { newRoot.style.zIndex = '0'; oldRoot.style.zIndex = '1'; run(oldRoot,[{transform:'none'},{transform:exit}]); }
    else { run(newRoot,[{transform:enter},{transform:'none'}]); if (type === 'push') run(oldRoot,[{transform:'none'},{transform:exit}]); }
  }
  return animations;
}

export function finishAnimations(animations) { for (const animation of animations) { try { animation.finish(); } catch { animation.cancel(); } } }
export function cancelAnimations(animations) { for (const animation of animations) animation.cancel(); }
