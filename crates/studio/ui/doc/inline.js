// Inline Markdown <-> rich runs. Pure: no DOM, no network, no globals.
//
// A doc stores block text as the Markdown subset the model writes: **bold**,
// *italic*, ~~strike~~, `code`, [links](url) and [^footnotes]. The editor shows
// it rich and writes back exactly that subset. A "run" is a stretch of text
// with the same marks: { t, b?, i?, s?, c?, a?: href, f?: footnote id }.
//
// Offsets everywhere are UTF-16 code units of the plain text, which is what the
// DOM uses. `toCodePoints` and `fromCodePoints` convert to the character offsets
// comment anchors use (they count characters, as the Rust side does).

const MARK_KEYS = ['b', 'i', 's', 'c'];
const ASCII_PUNCT = /[!-/:-@[-`{-~]/;
const WHITESPACE = /\s/u;
// Unicode punctuation and symbols, for the left- and right-flanking rules.
const PUNCT = /[\p{P}\p{S}]/u;
const ENTITIES = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };

export const hasMarks = run => MARK_KEYS.some(k => run[k]) || run.a != null || run.f != null;
const markKey = run => `${run.b ? 1 : 0}${run.i ? 1 : 0}${run.s ? 1 : 0}${run.c ? 1 : 0}|${run.a ?? ''}|${run.f ?? ''}`;

/** Merge neighbours that share marks and drop empty runs. */
export function normalize(runs) {
  const out = [];
  for (const run of runs) {
    if (!run.t) continue;
    const last = out.at(-1);
    if (last && markKey(last) === markKey(run) && last.f == null) last.t += run.t;
    else out.push({ ...run });
  }
  return out;
}

export const plainText = runs => runs.map(r => r.t).join('');

// -- parsing ---------------------------------------------------------------------

const isWs = ch => ch === undefined || WHITESPACE.test(ch);
const isPunct = ch => ch !== undefined && PUNCT.test(ch);
const isAlnum = ch => ch !== undefined && /[\p{L}\p{N}]/u.test(ch);

/** The end of a code span that opens with `n` backticks at `from`, or -1. */
function findCodeEnd(src, from, n) {
  let i = from;
  while (i < src.length) {
    if (src[i] !== '`') { i++; continue; }
    let j = i;
    while (src[j] === '`') j++;
    if (j - i === n) return i;
    i = j;
  }
  return -1;
}

/** Index of the `]` that closes the bracket opened just before `from`, or -1. */
function findBracketEnd(src, from) {
  let depth = 1;
  for (let i = from; i < src.length; i++) {
    const ch = src[i];
    if (ch === '\\') { i++; continue; }
    if (ch === '`') {
      let j = i;
      while (src[j] === '`') j++;
      const end = findCodeEnd(src, j, j - i);
      if (end >= 0) i = end + (j - i) - 1; else i = j - 1;
      continue;
    }
    if (ch === '[') depth++;
    else if (ch === ']' && --depth === 0) return i;
  }
  return -1;
}

/** `(url "title")` after a link's text: the destination and the end index, or null. */
function parseDestination(src, from) {
  if (src[from] !== '(') return null;
  let i = from + 1;
  while (src[i] === ' ' || src[i] === '\n') i++;
  let href = '';
  if (src[i] === '<') {
    const end = src.indexOf('>', i);
    if (end < 0) return null;
    href = src.slice(i + 1, end); i = end + 1;
  } else {
    let depth = 0;
    for (; i < src.length; i++) {
      const ch = src[i];
      if (ch === '\\' && ASCII_PUNCT.test(src[i + 1] ?? '')) { href += src[++i]; continue; }
      if (ch === '(') depth++;
      else if (ch === ')') { if (depth === 0) break; depth--; }
      else if (WHITESPACE.test(ch)) break;
      href += ch;
    }
  }
  while (src[i] === ' ' || src[i] === '\n') i++;
  if (src[i] === '"' || src[i] === "'") {
    const quote = src[i];
    let j = i + 1;
    while (j < src.length && src[j] !== quote) j += src[j] === '\\' ? 2 : 1;
    if (j >= src.length) return null;
    i = j + 1;
    while (src[i] === ' ' || src[i] === '\n') i++;
  }
  return src[i] === ')' ? { href: decodeURIComponentSafe(href), end: i + 1 } : null;
}
const decodeURIComponentSafe = text => { try { return decodeURI(text); } catch { return text; } };

