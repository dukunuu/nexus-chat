// Run: node crates/studio/ui-dev/doc-unit.mjs [--parity]
// Pure-function tests for the doc editor's modules (no DOM, no browser): the inline
// Markdown round trip, the block shortcuts, local operations and the structural edits.
// `--parity` prints {md, plain} pairs as JSON so a Rust test can check the same strings
// against the core's Markdown parser. Needs Node 22+ (it imports the UI modules directly).
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
const ui = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../ui/doc');
const I = await import(`${ui}/inline.js`);
const M = await import(`${ui}/model.js`);
const E = await import(`${ui}/edits.js`);

// -- inline round trips ------------------------------------------------------------------

/** Deterministic spaced runs: every marked run sits between spaces, as typed text does. */
function samples(count) {
  let seed = 7;
  const rnd = () => (seed = (seed * 1664525 + 1013904223) % 4294967296) / 4294967296;
  const pick = list => list[Math.floor(rnd() * list.length)];
  const edge = ['a', 'bc', 'é', 'x1', '😀'];
  const words = ['a', 'b c', 'x*y', 'snake_case', '_u_', '~t~', '`q`', '[l]', '(p)', 'a&amp;b', '<h>', 'p|q', '1.', '# h', '> q', '\\', 'end.'];
  const out = [];
  for (let n = 0; n < count; n++) {
    const runs = [];
    for (let k = 0; k < 1 + Math.floor(rnd() * 5); k++) {
      const marked = k % 2 === 0;
      const run = { t: marked ? pick(edge) + (rnd() < .5 ? pick(words).trim() || 'k' : '') + pick(edge) : ` ${pick(words).trim() || 'w'}${rnd() < .5 ? pick(words) : ''} ` };
      if (marked) {
        if (rnd() < .4) run.b = true; if (rnd() < .4) run.i = true; if (rnd() < .3) run.s = true; if (rnd() < .2) run.a = 'https://x.y/z';
        if (rnd() < .2) { run.c = true; delete run.b; delete run.i; delete run.s; }
      }
      runs.push(run);
    }
    out.push(I.normalize(runs));
  }
  return out;
}
const plainOf = runs => I.plainText(runs);

if (process.argv.includes('--parity')) {
  const pairs = samples(300).map(runs => ({ md: I.serializeRuns(runs), plain: plainOf(runs).trim() }));
  for (const md of ['plain', '**b** and *i*', '`code` ~~s~~', '[l](https://x.y) tail', 'a\\*b', 'x[^1] y', '\\# not a heading', '1\\. not a list']) pairs.push({ md, plain: plainOf(I.parseInline(md)) });
  console.log(JSON.stringify(pairs));
  process.exit(0);
}

const results = [];
const test = (name, run) => { try { run(); results.push([name, null]); } catch (error) { results.push([name, error]); } };

