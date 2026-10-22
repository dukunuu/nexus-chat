import { allNodes, childrenOf, parentOf, isAbsolute, id, slideSize } from './helpers.js';
import { element } from './render.js';
import { button } from './icons.js';

export function editingController(api) {
  const selected = () => allNodes(api.slide()).filter(n => api.state.selected.has(n.id));
  const update = (node, fields) => ({ op:'update_node', id:node.id, fields, base_hash:api.state.hashes[node.id] });
  function wrap(unwrap = false) {
    const nodes = selected(), slide = api.slide();
    if (!nodes.length || api.state.preview) return;
    if (unwrap) {
      const frames = nodes.filter(n => n.kind === 'frame');
      api.state.selected = new Set(frames.flatMap(n => childrenOf(n).map(c => c.id)));
      api.apply(() => frames.map(n => ({ op:'unwrap', id:n.id }))); return;
    }
    const parent = parentOf(slide, nodes[0].id)?.parent;
    if (!nodes.every(n => parentOf(slide, n.id)?.parent === parent)) { api.toast('Select siblings to wrap in auto-layout.'); return; }
    const boxes = nodes.map(n => api.rectOf(n.id)).filter(Boolean);
    if (!boxes.length) return;
    const spread = axis => Math.max(...boxes.map(r => r[axis])) - Math.min(...boxes.map(r => r[axis]));
    const row = spread('x') > spread('y'), axis = row ? 'x' : 'y', size = row ? 'w' : 'h';
    boxes.sort((a, b) => a[axis] - b[axis]);
    const gaps = boxes.slice(1).map((r, i) => Math.max(0, r[axis] - boxes[i][axis] - boxes[i][size])).sort((a, b) => a - b);
    const middle = Math.floor(gaps.length / 2), gap = gaps.length ? (gaps[middle] + gaps[Math.max(0, Math.ceil(gaps.length / 2) - 1)]) / 2 : 0;
    const frameId = id('frame');
    api.state.selected = new Set([frameId]);
    api.apply(() => [{ op:'wrap', ids:nodes.map(n => n.id), id:frameId, layout:{ mode:row ? 'row' : 'column', gap:Math.round(gap) } }]);
  }
  function align(action) {
    const nodes = selected(), slide = api.slide(); if (!nodes.length || api.state.preview) return;
    const horizontal = ['left','h-center','right','distribute-h'].includes(action), axis = horizontal ? 'x' : 'y', size = horizontal ? 'w' : 'h';
    const flow = nodes.filter(n => !isAbsolute(slide, n));
    if (flow.length) {
      const parents = [...new Set(flow.map(n => parentOf(slide, n.id).parent))];
      api.apply(() => parents.map(parent => {
        const main = (parent.layout?.mode === 'row') === horizontal;
        const value = action.startsWith('distribute') ? 'space_between' : ['h-center','v-center'].includes(action) ? 'center' : ['right','bottom'].includes(action) ? 'end' : 'start';
        const layout = { [main ? 'main' : 'cross']:value === 'space_between' && !main ? 'stretch' : value };
        return parent === slide ? { op:'update_slide', id:parent.id, fields:{ layout } } : update(parent, { layout });
      })); return;
    }
    const items = nodes.map(node => ({ node, rect:api.rectOf(node.id), parent:parentOf(slide, node.id)?.parent })).filter(n => n.rect);
    if (!items.length) return;
    const distribute = action.startsWith('distribute'); if (distribute && items.length < 3) return;
    const bounds = items.length === 1 ? (items[0].parent === slide ? { x:0, y:0, ...slideSize(api.state.deck.theme) } : api.rectOf(items[0].parent.id)) : {
      [axis]:Math.min(...items.map(i => i.rect[axis])),
      [size]:Math.max(...items.map(i => i.rect[axis] + i.rect[size])) - Math.min(...items.map(i => i.rect[axis])),
    };
    if (!bounds) return;
    items.sort((a, b) => a.rect[axis] - b.rect[axis]);
    const gap = (bounds[size] - items.reduce((sum, i) => sum + i.rect[size], 0)) / Math.max(1, items.length - 1);
    let cursor = bounds[axis];
    const ops = items.map(({ node, rect, parent }) => {
      const origin = parent === slide ? { x:0, y:0 } : api.rectOf(parent.id);
      const value = distribute ? cursor : bounds[axis] + (['h-center','v-center'].includes(action) ? (bounds[size] - rect[size]) / 2 : ['right','bottom'].includes(action) ? bounds[size] - rect[size] : 0);
      cursor += rect[size] + gap;
      return update(node, { position:{ x:rect.x - origin.x, y:rect.y - origin.y, [axis]:Math.round((value - origin[axis]) * 100) / 100 } });
    });
    api.apply(() => ops);
  }
  function tidy() {
    const nodes = selected(), slide = api.slide();
    if (nodes.length < 3) return;
    const flowing = nodes.find(n => !isAbsolute(slide, n));
    if (flowing) { align(parentOf(slide, flowing.id).parent.layout?.mode === 'column' ? 'distribute-v' : 'distribute-h'); return; }
    const items = nodes.map(node => ({ node, r:api.rectOf(node.id) })).filter(i => i.r);
    const spread = a => Math.max(...items.map(i => i.r[a])) - Math.min(...items.map(i => i.r[a]));
    const row = spread('x') >= spread('y'), a = row ? 'x' : 'y', b = row ? 'y' : 'x', s = row ? 'w' : 'h';
    items.sort((i, j) => i.r[a] - j.r[a]);
    const gaps = items.slice(1).map((i, n) => i.r[a] - items[n].r[a] - items[n].r[s]).sort((x, y) => x - y);
    const gap = Math.max(0, gaps[Math.floor(gaps.length / 2)]), cross = Math.min(...items.map(i => i.r[b]));
    let cursor = items[0].r[a];
    const ops = items.map(({ node, r }) => {
      const p = parentOf(slide, node.id).parent, origin = p === slide ? { x:0, y:0 } : api.rectOf(p.id);
      const position = { [a]:cursor - origin[a], [b]:cross - origin[b] }; cursor += r[s] + gap;
      return update(node, { position });
    });
    api.apply(() => ops);
  }
  function alignRow(container) {
    const row = element('div', 'alignment-actions');
    for (const [action, label, shortcut] of [['left','Align left','A'],['h-center','Align horizontal centers','H'],['right','Align right','D'],['top','Align top','W'],['v-center','Align vertical centers','V'],['bottom','Align bottom','S'],['distribute-h','Distribute horizontal spacing',''],['distribute-v','Distribute vertical spacing','']]) {
      const control = button(`object-${action}`, `${label}${shortcut ? ` (Alt+${shortcut})` : ''}`, () => align(action));
      control.disabled = selected().length < (action.startsWith('distribute') ? 3 : 1); row.append(control);
    }
    container.append(row);
    const tidyButton = button('sparkle', 'Tidy up', tidy, 'tidy-action'); tidyButton.disabled = selected().length < 3; container.append(tidyButton);
  }
  return { wrap, align, tidy, alignRow };
}