function tokenize(src) {
  const nodes = [];
  let text = '';
  const flush = () => { if (text) { nodes.push({ k: 'text', t: text }); text = ''; } };
  for (let i = 0; i < src.length;) {
    const ch = src[i];
    if (ch === '\\') {
      const next = src[i + 1];
      if (next === '\n') { flush(); nodes.push({ k: 'text', t: '\n' }); i += 2; }
      else if (next !== undefined && ASCII_PUNCT.test(next)) { text += next; i += 2; }
      else { text += ch; i++; }
    } else if (ch === '`') {
      let j = i;
      while (src[j] === '`') j++;
      const n = j - i, end = findCodeEnd(src, j, n);
      if (end < 0) { text += src.slice(i, j); i = j; continue; }
      let code = src.slice(j, end).replace(/\n/g, ' ');
      if (code.length > 2 && code.startsWith(' ') && code.endsWith(' ') && code.trim()) code = code.slice(1, -1);
      flush(); nodes.push({ k: 'code', t: code }); i = end + n;
    } else if (ch === '[') {
      const footnote = /^\[\^([^\s\]]+)\]/.exec(src.slice(i));
      if (footnote) { flush(); nodes.push({ k: 'fn', id: footnote[1] }); i += footnote[0].length; continue; }
      const end = findBracketEnd(src, i + 1), dest = end < 0 ? null : parseDestination(src, end + 1);
      if (dest) {
        flush();
        nodes.push({ k: 'link', href: dest.href, kids: parseInline(src.slice(i + 1, end)) });
        i = dest.end;
      } else { text += ch; i++; }
    } else if (ch === '*' || ch === '_' || ch === '~') {
      let j = i;
      while (src[j] === ch) j++;
      const n = j - i, before = src[i - 1], after = src[j];
      const left = !isWs(after) && (!isPunct(after) || isWs(before) || isPunct(before));
      const right = !isWs(before) && (!isPunct(before) || isWs(after) || isPunct(after));
      if (ch === '~' && n !== 2) { text += src.slice(i, j); i = j; continue; }
      const open = ch === '_' ? left && (!right || isPunct(before)) : left;
      const close = ch === '_' ? right && (!left || isPunct(after)) : right;
      flush(); nodes.push({ k: 'delim', ch, n, orig: n, open, close }); i = j;
    } else if (ch === '\n') {
      // Two spaces before the newline make a hard break; otherwise it is a soft
      // break, which reads as a space.
      const hard = / {2,}$/.test(text);
      if (hard) text = text.replace(/ +$/, '');
      text += hard ? '\n' : ' '; i++;
      while (src[i] === ' ') i++;
    } else if (ch === '&') {
      const entity = /^&(?:(amp|lt|gt|quot|apos|nbsp)|#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6}));/.exec(src.slice(i));
      if (entity) {
        const code = entity[1] ? null : entity[2] ? Number(entity[2]) : parseInt(entity[3], 16);
        text += entity[1] ? ENTITIES[entity[1]] : code > 0 && code < 0x110000 ? String.fromCodePoint(code) : '�';
        i += entity[0].length;
      } else { text += ch; i++; }
    } else { text += ch; i++; }
  }
  flush();
  return nodes;
}

