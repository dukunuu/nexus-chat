// Run: node crates/studio/ui-dev/doc-e2e.mjs
// Drives the doc editor in Chromium against the mock backend and asserts the editing
// behaviors that matter: typing batches, Enter, Backspace, Markdown shortcuts, marks,
// the "/" menu, comments and suggestions, cross-block deletes, paste, undo, external
// edits, page preview and the design and export panels. Writes shots to shots/.
import assert from 'node:assert/strict';
import { open, shot, finish, fixture } from './doc-harness.mjs';

const part = (page, block, path = 'text') => page.locator(`[data-block="${block}"] [data-part="${path}"]`);
const mockDoc = page => page.evaluate(() => window.__mock.doc);
const blockOf = async (page, id) => (await mockDoc(page)).blocks.find(b => b.id === id);
const sent = (page, op) => page.evaluate(() => window.__mock.commands.map(c => c.command));
/** Put the caret at a plain-text offset in a part (the Home and End keys differ by platform). */
async function caretAt(page, block, path = 'text', offset = Infinity) {
  await part(page, block, path).click();
  await page.evaluate(([block, path, offset]) => {
    const root = document.querySelector(`[data-block="${block}"] [data-part="${path}"]`);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let seen = 0, node = walker.nextNode(), last = node;
    const range = document.createRange();
    while (node) { if (seen + node.length >= offset) { range.setStart(node, offset - seen); range.collapse(true); break; } seen += node.length; last = node; node = walker.nextNode(); }
    if (!node) { if (last) { range.setStart(last, last.length); } else range.setStart(root, 0); range.collapse(true); }
    const s = getSelection(); s.removeAllRanges(); s.addRange(range);
  }, [block, path, offset]);
}
const caretAtEnd = (page, block, path) => caretAt(page, block, path);
const settle = page => page.waitForTimeout(950);
const tests = {
  async typing() {
    const page = await open('dark');
    await caretAtEnd(page, 'b20');
    await page.keyboard.type(' Thanks.');
    assert.equal((await page.locator('#doc-app .save-state').textContent()), 'Saving');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive. Thanks.');
    const applies = await page.evaluate(() => window.__mock.commands.filter(c => c.command === 'DocApply'));
    assert.equal(applies.length, 1, 'typing is batched into one Apply per pause');
    assert.equal(applies[0].ops[0].op, 'update_block');
    assert.equal(await page.locator('#doc-app .save-state').textContent(), 'Saved');
    await page.close();
  },
  async enterAndBackspace() {
    const page = await open('dark');
    await caretAt(page, 'b20', 'text', 4);
    await page.keyboard.press('Enter');
    await settle(page);
    let doc = await mockDoc(page);
    const at = doc.blocks.findIndex(b => b.id === 'b20');
    assert.equal(doc.blocks[at].text, 'Meth');
    assert.equal(doc.blocks[at + 1].text, 'odology notes live in the shared drive.');
    assert.equal(doc.blocks[at + 1].kind, 'paragraph');
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 1, 'Enter is one undo step');
    // The caret is at the start of the new block; Backspace merges it back.
    await page.keyboard.press('Backspace');
    await settle(page);
    doc = await mockDoc(page);
    assert.equal(doc.blocks.find(b => b.id === 'b20').text, 'Methodology notes live in the shared drive.');
    assert.equal(doc.blocks.length, 20);
    await page.close();
  },
};
/** Select from one location to another (offsets count plain text in each part). */
async function select(page, [fb, fp, fo], [tb, tp, to]) {
  await page.evaluate(([a, b]) => {
    const point = ([block, path, offset]) => {
      const root = document.querySelector(`[data-block="${block}"] [data-part="${path}"]`);
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      let seen = 0, node = walker.nextNode(), last = null;
      while (node) { if (seen + node.length >= offset) return [node, offset - seen]; seen += node.length; last = node; node = walker.nextNode(); }
      return last ? [last, last.length] : [root, 0];
    };
    document.querySelector('#doc-surface').focus();
    document.querySelector(`[data-block="${a[0]}"]`).scrollIntoView({ block: 'center' });
    const [sn, so] = point(a), [en, eo] = point(b), range = document.createRange();
    range.setStart(sn, so); range.setEnd(en, eo);
    const s = getSelection(); s.removeAllRanges(); s.addRange(range);
  }, [[fb, fp, fo], [tb, tp, to]]);
  await page.waitForTimeout(60);
}
/** The block right after b20, where `freshParagraph` puts new content. */
const lastBlock = async page => { const blocks = (await mockDoc(page)).blocks; return blocks[blocks.findIndex(b => b.id === 'b20') + 1]; };
/** A fresh empty paragraph at the end of the doc with the caret in it. */
async function freshParagraph(page) {
  await caretAt(page, 'b20');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(40);
}
const key = (page, k, n = 1) => (async () => { for (let i = 0; i < n; i++) await page.keyboard.press(k); })();