test('every mark survives rich -> markdown -> rich', () => {
  for (const [mark, md] of [['b', '**x**'], ['i', '*x*'], ['s', '~~x~~'], ['c', '`x`']]) {
    const runs = I.parseInline(`a ${md} b`);
    assert.deepEqual(runs.map(r => [r.t, Boolean(r[mark])]), [['a ', false], ['x', true], [' b', false]], md);
    assert.equal(I.serializeRuns(runs), `a ${md} b`);
  }
  const link = I.parseInline('see [the **docs**](https://x.y/a_b) now');
  assert.deepEqual(link.map(r => [r.t, r.a ?? null, Boolean(r.b)]), [['see ', null, false], ['the ', 'https://x.y/a_b', false], ['docs', 'https://x.y/a_b', true], [' now', null, false]]);
  assert.equal(I.serializeRuns(link), 'see [the **docs**](https://x.y/a_b) now');
  for (const md of ['***both*** x', '**a *b* c**', '*a **b** c*', 'hard\\\nbreak', 'x[^1] y', '`a ` b', 'snake_case_word and 2\\*3']) {
    assert.equal(I.serializeRuns(I.parseInline(md)), md, md);
  }
});
test('literal text is escaped so it cannot be read as markup, or as a block', () => {
  for (const text of ['a * b', '_x_', 'a ~ b', '[l](u)', '<b>', '&amp;', '| pipe', '`tick`', '\\', '# heading', '- item', '1. one', '> quote', '---', '```ts']) {
    const md = I.serializeRuns([{ t: text }]);
    assert.equal(plainOf(I.parseInline(md)), text, `${text} -> ${md}`);
    assert.ok(!I.parseInline(md).some(r => I.hasMarks(r)), `${text} stays plain`);
  }
  assert.equal(I.serializeRuns([{ t: 'a|b' }], { cell: true }), 'a\\|b');
  assert.equal(I.serializeRuns([{ t: ' padded ' }]), 'padded');
  assert.equal(I.serializeRuns([{ t: ' padded ' }], { trim: false }), ' padded ');
});
test('marks around spaces keep the spaces outside the delimiters', () => {
  assert.equal(I.serializeRuns([{ t: 'a' }, { t: ' bold ', b: true }, { t: 'b' }]), 'a **bold** b');
  assert.equal(I.serializeRuns([{ t: ' ', b: true }]), '');
});
test('rich -> markdown -> rich is stable for generated text', () => {
  for (const runs of samples(1500)) {
    const md = I.serializeRuns(runs), back = I.parseInline(md);
    assert.equal(plainOf(back), plainOf(runs).trim(), `${JSON.stringify(runs)} -> ${md}`);
    assert.deepEqual(I.normalize(I.parseInline(I.serializeRuns(back))), back, `second trip ${md}`);
    const marks = rs => rs.flatMap(r => [...r.t].map(ch => `${ch}${r.b ? 'b' : ''}${r.i ? 'i' : ''}${r.s ? 's' : ''}${r.c ? 'c' : ''}${r.a ?? ''}`)).join('');
    assert.equal(marks(back), marks(runs).replace(/^ +| +$/g, ''), `marks of ${md}`);
  }
});
test('toggling marks over ranges', () => {
  let runs = I.parseInline('Hello big world');
  runs = I.toggleMark(runs, 6, 9, 'b');
  assert.equal(I.serializeRuns(runs), 'Hello **big** world');
  assert.equal(I.markState(runs, 0, 15, 'b'), 'some');
  assert.equal(I.markState(runs, 6, 9, 'b'), 'all');
  runs = I.toggleMark(runs, 0, 15, 'b');
  assert.equal(I.serializeRuns(runs), '**Hello big world**');
  runs = I.toggleMark(runs, 0, 15, 'b');
  assert.equal(I.serializeRuns(runs), 'Hello big world');
  runs = I.setMark(runs, 0, 5, 'a', 'https://a.b');
  assert.equal(I.serializeRuns(runs), '[Hello](https://a.b) big world');
  assert.equal(I.serializeRuns(I.setMark(runs, 0, 5, 'a', null)), 'Hello big world');
  assert.equal(I.serializeRuns(I.deleteRange(I.parseInline('a **bold** c'), 2, 6)), 'a  c');
});
test('inline shortcuts turn typed delimiters into marks', () => {
  const run = text => { const r = I.inlineShortcut([{ t: text }], text.length); return r && I.serializeRuns(r.runs); };
  assert.equal(run('x **bold**'), 'x **bold**');
  assert.deepEqual(I.inlineShortcut([{ t: 'x **bold**' }], 10).runs.map(r => [r.t, Boolean(r.b)]), [['x ', false], ['bold', true]]);
  assert.deepEqual(I.inlineShortcut([{ t: 'a _it_' }], 6).runs.map(r => [r.t, Boolean(r.i)]), [['a ', false], ['it', true]]);
  assert.deepEqual(I.inlineShortcut([{ t: '`co`' }], 4).runs.map(r => [r.t, Boolean(r.c)]), [['co', true]]);
  assert.deepEqual(I.inlineShortcut([{ t: '~~no~~' }], 6).runs.map(r => [r.t, Boolean(r.s)]), [['no', true]]);
  assert.equal(I.inlineShortcut([{ t: 'snake_case_' }], 11), null, 'underscores inside words are text');
  assert.equal(I.inlineShortcut([{ t: '** nope**' }], 9), null, 'a delimiter followed by a space is not an opener');
  assert.equal(I.inlineShortcut([{ t: 'plain' }], 5), null);
});
test('character offsets for comment anchors count code points', () => {
  assert.equal(I.toCodePoints('a😀b', 3), 2);
  assert.equal(I.fromCodePoints('a😀b', 2), 3);
});