/** CommonMark's emphasis algorithm over the delimiter nodes, for `*`, `_` and `~~`. */
function emphasis(nodes) {
  let c = 0;
  while (c < nodes.length) {
    const closer = nodes[c];
    if (closer.k !== 'delim' || !closer.close || !closer.n) { c++; continue; }
    let o = c - 1;
    for (; o >= 0; o--) {
      const opener = nodes[o];
      if (opener.k !== 'delim' || opener.ch !== closer.ch || !opener.open || !opener.n) continue;
      if (closer.ch === '~') break;
      const both = (opener.open && opener.close) || (closer.open && closer.close);
      if (both && (opener.orig + closer.orig) % 3 === 0 && !(opener.orig % 3 === 0 && closer.orig % 3 === 0)) continue;
      break;
    }
    if (o < 0) { c++; continue; }
    const opener = nodes[o];
    const use = closer.ch === '~' ? 2 : opener.n >= 2 && closer.n >= 2 ? 2 : 1;
    const mark = closer.ch === '~' ? 's' : use === 2 ? 'b' : 'i';
    opener.n -= use; closer.n -= use;
    nodes.splice(o + 1, c - o - 1, { k: 'group', mark, kids: nodes.slice(o + 1, c) });
    c = o + 2;
    if (!closer.n) nodes.splice(c, 1);
    if (!opener.n) { nodes.splice(o, 1); c--; }
  }
}

function flatten(nodes, marks, out) {
  for (const node of nodes) {
    if (node.k === 'text') out.push({ ...marks, t: node.t });
    else if (node.k === 'code') out.push({ ...marks, t: node.t, c: true });
    else if (node.k === 'fn') out.push({ ...marks, t: `[^${node.id}]`, f: node.id });
    else if (node.k === 'delim') out.push({ ...marks, t: node.ch.repeat(node.n) });
    else if (node.k === 'group') flatten(node.kids, { ...marks, [node.mark]: true }, out);
    else if (node.k === 'link') {
      for (const run of node.kids) out.push({ ...marks, ...run, a: node.href });
    }
  }
  return out;
}

/** Parse a block's Markdown source into runs. Anything unknown stays literal text. */
export function parseInline(src) {
  const nodes = tokenize(String(src ?? ''));
  emphasis(nodes);
  const runs = flatten(nodes, {}, []);
  return normalize(runs.map(run => {
    // A link keeps the marks of its text; a nested mark inside is merged.
    const { a, ...rest } = run;
    return a == null ? rest : { ...rest, a };
  }));
}

// -- serializing -----------------------------------------------------------------

