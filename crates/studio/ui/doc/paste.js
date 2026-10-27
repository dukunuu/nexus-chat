// Clipboard HTML to the Markdown subset the doc stores. DOMParser builds an inert
// document (no scripts run, nothing loads), and only text is carried across.
import { escapeMarkdown } from './inline.js';

const BLOCK_SYNTAX = /^(#{1,6} |[-*+] |\d+[.)] |> |```|\|.*\||-{3,}$)/m;
export const looksLikeMarkdown = text => BLOCK_SYNTAX.test(text);

const esc = escapeMarkdown;
const SAFE_LINK = /^(https?:|mailto:)/i;

function inline(node) {
  let out = '';
  for (const child of node.childNodes) {
    if (child.nodeType === Node.TEXT_NODE) { out += esc(child.data.replace(/\s+/g, ' ')); continue; }
    if (child.nodeType !== Node.ELEMENT_NODE) continue;
    const tag = child.tagName.toLowerCase();
    const inner = () => inline(child);
    const wrap = mark => { const text = inner(); const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(text); return m[2] ? `${m[1]}${mark}${m[2]}${mark}${m[3]}` : text; };
    if (tag === 'br') out += '\\\n';
    else if (tag === 'strong' || tag === 'b') out += child.style.fontWeight === 'normal' ? inner() : wrap('**');
    else if (tag === 'em' || tag === 'i') out += wrap('*');
    else if (tag === 's' || tag === 'del' || tag === 'strike') out += wrap('~~');
    else if (tag === 'code') out += `\`${child.textContent.replace(/`/g, '')}\``;
    else if (tag === 'a' && SAFE_LINK.test(child.getAttribute('href') || '')) out += `[${inner()}](${child.getAttribute('href').replace(/ /g, '%20').replace(/\)/g, '%29')})`;
    else if (['script', 'style', 'meta', 'title', 'img', 'svg'].includes(tag)) continue;
    else out += inner();
  }
  return out;
}

function list(node, depth) {
  const ordered = node.tagName.toLowerCase() === 'ol';
  const lines = [];
  let n = 1;
  for (const li of node.children) {
    if (li.tagName.toLowerCase() !== 'li') continue;
    const own = document.createElement('div');
    const nested = [];
    for (const child of li.childNodes) {
      if (child.nodeType === Node.ELEMENT_NODE && /^(ul|ol)$/i.test(child.tagName)) nested.push(child); else own.append(child.cloneNode(true));
    }
    const checkbox = li.querySelector(':scope > input[type=checkbox]');
    const task = checkbox ? (checkbox.checked ? '[x] ' : '[ ] ') : '';
    lines.push(`${'  '.repeat(depth)}${ordered ? `${n++}.` : '-'} ${task}${inline(own).trim()}`);
    for (const inner of nested) lines.push(list(inner, depth + 1));
  }
  return lines.join('\n');
}

function table(node) {
  const rows = [...node.querySelectorAll('tr')].map(tr => [...tr.children].map(cell => inline(cell).trim().replace(/\|/g, '\\|').replace(/\n/g, ' ')));
  if (!rows.length) return '';
  const columns = Math.max(...rows.map(r => r.length));
  const pad = row => Array.from({ length: columns }, (_, i) => row[i] ?? '');
  const line = row => `| ${pad(row).join(' | ')} |`;
  return [line(rows[0]), line(Array(columns).fill('---')), ...rows.slice(1).map(line)].join('\n');
}

function blocks(node) {
  const out = [];
  let run = '';
  const flush = () => { if (run.trim()) out.push(run.trim()); run = ''; };
  for (const child of node.childNodes) {
    if (child.nodeType === Node.TEXT_NODE) { run += esc(child.data.replace(/\s+/g, ' ')); continue; }
    if (child.nodeType !== Node.ELEMENT_NODE) continue;
    const tag = child.tagName.toLowerCase();
    if (/^h[1-6]$/.test(tag)) { flush(); out.push(`${'#'.repeat(Math.min(3, Number(tag[1])))} ${inline(child).trim()}`); }
    else if (tag === 'p') { flush(); out.push(inline(child).trim()); }
    else if (tag === 'ul' || tag === 'ol') { flush(); out.push(list(child, 0)); }
    else if (tag === 'blockquote') { flush(); out.push(blocks(child).join('\n\n').split('\n').map(l => `> ${l}`).join('\n')); }
    else if (tag === 'pre') { flush(); out.push(`\`\`\`\n${child.textContent.replace(/\n$/, '')}\n\`\`\``); }
    else if (tag === 'hr') { flush(); out.push('---'); }
    else if (tag === 'table') { flush(); out.push(table(child)); }
    else if (['div', 'section', 'article', 'body', 'main', 'figure', 'li'].includes(tag)) { flush(); out.push(...blocks(child)); }
    else if (['script', 'style', 'meta', 'title', 'head'].includes(tag)) continue;
    else run += inline({ childNodes: [child] });
  }
  flush();
  return out.filter(Boolean);
}

/** Markdown for clipboard HTML, or null when there is no usable markup. */
export function htmlToMarkdown(html) {
  if (!html || !/<(p|h[1-6]|ul|ol|li|table|blockquote|pre|strong|b|em|i|a|code|br|hr|div)\b/i.test(html)) return null;
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const parts = blocks(doc.body);
  return parts.length ? parts.join('\n\n') : null;
}