// -- the doc model -----------------------------------------------------------------------------

const sample = () => ({
  title: 'T', theme: {}, blocks: [
    { id: 'b1', kind: 'title', text: 'Title' },
    { id: 'b2', kind: 'paragraph', text: 'Hello **bold** world' },
    { id: 'b3', kind: 'list', ordered: false, items: [{ text: 'one', children: [{ text: 'kid' }] }, { text: 'two' }] },
    { id: 'b4', kind: 'table', header: true, align: [], rows: [['a', 'b'], ['c', 'd']] },
    { id: 'b5', kind: 'heading', level: 2, text: 'Head' },
  ],
});
const apply = (doc, result) => M.applyOps(doc, result.ops);
test('block shortcuts at the start of a line', () => {
  assert.deepEqual(M.blockShortcut('## Hi'), { make: { kind: 'heading', level: 2, text: '' }, rest: 'Hi' });
  assert.equal(M.blockShortcut('### ').make.level, 3);
  assert.equal(M.blockShortcut('- x').make.kind, 'list');
  assert.equal(M.blockShortcut('* x').make.ordered, false);
  assert.equal(M.blockShortcut('1. x').make.ordered, true);
  assert.equal(M.blockShortcut('[] x').make.items[0].checked, false);
  assert.equal(M.blockShortcut('[x] y').make.items[0].checked, true);
  assert.equal(M.blockShortcut('> q').make.kind, 'quote');
  assert.deepEqual(M.blockShortcut('```ts ').make, { kind: 'code', lang: 'ts', text: '' });
  assert.equal(M.blockShortcut('---').make.kind, 'divider');
  assert.equal(M.blockShortcut('#hashtag'), null);
  assert.equal(M.blockShortcut('-no'), null);
  assert.equal(M.fenceOnEnter('```py').make.lang, 'py');
  assert.equal(M.fenceOnEnter('```py x'), null);
});
test('local operations match the backend order and merge-patch rules', () => {
  const doc = sample();
  const next = M.applyOps(doc, [
    { op: 'update_block', id: 'b2', patch: { text: 'New' } },
    { op: 'insert_blocks', after: 'b2', blocks: [{ id: 'n1', kind: 'paragraph', text: 'X' }] },
    { op: 'move_blocks', ids: ['b5'], after: 'b1' },
    { op: 'delete_blocks', ids: ['b4'] },
    { op: 'set_title', title: 'Z' },
  ]);
  assert.deepEqual(next.blocks.map(b => b.id), ['b1', 'b5', 'b2', 'n1', 'b3']);
  assert.equal(next.title, 'Z');
  assert.equal(doc.blocks.length, 5, 'the input is untouched');
  assert.equal(M.applyOps(doc, [{ op: 'insert_markdown', markdown: 'x' }]), null, 'what needs the backend is not guessed');
  assert.equal(M.applyOps(doc, [{ op: 'update_block', id: 'b2', patch: { kind: 'quote' } }]), null);
  // There is no "first" placement: moving to the top moves everything else behind the block.
  const top = M.applyOps(doc, M.moveOps(doc, 'b5', 'b1'));
  assert.deepEqual(top.blocks.map(b => b.id), ['b5', 'b1', 'b2', 'b3', 'b4']);
  const above = M.applyOps(doc, M.insertAtOps(doc, 0, [{ id: 'n0', kind: 'paragraph', text: '' }]));
  assert.equal(above.blocks[0].id, 'n0');
  const swapped = M.applyOps(doc, M.replaceOps(doc, 'b1', [{ id: 'b1', kind: 'heading', level: 1, text: 'Title' }]));
  assert.deepEqual(swapped.blocks.map(b => [b.id, b.kind])[0], ['b1', 'heading']);
});
test('comment anchors are offsets in the block plain text, built the way the core builds it', () => {
  const doc = sample();
  const list = M.block(doc, 'b3');
  assert.equal(M.blockPlainText(list), 'one\nkid\ntwo');
  assert.deepEqual(M.rangeParts(list, 2, 6), [{ path: 'item:1', start: 2, end: 3 }, { path: 'item:1.1', start: 0, end: 2 }]);
  assert.equal(M.blockOffset(list, 'item:2', 1), 9);
  assert.equal(M.blockPlainText(M.block(doc, 'b4')), 'a | b\nc | d');
  assert.equal(M.blockPlainText(M.block(doc, 'b2')), 'Hello bold world');
  assert.deepEqual(M.outline(doc).map(e => [e.level, e.text]), [[0, 'Title'], [2, 'Head']]);
});
test('the insert menu filters by name and keyword', () => {
  assert.equal(M.filterInsert('').length, M.INSERT_ITEMS.length);
  assert.equal(M.filterInsert('tab')[0].id, 'table');
  assert.equal(M.filterInsert('todo')[0].id, 'checklist');
  assert.equal(M.filterInsert('h2')[0].id, 'h2');
  assert.deepEqual(M.filterInsert('zzzz'), []);
  const kinds = new Set(M.INSERT_ITEMS.filter(i => i.make).map(i => i.make().kind));
  for (const kind of ['title', 'heading', 'paragraph', 'quote', 'callout', 'code', 'list', 'table', 'chart', 'divider', 'page_break', 'toc']) assert.ok(kinds.has(kind), kind);
  assert.ok(M.INSERT_ITEMS.some(i => i.file), 'images come from a file');
});