const BLOCK_START = /^(#{1,6}(?=\s|$)|[-+*](?=\s|$)|>|\d{1,9}[.)](?=\s|$)|`{3,}|~{3,}|={2,}\s*$|-{3,}\s*$|_{3,}\s*$|\*{3,}\s*$)/;

function escapeText(text, previous, cell) {
  let out = '';
  for (let i = 0; i < text.length; i++) {
    const ch = text[i], before = text[i - 1] ?? previous, after = text[i + 1];
    if (ch === '\n') out += '\\\n';
    else if (ch === '_') out += isAlnum(before) && isAlnum(after) ? ch : '\\_';
    else if ('\\`*~[]<'.includes(ch)) out += `\\${ch}`;
    else if (ch === '&' && /^&(?:#\d+|#[xX][0-9a-fA-F]+|[a-zA-Z]\w*);/.test(text.slice(i))) out += '\\&';
    else if (ch === '|' && cell) out += '\\|';
    else out += ch;
  }
  return out;
}
/** Escape literal text for inline Markdown, keeping its whitespace (for building larger strings). */
export const escapeMarkdown = text => escapeText(text, undefined, false);
const codeSpan = (text, cell) => {
  const body = cell ? text.replace(/\|/g, '\\|') : text;
  const fence = '`'.repeat(Math.max(0, ...[...body.matchAll(/`+/g)].map(m => m[0].length)) + 1);
  const pad = body.startsWith('`') || body.endsWith('`') || (body.startsWith(' ') && body.endsWith(' ') && body.trim()) ? ' ' : '';
  return `${fence}${pad}${body}${pad}${fence}`;
};
const DELIMITERS = { b: '**', i: '*', s: '~~' };
const layerOn = (run, layer) => layer === 'a' ? run.a != null : Boolean(run[layer]);

/** The longest stretch of text, in characters, over which a mark is unbroken. */
function longestSpan(runs, layer) {
  let best = 0, current = 0;
  for (const run of runs) {
    current = layerOn(run, layer) ? current + run.t.length : 0;
    best = Math.max(best, current);
  }
  return best;
}

function wrap(inner, layer, run) {
  if (layer === 'a') return `[${inner}](${run.a.replace(/ /g, '%20').replace(/\(/g, '%28').replace(/\)/g, '%29')})`;
  const [, lead, core, trail] = /^(\s*)([\s\S]*?)(\s*)$/.exec(inner);
  if (!core) return inner;
  return `${lead}${DELIMITERS[layer]}${core}${DELIMITERS[layer]}${trail}`;
}

/**
 * Wrap the mark that spans the most text outermost, then the next inside it,
 * so `*a **b** c*` stays one italic span rather than three.
 */
function serializeLayer(runs, layers, ctx) {
  if (!layers.length) {
    return runs.map(run => {
      if (run.f != null) return `[^${run.f}]`;
      const text = run.c ? codeSpan(run.t, ctx.cell) : escapeText(run.t, ctx.previous, ctx.cell);
      ctx.previous = run.t.at(-1) ?? ctx.previous;
      return text;
    }).join('');
  }
  const layer = layers.includes('a') ? 'a' : layers.reduce((best, next) => longestSpan(runs, next) > longestSpan(runs, best) ? next : best);
  const rest = layers.filter(l => l !== layer);
  let out = '';
  for (let i = 0; i < runs.length;) {
    let j = i + 1;
    while (j < runs.length && layerOn(runs[j], layer) === layerOn(runs[i], layer) && (layer !== 'a' || runs[j].a === runs[i].a)) j++;
    const group = runs.slice(i, j);
    const inner = serializeLayer(group, rest, ctx);
    out += layerOn(group[0], layer) ? wrap(inner, layer, group[0]) : inner;
    i = j;
  }
  return out;
}

/**
 * Write runs back as the Markdown subset. Text that would read as block syntax
 * at the start of a line is escaped, so a paragraph that begins "# " stays one.
 */
export function serializeRuns(runs, { cell = false, trim = true } = {}) {
  const merged = normalize(runs);
  const ctx = { cell, previous: undefined };
  let out = serializeLayer(merged, ['a', 'b', 'i', 's'], ctx);
  out = out.split('\n').map(line => {
    const trimmed = trim ? line.replace(/^\s+/, '') : line;
    const match = BLOCK_START.exec(trimmed);
    if (!match) return trimmed;
    return /^\d/.test(trimmed) ? trimmed.replace(/^(\d+)([.)])/, '$1\\$2') : `\\${trimmed}`;
  }).join('\n');
  return trim ? out.replace(/\s+$/, '') : out;
}

// -- editing runs ----------------------------------------------------------------

/** Split so that `start` and `end` fall on run boundaries; returns the runs and the index range. */
function split(runs, start, end) {
  const out = [];
  let at = 0, from = -1, to = -1;
  for (const run of runs) {
    const next = at + run.t.length;
    const cuts = [start, end].filter(p => p > at && p < next).sort((a, b) => a - b);
    let pos = at;
    for (const cut of [...cuts, next]) {
      if (cut === pos) continue;
      const piece = { ...run, t: run.t.slice(pos - at, cut - at) };
      if (pos >= start && cut <= end && from < 0) from = out.length;
      out.push(piece);
      if (pos >= start && cut <= end) to = out.length;
      pos = cut;
    }
    at = next;
  }
  return { runs: out, from: from < 0 ? 0 : from, to: from < 0 ? 0 : to };
}

/** Whether every / some / no character of the range carries the mark. */
export function markState(runs, start, end, mark) {
  let all = true, any = false, at = 0;
  for (const run of runs) {
    const from = Math.max(start, at), to = Math.min(end, at + run.t.length);
    at += run.t.length;
    if (to <= from) continue;
    const on = mark === 'a' ? run.a != null : Boolean(run[mark]);
    all &&= on; any ||= on;
  }
  return end > start && any ? (all ? 'all' : 'some') : 'none';
}

/** Turn a mark on or off over a range. `value` is the href for links. */
export function setMark(runs, start, end, mark, value) {
  if (end <= start) return runs;
  const parts = split(runs, start, end);
  for (let k = parts.from; k < parts.to; k++) {
    const run = parts.runs[k];
    if (run.f != null) continue;
    if (mark === 'a') { if (value == null) delete run.a; else run.a = value; }
    else if (value) run[mark] = true;
    else delete run[mark];
  }
  return normalize(parts.runs);
}

/** Toggle a mark: off when the whole range has it, otherwise on. */
export function toggleMark(runs, start, end, mark, href) {
  const on = markState(runs, start, end, mark) !== 'all';
  return setMark(runs, start, end, mark, mark === 'a' ? (on ? href : null) : on);
}

export function sliceRuns(runs, start, end) {
  const parts = split(runs, start, end);
  return normalize(parts.runs.slice(parts.from, parts.to));
}

export function deleteRange(runs, start, end) {
  if (end <= start) return runs;
  return normalize([...sliceRuns(runs, 0, start), ...sliceRuns(runs, end, plainText(runs).length)]);
}

/** Insert text at an offset with the given marks (default: none). */
export function insertText(runs, at, text, marks = {}) {
  return normalize([...sliceRuns(runs, 0, at), { ...marks, t: text }, ...sliceRuns(runs, at, plainText(runs).length)]);
}

/** The marks of the character before `at`, for typing that continues a style. */
export function marksBefore(runs, at) {
  let pos = 0;
  for (const run of runs) {
    if (at > pos && at <= pos + run.t.length) { const { t, f, ...marks } = run; return marks; }
    pos += run.t.length;
  }
  return {};
}

// -- code points -----------------------------------------------------------------

/** UTF-16 offset to character (code point) offset. */
export const toCodePoints = (text, offset) => [...text.slice(0, offset)].length;
/** Character offset to UTF-16 offset. */
export const fromCodePoints = (text, count) => [...text].slice(0, count).join('').length;

// -- shortcuts typed as inline Markdown --------------------------------------------

const INLINE_SHORTCUTS = [
  { pattern: /(?:^|[^*\\])(\*\*)([^*\s](?:[^*]*[^*\s])?)(\*\*)$/, mark: 'b', size: 2 },
  { pattern: /(?:^|[^_\\])(__)([^_\s](?:[^_]*[^_\s])?)(__)$/, mark: 'b', size: 2 },
  { pattern: /(?:^|[^*\\])(\*)([^*\s](?:[^*]*[^*\s])?)(\*)$/, mark: 'i', size: 1 },
  { pattern: /(?:^|[^\w_\\])(_)([^_\s](?:[^_]*[^_\s])?)(_)$/, mark: 'i', size: 1 },
  { pattern: /(?:^|[^~\\])(~~)([^~\s](?:[^~]*[^~\s])?)(~~)$/, mark: 's', size: 2 },
  { pattern: /(?:^|[^`\\])(`)([^`]+)(`)$/, mark: 'c', size: 1 },
];

/**
 * After a delimiter is typed at `caret`, turn `**bold**`, `_italic_`,
 * `~~strike~~` and `` `code` `` that ends there into marks. Returns the new
 * runs and caret, or null when nothing matches.
 */
export function inlineShortcut(runs, caret) {
  const plain = plainText(runs), head = plain.slice(0, caret);
  for (const { pattern, mark, size } of INLINE_SHORTCUTS) {
    const match = pattern.exec(head);
    if (!match) continue;
    const inner = match[2], end = caret, start = end - inner.length - size * 2;
    // Delimiters typed inside a code span or link are text.
    if (markState(runs, start, end, 'c') === 'all' && mark !== 'c') continue;
    let next = deleteRange(runs, end - size, end);
    next = deleteRange(next, start, start + size);
    next = setMark(next, start, start + inner.length, mark, true);
    return { runs: next, caret: start + inner.length, mark };
  }
  return null;
}
