// P4 visual/chooser/drop checks using doc-shot.mjs's existing Chromium harness.
import assert from 'node:assert/strict';
import { open, shot, finish } from './doc-harness.mjs';

const page = await open('dark');
await page.evaluate(() => {
  window.__templateCommands = [];
  const original = window.ipc.postMessage;
  window.ipc.postMessage = source => {
    const cmd = JSON.parse(source);
    window.__templateCommands.push(cmd);
    if (cmd.command === 'ImportTemplateData') setTimeout(() => window.studioEvent({ event: 'TemplateImported', slug: 'brand', name: 'Brand', warnings: [] }), 10);
    else if (cmd.command === 'ApplyDesign') setTimeout(() => { window.studioEvent({ event: 'Doc', doc: window.__mock.doc, colors: window.__docColors, revision: 'p4', relative: 'docs/q3.doc.json', threads: window.__mock.threads, linked: true, activities: {}, changed: [], assets: {}, design: { slug: 'brand', name: 'Brand', colors: window.__mock.doc.theme.colors, fonts: window.__mock.doc.theme.fonts, update_available: false, template: true }, can_undo: true, can_redo: false }); }, 10);
    else if (cmd.command === 'Designs') setTimeout(() => window.studioEvent({ event: 'Designs', available: [{ slug: 'brand', name: 'Brand', current: true, colors: {}, fonts: {} }] }), 10);
    else if (cmd.command === 'Export') setTimeout(() => window.studioEvent({ event: 'Exported', path: '/exports/brand.pptx', warnings: [] }), 10);
    else original(source);
  };
});
await page.getByRole('button', { name: 'Design', exact: true }).click();
await page.getByRole('button', { name: 'Add template…', exact: true }).waitFor();
await shot(page, 'p4-design-add-template');
const chooser = page.waitForEvent('filechooser');
await page.getByRole('button', { name: 'Add template…', exact: true }).click();
await (await chooser).setFiles({ name: 'brand.dotx', mimeType: 'application/octet-stream', buffer: Buffer.from('chooser fixture') });
await page.getByRole('button', { name: 'Use for this doc', exact: true }).click();
await page.waitForTimeout(100);
await page.getByRole('button', { name: 'Export', exact: true }).click();
const toggle = page.getByRole('menuitemcheckbox', { name: /Into template/ });
await toggle.waitFor();
assert.equal(await toggle.getAttribute('aria-checked'), 'true');
await shot(page, 'p4-export-into-template');
// Unchecking the toggle makes the next export a plain file.
await toggle.click();
assert.equal(await toggle.getAttribute('aria-checked'), 'false');
await page.getByRole('menuitem', { name: /Word document/ }).click();
await page.waitForTimeout(50);
assert((await page.evaluate(() => window.__templateCommands)).some(c => c.command === 'DocExport' && c.template === 'none'));
await page.evaluate(() => {
  const transfer = new DataTransfer(); transfer.items.add(new File(['drop fixture'], 'brand.potx'));
  document.body.dispatchEvent(new DragEvent('drop', { bubbles: true, dataTransfer: transfer }));
});
await page.getByRole('button', { name: 'Use for this doc', exact: true }).waitFor();
const commands = await page.evaluate(() => window.__templateCommands);
assert(commands.some(c => c.command === 'ImportTemplateData' && c.name === 'brand.dotx'));
assert(commands.some(c => c.command === 'ImportTemplateData' && c.name === 'brand.potx'));
await page.getByRole('button', { name: 'Use for this doc', exact: true }).click();
await page.waitForTimeout(50);
await page.evaluate(() => {
  const theme = structuredClone(window.__mock.doc.theme);
  theme.size = { w: 1920, h: 1080 };
  theme.text_styles.title = { family: 'heading', size: 80, weight: 700, color: 'fg' };
  window.studioAsyncEvent({ event: 'Deck', deck: { version: 2, title: 'Brand presentation', design: 'brand', theme, slides: [{ id: 's1', layout: { mode: 'none' }, notes: '', children: [{ id: 'b1', kind: 'text', style: 'title', text: 'Brand presentation', position: { x: 140, y: 320 }, sizing: { w: 1500, h: 160 } }] }] }, colors: window.__docColors, revision: 'deck-p4', hashes: {}, images: {}, threads: [], linked: true, activities: {}, changed: [], import_warnings: [], template: true });
});
await page.getByRole('button', { name: 'Design', exact: true }).click();
await page.locator('#add-template').waitFor();
await page.locator('#deck-design-list button').filter({ hasText: 'Brand' }).waitFor();
await shot(page, 'p4-deck-design-add-template');
await page.locator('#export').click();
await page.locator('#export-template[aria-checked="true"]').waitFor();
await shot(page, 'p4-deck-export-into-template');
await page.locator('#export-plain').click();
await page.waitForTimeout(50);
assert((await page.evaluate(() => window.__templateCommands)).some(c => c.command === 'Export' && c.template === 'none'));
await page.close();
await finish();