// -- structural edits -----------------------------------------------------------------------------

test('Enter splits a block and keeps marks on both sides', () => {
  const doc = sample();
  const r = E.enter(doc, { block: 'b2', part: 'text', offset: 9 });
  const next = apply(doc, r);
  assert.equal(next.blocks[1].text, 'Hello **bol**');
  assert.equal(next.blocks[2].text, '**d** world');
  assert.equal(next.blocks[2].kind, 'paragraph');
  assert.deepEqual(r.select, { block: next.blocks[2].id, part: 'text', offset: 0 });
  const head = E.enter(doc, { block: 'b5', part: 'text', offset: 4 });
  assert.equal(apply(doc, head).blocks[5].kind, 'paragraph', 'a heading continues as text');
  const start = apply(doc, E.enter(doc, { block: 'b2', part: 'text', offset: 0 }));
  assert.equal(start.blocks[1].text, '', 'Enter at the start opens an empty block above');
  assert.equal(start.blocks[2].id, 'b2');
});
test('Enter in a list adds an item, on an empty item it leaves the list', () => {
  const doc = sample();
  const next = apply(doc, E.enter(doc, { block: 'b3', part: 'item:1', offset: 1 }));
  assert.deepEqual(next.blocks[2].items.map(i => i.text), ['o', 'ne', 'two']);
  assert.equal(next.blocks[2].items[1].children[0].text, 'kid', 'children travel with the text after the caret');
  const empty = { ...sample(), blocks: [{ id: 'l', kind: 'list', ordered: false, items: [{ text: 'a' }, { text: '' }, { text: 'c' }] }] };
  const left = apply(empty, E.enter(empty, { block: 'l', part: 'item:2', offset: 0 }));
  assert.deepEqual(left.blocks.map(b => b.kind), ['list', 'paragraph', 'list']);
  assert.equal(left.blocks[0].items.length, 1); assert.equal(left.blocks[2].items[0].text, 'c');
  const nested = { ...sample(), blocks: [{ id: 'l', kind: 'list', ordered: false, items: [{ text: 'a', children: [{ text: '' }] }] }] };
  assert.equal(apply(nested, E.enter(nested, { block: 'l', part: 'item:1.1', offset: 0 })).blocks[0].items.length, 2, 'an empty nested item un-nests');
});
test('Backspace at the start: heading to paragraph, then merge; list item to paragraph', () => {
  const doc = sample();
  const turned = apply(doc, E.backspaceAtStart(doc, { block: 'b5', part: 'text', offset: 0 }));
  assert.deepEqual([turned.blocks[4].id, turned.blocks[4].kind, turned.blocks[4].text], ['b5', 'paragraph', 'Head']);
  const merged = E.backspaceAtStart({ ...doc, blocks: [doc.blocks[1], { id: 'p', kind: 'paragraph', text: ' tail' }] }, { block: 'p', part: 'text', offset: 0 });
  assert.equal(merged.ops[0].patch.text, 'Hello **bold** world tail');
  assert.deepEqual(merged.select, { block: 'b2', part: 'text', offset: 16 });
  const item = apply(doc, E.backspaceAtStart(doc, { block: 'b3', part: 'item:2', offset: 0 }));
  assert.deepEqual(item.blocks.slice(2, 4).map(b => b.kind), ['list', 'paragraph']);
  assert.equal(item.blocks[3].text, 'two');
  assert.equal(E.backspaceAtStart(doc, { block: 'b1', part: 'text', offset: 0 }).ops.length > 0, true);
  const first = { ...doc, blocks: [{ id: 'p', kind: 'paragraph', text: 'x' }] };
  assert.equal(E.backspaceAtStart(first, { block: 'p', part: 'text', offset: 0 }), null);
  const after = { ...doc, blocks: [{ id: 'd', kind: 'divider' }, { id: 'p', kind: 'paragraph', text: 'x' }] };
  assert.deepEqual(apply(after, E.backspaceAtStart(after, { block: 'p', part: 'text', offset: 0 })).blocks.map(b => b.id), ['p']);
});
test('Tab nests under the item above and Shift+Tab takes the followers along', () => {
  const doc = sample();
  const nested = apply(doc, E.indent(doc, { block: 'b3', part: 'item:2', offset: 0 }));
  assert.deepEqual(nested.blocks[2].items[0].children.map(i => i.text), ['kid', 'two']);
  assert.equal(E.indent(doc, { block: 'b3', part: 'item:1', offset: 0 }), null, 'the first item has nothing above it');
  const out = apply(nested, E.outdent(nested, { block: 'b3', part: 'item:1.1', offset: 0 }));
  assert.deepEqual(out.blocks[2].items.map(i => i.text), ['one', 'kid']);
  assert.deepEqual(out.blocks[2].items[1].children.map(i => i.text), ['two']);
  let deep = { blocks: [{ id: 'l', kind: 'list', items: [{ text: 'a', children: [{ text: 'b', children: [{ text: 'c', children: [{ text: 'd' }] }, { text: 'e' }] }] }] }] };
  assert.equal(E.indent(deep, { block: 'l', part: 'item:1.1.1.1', offset: 0 }), null, 'four levels is the limit');
});
test('deleting across blocks joins the ends and drops what is between', () => {
  const doc = sample();
  const r = E.deleteSelection(doc, { block: 'b2', part: 'text', offset: 6 }, { block: 'b5', part: 'text', offset: 2 });
  const next = apply(doc, r);
  assert.deepEqual(next.blocks.map(b => b.id), ['b1', 'b2']);
  assert.equal(next.blocks[1].text, 'Hello ad');
  assert.deepEqual(r.select, { block: 'b2', part: 'text', offset: 6 });
  const both = E.deleteSelection(doc, { block: 'b5', part: 'text', offset: 2 }, { block: 'b2', part: 'text', offset: 6 });
  assert.deepEqual(apply(doc, both).blocks.map(b => b.id), ['b1', 'b2'], 'the order of the ends does not matter');
  const toList = apply(doc, E.deleteSelection(doc, { block: 'b2', part: 'text', offset: 5 }, { block: 'b3', part: 'item:1', offset: 1 }));
  assert.equal(toList.blocks[1].text, 'Hellone');
  assert.deepEqual(toList.blocks[2].items.map(i => i.text), ['kid', 'two']);
  const within = apply(doc, E.deleteSelection(doc, { block: 'b3', part: 'item:1', offset: 1 }, { block: 'b3', part: 'item:2', offset: 1 }));
  assert.deepEqual(within.blocks[2].items.map(i => i.text), ['owo']);
  const cells = apply(doc, E.deleteSelection(doc, { block: 'b4', part: 'cell:0:1', offset: 0 }, { block: 'b4', part: 'cell:1:0', offset: 1 }));
  assert.deepEqual(cells.blocks[3].rows, [['a', ''], ['', 'd']]);
  const edge = apply(doc, E.deleteSelection(doc, { block: 'b2', part: 'text', offset: 0 }, { block: 'b4', part: null, side: 'after' }));
  assert.deepEqual(edge.blocks.map(b => b.id), ['b1', 'b2', 'b5']);
  assert.equal(edge.blocks[1].text, '');
});
test('turning a block into another keeps its id', () => {
  const doc = sample();
  const quote = apply(doc, E.turnInto(doc, 'b2', 'quote'));
  assert.deepEqual([quote.blocks[1].id, quote.blocks[1].kind, quote.blocks[1].text], ['b2', 'quote', 'Hello **bold** world']);
  const heading = apply(doc, E.turnInto(doc, 'b5', 'h1'));
  assert.equal(heading.blocks[4].level, 1);
  const list = apply(doc, E.turnInto(doc, 'b2', 'checklist'));
  assert.deepEqual(list.blocks[1].items, [{ text: 'Hello **bold** world', checked: false }]);
  const flat = apply(doc, E.turnInto(doc, 'b3', 'text'));
  assert.deepEqual(flat.blocks.slice(2, 5).map(b => b.text), ['one', 'kid', 'two']);
  assert.equal(flat.blocks[2].id, 'b3');
  const code = apply(doc, E.turnInto(doc, 'b2', 'code'));
  assert.equal(code.blocks[1].text, 'Hello bold world');
  const numbered = apply(doc, E.turnInto(doc, 'b3', 'numbers'));
  assert.equal(numbered.blocks[2].ordered, true);
  assert.equal(E.turnInto(doc, 'b4', 'text'), null, 'tables do not become text');
  const top = apply(doc, E.turnInto(doc, 'b1', 'h1'));
  assert.deepEqual(top.blocks.map(b => b.id), ['b1', 'b2', 'b3', 'b4', 'b5'], 'the first block can change kind in place');
});
test('inserting blocks: in place of an empty paragraph, after others, with a paragraph after structures', () => {
  const doc = { ...sample(), blocks: [{ id: 'p', kind: 'paragraph', text: '' }, { id: 'q', kind: 'paragraph', text: 'x' }] };
  const table = E.insertBlock(doc, 'p', INSERT('table'));
  const next = apply(doc, table);
  assert.deepEqual(next.blocks.map(b => b.kind), ['table', 'paragraph']);
  assert.deepEqual(next.blocks[0].rows.length, 3);
  const after = apply(doc, E.insertBlock(doc, 'q', INSERT('divider')));
  assert.deepEqual(after.blocks.map(b => b.kind), ['paragraph', 'paragraph', 'divider', 'paragraph']);
  const heading = E.insertBlock(doc, 'q', INSERT('h2'));
  assert.equal(heading.select.block, heading.id);
  const dup = apply(doc, E.duplicateBlock(doc, 'q'));
  assert.equal(dup.blocks[2].text, 'x'); assert.notEqual(dup.blocks[2].id, 'q');
});
function INSERT(id) { return M.INSERT_ITEMS.find(i => i.id === id).make(); }
test('tables: add and delete rows and columns, Tab and Enter move between cells', () => {
  const doc = sample();
  assert.equal(apply(doc, E.tableAddRow(doc, 'b4', 1)).blocks[3].rows.length, 3);
  assert.deepEqual(apply(doc, E.tableAddColumn(doc, 'b4', 0)).blocks[3].rows[0], ['a', '', 'b']);
  assert.deepEqual(apply(doc, E.tableDeleteColumn(doc, 'b4', 0)).blocks[3].rows[0], ['b']);
  assert.equal(E.tableDeleteRow({ blocks: [{ id: 't', kind: 'table', rows: [['x']] }] }, 't', 0), null);
  assert.deepEqual(E.tableStep(doc, { block: 'b4', part: 'cell:0:1', offset: 0 }, false).select.part, 'cell:1:0');
  assert.equal(E.tableStep(doc, { block: 'b4', part: 'cell:1:1', offset: 0 }, false).ops[0].patch.rows.length, 3, 'Tab in the last cell adds a row');
  assert.equal(E.tableDown(doc, { block: 'b4', part: 'cell:0:0', offset: 0 }).select.part, 'cell:1:0');
});
test('checkbox toggles one item', () => {
  const doc = { blocks: [{ id: 'c', kind: 'list', items: [{ text: 'a', checked: false }, { text: 'b', checked: true }] }] };
  const next = apply(doc, E.toggleCheck(doc, 'c', 'item:1'));
  assert.deepEqual(next.blocks[0].items.map(i => i.checked), [true, true]);
});

let failed = 0;
for (const [name, error] of results) {
  if (error) { failed++; console.log(`FAIL ${name}\n${error.stack}`); } else console.log(`ok   ${name}`);
}
console.log(`${results.length - failed}/${results.length} passed`);
if (failed) process.exit(1);