Object.assign(tests, {
  async markdownShortcuts() {
    const page = await open('dark');
    for (const [typed, check] of [
      ['## Next steps', b => b.kind === 'heading' && b.level === 2 && b.text === 'Next steps'],
      ['# Big', b => b.kind === 'heading' && b.level === 1 && b.text === 'Big'],
      ['### Small', b => b.kind === 'heading' && b.level === 3],
      ['- one', b => b.kind === 'list' && !b.ordered && b.items[0].text === 'one'],
      ['* star', b => b.kind === 'list' && !b.ordered && b.items[0].text === 'star'],
      ['1. first', b => b.kind === 'list' && b.ordered && b.items[0].text === 'first'],
      ['[] todo', b => b.kind === 'list' && b.items[0].checked === false && b.items[0].text === 'todo'],
      ['> wise', b => b.kind === 'quote' && b.text === 'wise'],
    ]) {
      await freshParagraph(page);
      await page.keyboard.type(typed);
      await settle(page);
      assert.ok(check(await lastBlock(page)), `${typed}: ${JSON.stringify(await lastBlock(page))}`);
    }
    await freshParagraph(page);
    await page.keyboard.type('```ts ');
    await page.keyboard.type('let x');
    await settle(page);
    const code = await lastBlock(page);
    assert.equal(code.kind, 'code'); assert.equal(code.lang, 'ts'); assert.equal(code.text, 'let x');
    // Enter inside code is a newline, not a new block.
    await page.keyboard.press('Enter'); await page.keyboard.type('let y');
    await settle(page);
    assert.equal((await lastBlock(page)).text, 'let x\nlet y');
    await freshParagraph(page);
    await page.keyboard.type('---');
    await settle(page);
    const doc = await mockDoc(page);
    const at = doc.blocks.findIndex(b => b.id === 'b20');
    assert.equal(doc.blocks[at + 1].kind, 'divider', 'a divider was made');
    assert.equal(doc.blocks[at + 2].kind, 'paragraph', 'with a paragraph to continue in');
    await shot(page, 'doc-shortcuts');
    await page.close();
  },
  async inlineMarks() {
    const page = await open('dark');
    await freshParagraph(page);
    await page.keyboard.type('a **bold** b _it_ c `code` d ~~gone~~ e');
    await settle(page);
    assert.equal((await lastBlock(page)).text, 'a **bold** b *it* c `code` d ~~gone~~ e');
    const html = await page.evaluate(() => document.querySelector('.blk:last-child .part').innerHTML);
    assert.match(html, /<strong>bold<\/strong>/); assert.match(html, /<em>it<\/em>/); assert.match(html, /<code>code<\/code>/); assert.match(html, /<s>gone<\/s>/);
    assert.ok(!/\*\*/.test(await page.evaluate(() => document.querySelector('.blk:last-child .part').textContent)), 'no raw ** shown');
    await page.close();
  },
  async keyboardMarksAndToolbar() {
    const page = await open('dark');
    await select(page, ['b20', 'text', 0], ['b20', 'text', 11]);
    await page.locator('.sel-toolbar').waitFor();
    assert.equal(await page.locator('.sel-toolbar button[aria-label^="Bold"]').getAttribute('aria-pressed'), 'false');
    await shot(page, 'doc-toolbar');
    await page.keyboard.press('Meta+b');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, '**Methodology** notes live in the shared drive.');
    await page.keyboard.press('Meta+b');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive.');
    await page.keyboard.press('Meta+i'); await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, '*Methodology* notes live in the shared drive.');
    await page.keyboard.press('Meta+e'); await settle(page);
    assert.match((await blockOf(page, 'b20')).text, /^\*`Methodology`\* notes/);
    await page.keyboard.press('Meta+Shift+x'); await settle(page);
    assert.match((await blockOf(page, 'b20')).text, /~~/);
    // Toolbar button.
    await page.locator('.sel-toolbar button[aria-label^="Bold"]').click();
    await settle(page);
    assert.match((await blockOf(page, 'b20')).text, /\*\*/);
    // Style dropdown turns the block into a heading.
    await page.locator('.tb-style').click();
    await page.locator('.style-menu button', { hasText: 'Heading 2' }).click();
    await settle(page);
    const turned = await blockOf(page, 'b20');
    assert.equal(turned.kind, 'heading'); assert.equal(turned.level, 2);
    await page.close();
  },
  async linkPopover() {
    const page = await open('dark');
    await select(page, ['b20', 'text', 0], ['b20', 'text', 11]);
    await page.keyboard.press('Meta+k');
    await page.locator('.link-pop input').fill('example.com/docs');
    await page.keyboard.press('Enter');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, '[Methodology](https://example.com/docs) notes live in the shared drive.');
    assert.equal(await page.locator('[data-block="b20"] a.link').getAttribute('href'), 'https://example.com/docs');
    await select(page, ['b20', 'text', 0], ['b20', 'text', 11]);
    await page.keyboard.press('Meta+k');
    await page.locator('.link-pop input').fill('javascript:alert(1)');
    await page.keyboard.press('Enter');
    assert.match(await page.locator('.link-hint').textContent(), /http/);
    await page.keyboard.press('Escape');
    await page.close();
  },
  async slashMenu() {
    const page = await open('dark');
    await freshParagraph(page);
    await page.keyboard.type('/');
    await page.locator('.slash').waitFor();
    assert.ok(await page.locator('.slash .menu-row').count() >= 14, 'every block kind is offered');
    await shot(page, 'doc-slash');
    await page.keyboard.type('tab');
    assert.equal(await page.locator('.slash .menu-row').count(), 1);
    await page.keyboard.press('Enter');
    await settle(page);
    const doc = await mockDoc(page);
    assert.ok(doc.blocks.some((b, i) => b.kind === 'table' && i > 19));
    // Arrow keys move, Escape closes, filtering finds by keyword.
    await freshParagraph(page);
    await page.keyboard.type('/');
    await page.keyboard.press('ArrowDown'); await page.keyboard.press('ArrowDown');
    assert.equal(await page.locator('.slash .menu-row.active .menu-label').textContent(), 'Heading 1');
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('.slash').count(), 0);
    await page.keyboard.press('Backspace');
    await page.keyboard.type('/todo');
    await page.keyboard.press('Enter');
    await settle(page);
    const last = (await mockDoc(page)).blocks.filter(b => b.kind === 'list').at(-1);
    assert.equal(last.items[0].checked, false);
    await page.close();
  },
  async listsNestAndExit() {
    const page = await open('dark');
    await caretAt(page, 'b5', 'item:3');
    await page.keyboard.press('Enter'); await page.keyboard.type('Pdf');
    await page.keyboard.press('Tab');
    await settle(page);
    let list = await blockOf(page, 'b5');
    assert.equal(list.items.length, 3);
    assert.equal(list.items[2].children.at(-1).text, 'Pdf', 'Tab nests under the item above');
    await page.keyboard.press('Shift+Tab'); await settle(page);
    list = await blockOf(page, 'b5');
    assert.equal(list.items.length, 4);
    // Enter on an empty item leaves the list.
    await page.keyboard.press('Enter');
    await page.keyboard.press('Enter');
    await settle(page);
    const doc = await mockDoc(page);
    const at = doc.blocks.findIndex(b => b.id === 'b5');
    assert.equal(doc.blocks[at + 1].kind, 'paragraph');
    assert.equal(doc.blocks[at].items.length, 4);
    // Backspace at the start of a list item makes it a paragraph first.
    await caretAt(page, 'b8', 'item:2', 0);
    await page.keyboard.press('Backspace');
    await settle(page);
    const after = await mockDoc(page);
    assert.equal(after.blocks.find(b => b.id === 'b8').items.length, 1);
    assert.ok(after.blocks.some(b => b.kind === 'paragraph' && b.text === 'Agree the numbers'));
    await page.close();
  },
  async checkboxes() {
    const page = await open('dark');
    await page.locator('[data-block="b6"] [data-check="item:2"]').click();
    await settle(page);
    assert.equal((await blockOf(page, 'b6')).items[1].checked, true);
    await page.close();
  },
  async headingBackspace() {
    const page = await open('dark');
    await caretAt(page, 'b12', 'text', 0);
    await page.keyboard.press('Backspace');
    await settle(page);
    assert.equal((await blockOf(page, 'b12')).kind, 'paragraph');
    assert.equal((await blockOf(page, 'b12')).text, 'Results');
    await page.keyboard.press('Backspace');
    await settle(page);
    const doc = await mockDoc(page);
    assert.equal(doc.blocks.find(b => b.id === 'b11').text, 'Numbers are preliminary until finance signs off.Results');
    await page.close();
  },
  async crossBlockDelete() {
    const page = await open('dark');
    await select(page, ['b9', 'text', 5], ['b11', 'text', 8]);
    await page.keyboard.press('Delete');
    await settle(page);
    const doc = await mockDoc(page);
    assert.equal(doc.blocks.find(b => b.id === 'b9').text, 'Make are preliminary until finance signs off.');
    assert.ok(!doc.blocks.some(b => b.id === 'b10' || b.id === 'b11'));
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 1);
    // Typing over a cross-block selection replaces it.
    await select(page, ['b2', 'text', 3], ['b4', 'text', 2]);
    await page.keyboard.type('X');
    await settle(page);
    const typed = await blockOf(page, 'b2');
    assert.match(typed.text, /^We Xals$/);
    await page.close();
  },
  async undoRedo() {
    const page = await open('dark');
    await caretAt(page, 'b20');
    await page.keyboard.type('!');
    await page.keyboard.press('Meta+z');
    await page.waitForTimeout(200);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive.');
    assert.equal(await part(page, 'b20').textContent(), 'Methodology notes live in the shared drive.');
    await page.keyboard.press('Meta+Shift+z');
    await page.waitForTimeout(200);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive.!');
    await page.close();
  },
  async paste() {
    const page = await open('dark');
    await caretAt(page, 'b20');
    const paste = (html, text) => page.evaluate(([html, text]) => {
      const data = new DataTransfer();
      if (html) data.setData('text/html', html);
      if (text) data.setData('text/plain', text);
      document.querySelector('#doc-surface').dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    }, [html, text]);
    await paste('<b>bold</b> and <i>it</i>', 'bold and it');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive.**bold** and *it*');
    await paste(null, 'First paragraph.\n\nSecond one.');
    await settle(page);
    let doc = await mockDoc(page);
    const at = doc.blocks.findIndex(b => b.id === 'b20');
    assert.equal(doc.blocks[at + 1].text, 'First paragraph.');
    assert.equal(doc.blocks[at + 2].text, 'Second one.');
    await paste('<h2>Pasted heading</h2><ul><li>one</li><li>two</li></ul>', 'Pasted heading\none\ntwo');
    await settle(page);
    doc = await mockDoc(page);
    assert.ok(doc.blocks.some(b => b.kind === 'heading' && b.text === 'Pasted heading'), JSON.stringify(doc.blocks.slice(-4)));
    await page.close();
  },
});
Object.assign(tests, {
  async commentsAnchoredAndSent() {
    const page = await open('dark');
    await select(page, ['b9', 'text', 0], ['b9', 'text', 15]);
    await page.keyboard.press('Meta+Alt+m');
    await page.locator('.card.draft textarea').waitFor();
    await shot(page, 'doc-comment-composer');
    await page.locator('.card.draft textarea').fill('Is this still the right framing?');
    await page.locator('.card.draft').getByRole('button', { name: 'Send to nexus' }).click();
    await page.locator('.card[data-thread^="new-"]').waitFor();
    const create = await page.evaluate(() => window.__mock.commands.find(c => c.command === 'DocCommentCreate'));
    assert.deepEqual(create.anchor, { block: 'b9', range: { start: 0, end: 15 }, quote: 'Make the report' });
    assert.equal(create.send, true);
    assert.equal(await page.locator('mark.cmt[data-thread^="new-"]').textContent(), 'Make the report');
    // The toolbar's comment button does the same.
    await select(page, ['b20', 'text', 0], ['b20', 'text', 11]);
    await page.locator('.sel-toolbar button[aria-label^="Comment"]').click();
    await page.locator('.card.draft textarea').fill('Note only');
    await page.locator('.card.draft').getByRole('button', { name: 'Comment', exact: true }).click();
    await page.waitForFunction(() => document.querySelectorAll('.card[data-thread^="new-"]').length === 2);
    const second = await page.evaluate(() => window.__mock.commands.filter(c => c.command === 'DocCommentCreate').at(-1));
    assert.equal(second.send, false);
    await page.close();
  },
  async cardsAlignAndStack() {
    const page = await open('dark');
    const tops = (await page.evaluate(() => [...document.querySelectorAll('.margin-track > .card')].map(c => [c.dataset.thread, c.offsetTop, c.offsetHeight]))).sort((a, b) => a[1] - b[1]);
    for (let i = 1; i < tops.length; i++) assert.ok(tops[i][1] >= tops[i - 1][1] + tops[i - 1][2], `cards do not overlap: ${JSON.stringify(tops)}`);
    // The first card sits beside its anchor.
    const [card, anchor] = await page.evaluate(() => [document.querySelector('.card[data-thread="t1"]').getBoundingClientRect().top, document.querySelector('mark.cmt[data-thread="t1"]').getBoundingClientRect().top]);
    assert.ok(Math.abs(card - anchor) < 14, `${card} vs ${anchor}`);
    // Opening a lower thread slides it to its anchor.
    await page.locator('.card[data-thread="t2"]').click();
    await page.waitForTimeout(100);
    const [c2, a2] = await page.evaluate(() => [document.querySelector('.card[data-thread="t2"]').getBoundingClientRect().top, document.querySelector('mark.cmt[data-thread="t2"]').getBoundingClientRect().top]);
    assert.ok(Math.abs(c2 - a2) < 14, `active card beside its anchor: ${c2} vs ${a2}`);
    await shot(page, 'doc-comment-active');
    // Hovering the card tints its text, and the reverse.
    await page.locator('.card[data-thread="t1"]').hover();
    assert.equal(await page.locator('mark.cmt[data-thread="t1"].hot').count() > 0, true);
    await page.locator('mark.cmt[data-thread="t1"]').first().hover();
    assert.ok(await page.locator('.card[data-thread="t1"].hot').count());
    await page.close();
  },
  async suggestionsAreTrackedChanges() {
    const page = await open('dark');
    assert.equal(await page.locator('[data-block="b2"] del.sug-del').textContent(), 'ahead of plan');
    assert.equal(await page.locator('[data-block="b2"] ins.sug-ins').textContent(), 'two weeks ahead of plan');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('del.sug-del')).textDecorationLine), 'line-through');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('ins.sug-ins')).textDecorationLine), 'underline');
    // Typing elsewhere in the block does not make the proposal part of the text.
    await caretAt(page, 'b2', 'text', 2);
    await page.keyboard.type('!');
    await settle(page);
    assert.match((await blockOf(page, 'b2')).text, /^We! grew/);
    assert.ok(!(await blockOf(page, 'b2')).text.includes('two weeks'));
    await page.locator('.card[data-thread="t3"] .edit').getByRole('button', { name: 'Accept' }).click();
    await page.waitForTimeout(200);
    assert.match((await blockOf(page, 'b2')).text, /two weeks ahead of plan/);
    assert.equal(await page.locator('del.sug-del').count(), 0);
    await page.close();
    const again = await open('dark');
    await again.getByRole('button', { name: 'Reject all' }).click();
    await again.waitForTimeout(200);
    assert.equal(await again.locator('del.sug-del').count(), 0);
    assert.ok(!(await blockOf(again, 'b2')).text.includes('two weeks'));
    await again.close();
    const third = await open('dark');
    await third.getByRole('button', { name: 'Accept all' }).click();
    await third.waitForTimeout(200);
    assert.match((await blockOf(third, 'b2')).text, /two weeks ahead of plan/);
    await third.close();
  },
  async resolveAndActivity() {
    const page = await open('dark');
    assert.match(await page.locator('.resolved-fold').textContent(), /Resolved \(1\)/);
    await page.locator('.card[data-thread="t2"] .resolve').click({ force: true });
    await page.waitForTimeout(150);
    assert.match(await page.locator('.resolved-fold').textContent(), /Resolved \(2\)/);
    assert.equal(await page.locator('mark.cmt[data-thread="t2"]').count(), 0, 'resolved threads lose their highlight');
    await page.locator('.resolved-fold').click();
    assert.equal(await page.locator('.resolved-list .card').count(), 2);
    await page.evaluate(() => window.__mock.activity('t1', 'working'));
    await page.waitForTimeout(100);
    assert.equal(await page.locator('#doc-app .link-pill').textContent(), 'linked · nexus working');
    await page.locator('.card[data-thread="t1"]').click();
    assert.match(await page.locator('.card[data-thread="t1"] .activity').textContent(), /nexus is working/);
    await page.evaluate(() => window.__mock.activity('t1', 'done', 'edited blocks b2'));
    await page.waitForTimeout(100);
    assert.match(await page.locator('.card[data-thread="t1"] .activity').textContent(), /Done/);
    await shot(page, 'doc-activity');
    await page.close();
  },
  async externalEditFlashesAndUndoes() {
    const page = await open('dark');
    await page.evaluate(() => window.__mock.externalEdit([{ op: 'update_block', id: 'b9', patch: { text: 'Written by the model.' } }, { op: 'update_block', id: 'b20', patch: { text: 'Also changed.' } }], 'b9'));
    await page.locator('.blk.flash').first().waitFor();
    assert.deepEqual(await page.locator('.blk.flash').evaluateAll(n => n.map(x => x.dataset.block).sort()), ['b20', 'b9']);
    assert.match(await page.locator('.doc-toast').textContent(), /changed on disk/);
    assert.equal(await part(page, 'b9').textContent(), 'Written by the model.');
    await page.locator('.doc-toast').getByRole('button', { name: 'Undo' }).click();
    await page.waitForTimeout(200);
    assert.equal(await part(page, 'b9').textContent(), 'Make the report and the deck look like one company.');
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 0, 'one undo step took the whole edit back');
    await page.close();
  },
  async conflictDropsTheLocalAction() {
    const page = await open('dark');
    await page.evaluate(() => { window.__mock.rejectNext = true; });
    await caretAt(page, 'b20');
    await page.keyboard.type('x');
    await settle(page);
    assert.match(await page.locator('.doc-toast').textContent(), /Mock rejected/);
    await page.waitForTimeout(100);
    assert.equal(await part(page, 'b20').textContent(), 'Methodology notes live in the shared drive.', 'the file wins');
    await page.close();
  },
  async previewExportDesign() {
    const page = await open('dark');
    await page.getByRole('button', { name: 'Pages' }).click();
    await page.locator('.preview-page img').first().waitFor();
    assert.equal(await page.locator('.preview-page').count(), 3);
    assert.equal(await page.locator('.page-scroll').isVisible(), false);
    await shot(page, 'doc-preview');
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('.page-scroll').isVisible(), true);
    // A new revision refreshes the open preview.
    await page.getByRole('button', { name: 'Pages' }).click();
    await page.locator('.preview-page img').first().waitFor();
    const before = await page.evaluate(() => window.__mock.commands.filter(c => c.command === 'DocPages').length);
    await page.evaluate(() => window.__mock.externalEdit([{ op: 'set_title', title: 'Changed' }]));
    await page.waitForFunction(n => window.__mock.commands.filter(c => c.command === 'DocPages').length > n, before);
    await page.getByRole('button', { name: 'Back to editing' }).click();
    await page.getByRole('button', { name: 'Export' }).click();
    const items = await page.locator('.export-pop .menu-row').allTextContents();
    assert.deepEqual(items, ['Word document.docx', 'PDF.pdf', 'Markdown.md', 'Web page.html', 'Into templatecoming soon']);
    assert.equal(await page.locator('.export-pop .menu-row').last().isDisabled(), true);
    await shot(page, 'doc-export');
    await page.locator('.export-pop .menu-row', { hasText: 'Markdown' }).click();
    await page.locator('.doc-toast', { hasText: 'Exported q3-strategy.md' }).waitFor();
    await page.locator('.doc-toast').getByRole('button', { name: 'Reveal' }).click();
    assert.ok(await page.evaluate(() => window.__mock.commands.some(c => c.command === 'Reveal')));
    await page.getByRole('button', { name: 'Design' }).click();
    await page.locator('.design-row', { hasText: 'Warm paper' }).waitFor();
    await shot(page, 'doc-design');
    await page.locator('.design-row', { hasText: 'Warm paper' }).click();
    assert.ok(await page.evaluate(() => window.__mock.commands.some(c => c.command === 'DocApplyDesign' && c.design === 'warm')));
    await page.close();
  },
  async designUpdateIsAnnounced() {
    const page = await open('dark');
    await page.evaluate(() => { window.__mock.designUpdate = true; window.__mock.emitCurrent(); });
    await page.getByRole('button', { name: 'Design' }).click();
    await page.getByText('Update available').waitFor();
    await page.getByRole('button', { name: 'Apply update' }).click();
    assert.ok(await page.evaluate(() => window.__mock.commands.some(c => c.command === 'DocApplyDesign' && c.design === null)));
    await page.close();
  },
  async handleDragAndBlockMenu() {
    const page = await open('dark');
    await part(page, 'b9').scrollIntoViewIfNeeded();
    const box = await part(page, 'b9').boundingBox();
    await page.mouse.move(box.x + 60, box.y + 8);
    const handle = page.locator('.block-handle');
    await handle.waitFor({ state: 'visible' });
    const target = await part(page, 'b11').boundingBox();
    const h = await handle.boundingBox();
    await page.mouse.move(h.x + 10, h.y + 10);
    await page.mouse.down();
    await page.mouse.move(h.x + 10, h.y + 40, { steps: 4 });
    await page.mouse.move(h.x + 10, target.y + target.height + 14, { steps: 8 });
    await page.mouse.up();
    await settle(page);
    const ids = (await mockDoc(page)).blocks.map(b => b.id);
    assert.ok(ids.indexOf('b9') > ids.indexOf('b11'), `b9 moved after b11: ${ids}`);
    // Click opens the menu; Duplicate adds a copy.
    await part(page, 'b20').scrollIntoViewIfNeeded();
    const b = await part(page, 'b20').boundingBox();
    await page.mouse.move(b.x + 60, b.y + 8);
    await handle.click();
    await page.locator('.block-menu').waitFor();
    await shot(page, 'doc-block-menu');
    await page.locator('.block-menu').getByRole('menuitem', { name: 'Duplicate' }).click();
    await settle(page);
    assert.equal((await mockDoc(page)).blocks.filter(x => x.text === 'Methodology notes live in the shared drive.').length, 2);
    await page.close();
  },
  async tableControls() {
    const page = await open('dark');
    await part(page, 'b13', 'cell:1:0').scrollIntoViewIfNeeded();
    const box = await part(page, 'b13', 'cell:1:0').boundingBox();
    await page.mouse.move(box.x + 20, box.y + 10);
    await page.locator('.table-add.row').waitFor({ state: 'visible' });
    await page.locator('.table-add.row').click();
    await page.locator('.table-add.column').click();
    await settle(page);
    const table = await blockOf(page, 'b13');
    assert.equal(table.rows.length, 5);
    assert.equal(table.rows[0].length, 4);
    // Tab moves between cells; Enter goes down a row.
    await caretAt(page, 'b13', 'cell:0:0', 0);
    await page.keyboard.press('Tab'); await page.keyboard.type('R');
    await page.keyboard.press('Enter'); await page.keyboard.type('D');
    await settle(page);
    const typed = await blockOf(page, 'b13');
    assert.equal(typed.rows[0][1], 'RevenueR');
    assert.equal(typed.rows[1][1], '$4.1mD');
    await page.close();
  },
  async emptyDocShowsPlaceholders() {
    const blank = structuredClone(fixture);
    blank.doc.blocks = []; blank.threads = [];
    const page = await open('dark', { width: 1440, height: 900 }, { fixture: blank });
    assert.equal(await page.locator('.ghost [data-ph]').count(), 2);
    const phs = await page.locator('.ghost .part').evaluateAll(n => n.map(x => [x.dataset.ph, getComputedStyle(x, '::before').content]));
    assert.deepEqual(phs, [['Untitled', '"Untitled"'], ['Start writing, or press / for blocks', '"Start writing, or press / for blocks"']]);
    await shot(page, 'doc-empty');
    await page.locator('.ghost.blk-title .part').click();
    await page.keyboard.type('Quarterly review');
    await settle(page);
    const doc = await mockDoc(page);
    assert.equal(doc.blocks[0].kind, 'title'); assert.equal(doc.blocks[0].text, 'Quarterly review');
    assert.equal(doc.blocks[1].kind, 'paragraph');
    assert.equal(await page.locator('.ghost').count(), 0);
    // The caret continues in the title; Enter moves on to the body, whose placeholder stays.
    await page.keyboard.press('Enter');
    await page.keyboard.type('First line');
    await settle(page);
    assert.equal((await mockDoc(page)).blocks[1].text, 'First line');
    await page.close();
  },
  async keyboardAndAccessibility() {
    const page = await open('dark');
    const headings = await page.locator('#doc-surface h1, #doc-surface h2, #doc-surface h3, #doc-surface h4').evaluateAll(n => n.map(x => `${x.tagName}:${x.textContent}`));
    assert.deepEqual(headings.slice(0, 3), ['H1:Q3 strategy', 'H2:Goals', 'H4:Plan'].slice(0, 3).map((x, i) => i === 2 ? 'H3:Plan' : x));
    // Every button in the chrome has a name.
    const unnamed = await page.evaluate(() => [...document.querySelectorAll('#doc-app button')].filter(b => !(b.getAttribute('aria-label') || b.textContent.trim() || b.title)).length);
    assert.equal(unnamed, 0);
    await select(page, ['b9', 'text', 0], ['b9', 'text', 4]);
    await page.locator('.sel-toolbar').waitFor();
    const toolbarButtons = await page.locator('.sel-toolbar button').evaluateAll(n => n.map(b => b.getAttribute('aria-label')));
    assert.ok(toolbarButtons.every(Boolean), toolbarButtons.join());
    // Focus ring on a bar button.
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: 'Design' }).focus();
    await page.keyboard.press('Tab');
    assert.ok(await page.evaluate(() => getComputedStyle(document.activeElement).outlineStyle !== 'none'));
    await page.close();
    const motion = await open('dark', { width: 1440, height: 900 }, { reducedMotion: 'reduce' });
    assert.equal(await motion.evaluate(() => getComputedStyle(document.querySelector('.card')).transitionDuration), '0s');
    await motion.close();
  },
  async narrowWindow() {
    const page = await open('dark', { width: 1000, height: 800 });
    assert.equal(await page.locator('.doc-outline').evaluate(n => n.getBoundingClientRect().right <= 0), true, 'the outline collapses below 1100px');
    await page.locator('.outline-toggle').click();
    await page.locator('.doc-outline.open').waitFor();
    await page.locator('.outline-link', { hasText: 'Results' }).click();
    await shot(page, 'doc-narrow');
    await page.close();
  },
  async focusAndLinkMessages() {
    const page = await open('dark');
    await select(page, ['b9', 'text', 0], ['b9', 'text', 4]);
    await page.waitForFunction(() => window.__mock.sent.some(s => s.type === 'Focus' && s.selection === 'Make'));
    const focus = await page.evaluate(() => window.__mock.sent.find(s => s.selection === 'Make'));
    assert.equal(focus.block, 'b9');
    await page.close();
  },
});
Object.assign(tests, {
  async selectAllDelete() {
    const page = await open('dark');
    await caretAt(page, 'b20');
    await page.keyboard.press('Meta+a');
    await page.keyboard.press('Backspace');
    await settle(page);
    const doc = await mockDoc(page);
    assert.ok(doc.blocks.length <= 1, `everything went: ${doc.blocks.map(b => b.id)}`);
    assert.equal(doc.blocks[0]?.text ?? '', '');
    assert.equal(await page.locator('#doc-surface .blk').count() >= 1, true);
    // Undo brings it all back as one step.
    await page.keyboard.press('Meta+z');
    await page.waitForTimeout(200);
    assert.equal((await mockDoc(page)).blocks.length, 20);
    await page.close();
  },
  async arrowKeysCrossBlocks() {
    const page = await open('dark');
    await caretAt(page, 'b9', 'text', 0);
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('ArrowUp');
    const block = await page.evaluate(() => getSelection().anchorNode.parentElement.closest('[data-block]').dataset.block);
    assert.notEqual(block, 'b9', 'the caret left the quote');
    await caretAt(page, 'b9', 'text');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.type('Z');
    await settle(page);
    const doc = await mockDoc(page);
    assert.ok([...doc.blocks].some(b => b.id === 'b10' && /Z/.test(b.text)), 'ArrowDown moved into the callout below');
    await page.close();
  },
  async titleAndHeadingStayInSync() {
    const page = await open('dark');
    await caretAt(page, 'b1');
    await page.keyboard.type(' 2026');
    await settle(page);
    const doc = await mockDoc(page);
    assert.equal(doc.blocks[0].text, 'Q3 strategy 2026');
    assert.equal(doc.title, 'Q3 strategy 2026', 'the document title follows the title block');
    assert.equal(await page.locator('#doc-app .doc-title-input').inputValue(), 'Q3 strategy 2026');
    // Renaming from the bar updates the title block too.
    await page.locator('.doc-title-input').fill('Renamed');
    await page.keyboard.press('Enter');
    await settle(page);
    const after = await mockDoc(page);
    assert.equal(after.title, 'Renamed'); assert.equal(after.blocks[0].text, 'Renamed');
    assert.match(await page.locator('.doc-outline .outline-link').first().textContent(), /Renamed/);
    await page.close();
  },
  async codeLanguageAndCalloutTone() {
    const page = await open('dark');
    await caretAt(page, 'b16', 'lang');
    await page.keyboard.press('Meta+a');
    await page.keyboard.type('rust');
    await settle(page);
    assert.equal((await blockOf(page, 'b16')).lang, 'rust');
    assert.equal((await blockOf(page, 'b16')).text.split('\n')[0], 'const growth = (now: number, then: number) =>');
    await part(page, 'b11').scrollIntoViewIfNeeded();
    const box = await part(page, 'b11').boundingBox();
    await page.mouse.move(box.x + 80, box.y + 8);
    await page.locator('[data-block="b11"] .callout-icon').click();
    await page.locator('.block-menu').getByRole('menuitem', { name: 'Success' }).click();
    await settle(page);
    assert.equal((await blockOf(page, 'b11')).tone, 'success');
    assert.equal(await page.locator('[data-block="b11"]').getAttribute('data-tone'), 'success');
    await page.close();
  },
  async imageWidthAndToc() {
    const page = await open('dark');
    await part(page, 'b14', 'caption').scrollIntoViewIfNeeded();
    const box = await page.locator('[data-block="b14"] img').boundingBox();
    await page.mouse.move(box.x + 60, box.y + 20);
    await page.locator('.block-handle').click();
    await page.locator('.block-menu').getByRole('menuitem', { name: 'Wide' }).click();
    await settle(page);
    assert.equal((await blockOf(page, 'b14')).width, 'wide');
    // Table of contents entries scroll to their heading.
    await page.evaluate(() => { document.querySelector('.page-scroll').scrollTop = 0; });
    await page.locator('.toc a', { hasText: 'Results' }).click();
    await page.waitForTimeout(250);
    const top = await page.locator('[data-block="b12"]').evaluate(n => n.getBoundingClientRect().top);
    assert.ok(top > 100 && top < 700, `Results is in view: ${top}`);
    await page.close();
  },
  async captionsAndImages() {
    const page = await open('dark');
    await caretAt(page, 'b14', 'caption');
    await page.keyboard.type(' (draft)');
    await settle(page);
    assert.equal((await blockOf(page, 'b14')).caption, 'Figure 1 · Revenue by month (draft)');
    // A pasted image goes through ImportAsset and lands as an image block.
    await caretAt(page, 'b20');
    await page.evaluate(() => {
      const bytes = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='), c => c.charCodeAt(0));
      const data = new DataTransfer();
      data.items.add(new File([bytes], 'shot.png', { type: 'image/png' }));
      document.querySelector('#doc-surface').dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await page.waitForFunction(() => window.__mock.commands.some(c => c.command === 'DocImportAsset'));
    await settle(page);
    const doc = await mockDoc(page);
    assert.ok(doc.blocks.some(b => b.kind === 'image' && b.src === 'assets/pasted.png' && b.alt === 'shot'));
    const upload = await page.evaluate(() => window.__mock.commands.find(c => c.command === 'DocImportAsset'));
    assert.match(upload.data, /^89504e47/, 'the bytes travel as hex');
    await page.close();
  },
});
Object.assign(tests, {
  async typingThenEnterIsOneUndoStep() {
    const page = await open('dark');
    await caretAt(page, 'b20');
    await page.keyboard.type(' quick');
    await page.keyboard.press('Enter');
    await settle(page);
    const applies = await page.evaluate(() => window.__mock.commands.filter(c => c.command === 'DocApply'));
    assert.equal(applies.length, 1, 'the typing and the split share one Apply');
    assert.deepEqual(applies[0].ops.map(o => o.op), ['update_block', 'insert_blocks']);
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 1);
    await page.close();
  },
  async islandsAreSelectedWholeAndDeleted() {
    const page = await open('dark');
    await page.locator('[data-block="b17"]').scrollIntoViewIfNeeded();
    await page.locator('[data-block="b17"] hr').click();
    assert.equal(await page.locator('.blk.selected').count(), 1);
    await page.keyboard.press('Backspace');
    await settle(page);
    assert.ok(!(await mockDoc(page)).blocks.some(b => b.id === 'b17'), 'the divider is gone');
    await page.locator('[data-block="b14"] img').click();
    await page.keyboard.press('Delete');
    await settle(page);
    assert.ok(!(await mockDoc(page)).blocks.some(b => b.id === 'b14'), 'the image is gone');
    await page.close();
  },
  async clickingBelowTheTextEndsTheDocument() {
    const page = await open('dark');
    await page.evaluate(() => { document.querySelector('.page-scroll').scrollTop = 99999; });
    const sheet = await page.locator('.sheet').boundingBox();
    await page.mouse.click(sheet.x + 100, sheet.y + sheet.height - 30);
    await page.keyboard.type('Tail');
    await settle(page);
    assert.equal((await blockOf(page, 'b20')).text, 'Methodology notes live in the shared drive.Tail');
    await page.close();
  },
});
const only = process.argv.slice(2);
let failed = 0;
for (const [name, run] of Object.entries(tests)) {
  if (only.length && !only.includes(name)) continue;
  try { await run(); console.log(`ok   ${name}`); } catch (error) { failed++; console.log(`FAIL ${name}\n${error.stack}`); }
}
await finish();
if (failed) process.exitCode = 1;
