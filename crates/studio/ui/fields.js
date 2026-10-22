import { element } from './render.js';

// Keep accessible field names while using a short, in-field caption.
export function compact(row, caption) {
  const label = row.firstChild;
  label.title = `${label.textContent}${label.title ? ` · ${label.title}` : ''}`;
  label.replaceChildren(caption); return row;
}

// A deliberately small arithmetic grammar, never JavaScript evaluation.
export function arithmetic(source, base = 0) {
  let text = String(source).trim();
  if (/^[+\-*/]/.test(text)) text = `${base}${text}`;
  const tokens = text.match(/\d*\.?\d+(?:e[+-]?\d+)?|[()+\-*/]/gi) || [];
  if (tokens.join('') !== text.replace(/\s/g, '')) return null;
  let at = 0;
  const atom = () => {
    const token = tokens[at++];
    if (token === '+') return atom();
    if (token === '-') return -atom();
    if (token === '(') { const n = sum(); if (tokens[at++] !== ')') return NaN; return n; }
    return token && !/[()*/]/.test(token) ? Number(token) : NaN;
  };
  const product = () => { let n = atom(); while (['*', '/'].includes(tokens[at])) { const op = tokens[at++], rhs = atom(); n = op === '*' ? n * rhs : n / rhs; } return n; };
  const sum = () => { let n = product(); while (['+', '-'].includes(tokens[at])) { const op = tokens[at++], rhs = product(); n = op === '+' ? n + rhs : n - rhs; } return n; };
  const n = sum(); return at === tokens.length && Number.isFinite(n) ? Math.round(n * 1000) / 1000 : null;
}

export function fields(api) {
  return function field(label, value, change, options, type = 'text') {
    const row = element('label', 'property'), caption = element('span', '', label);
    row.append(caption);
    const input = element(options ? 'select' : 'input');
    input.setAttribute('aria-label', label);
    const values = Array.isArray(value) ? value : [value], first = values[0];
    const mixed = values.some(v => v !== first);
    if (options) {
      if (mixed) { const option = element('option', '', 'Mixed'); option.value = ''; input.append(option); }
      for (const [v, title] of options.map(o => Array.isArray(o) ? o : [o, o])) { const option = element('option', '', title); option.value = v; input.append(option); }
    } else input.type = type === 'number' ? 'text' : type;
    if (type === 'checkbox') { input.checked = Boolean(first); input.indeterminate = mixed; input.onchange = () => change(input.checked); }
    else {
      input.value = mixed ? '' : first ?? ''; input.placeholder = mixed ? 'Mixed' : '';
      input.onchange = () => {
        const n = type === 'number' ? arithmetic(input.value, Number(first) || 0) : input.value;
        if (n !== null) change(n); else input.value = first ?? '';
      };
    }
    if (type === 'number' && !options) {
      input.inputMode = 'decimal'; input.classList.add('numeric'); caption.classList.add('scrub-label');
      caption.title = 'Drag to scrub · Shift ×10 · Alt ×0.1';
      input.onkeydown = event => {
        if (!['ArrowUp', 'ArrowDown'].includes(event.key)) return;
        event.preventDefault(); event.stopPropagation();
        const n = arithmetic(input.value, Number(first) || 0) ?? Number(first) ?? 0;
        change(n + (event.key === 'ArrowUp' ? 1 : -1) * (event.shiftKey ? 10 : event.altKey ? .1 : 1));
      };
      caption.onpointerdown = event => {
        if (event.button !== 0 || input.disabled) return;
        event.preventDefault();
        const start = event.clientX, base = Number(first) || 0;
        let moved = false;
        api.beginGesture(); caption.setPointerCapture(event.pointerId);
        caption.onpointermove = move => {
          const dx = move.clientX - start; moved ||= Math.abs(dx) > 1;
          if (!moved) return;
          const n = Math.round((base + dx * (move.shiftKey ? 10 : move.altKey ? .1 : 1)) * 1000) / 1000;
          input.value = n; change(n);
        };
        const finish = cancel => {
          caption.onpointermove = null; caption.onpointerup = null; caption.onpointercancel = null;
          api.endGesture(cancel || !moved);
        };
        caption.onpointerup = () => finish(false); caption.onpointercancel = () => finish(true);
      };
    }
    row.append(input); return row;
  };
}
