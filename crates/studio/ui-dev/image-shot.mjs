// Run: node crates/studio/ui-dev/image-shot.mjs
// Images on the deck canvas against mock.js: paste into a selected shape (the
// shape becomes a masked image in place, one undo step), the mask grid, the
// focal point pad, "Use as mask for image below" from the context menu, and
// dropping a file on the slide. Writes screenshots to ui-dev/shots/.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const candidates = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'];
const installed = candidates.find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const ui = path.resolve(here, '../ui'), out = path.join(here, 'shots'); fs.mkdirSync(out, { recursive:true });
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/workspace.json'), 'utf8'));
const mock = fs.readFileSync(path.join(here, 'mock.js'), 'utf8');
const types = { '.html':'text/html', '.js':'text/javascript', '.css':'text/css' };
const server = http.createServer((req, res) => {
  const name = decodeURIComponent(req.url.split('?')[0]), target = path.resolve(ui, name === '/' ? 'index.html' : `.${name}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target) || !fs.statSync(target).isFile()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type':types[path.extname(target)] || 'application/octet-stream', 'Content-Security-Policy':"default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'" }); fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch();
const errors = [];
const page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1, colorScheme:'dark', reducedMotion:'reduce' });
page.on('pageerror', error => errors.push(error.message));
await page.addInitScript(value => { window.__studioFixture = value; }, fixture); await page.addInitScript({ content:mock });
const shot = name => page.screenshot({ path:path.join(out, `image-${name}.png`) });
const settle = () => page.waitForTimeout(120);
const deck = () => page.evaluate(() => window.__mock.deck);
const nodes = async () => (await deck()).slides[0].children;
const node = async nodeId => (await nodes()).find(n => n.id === nodeId);
const applies = () => page.evaluate(() => window.__mock.commands.filter(c => c.command === 'Apply').length);
// A small PNG in the page, as the clipboard or a drop would carry it.
const png = () => page.evaluateHandle(() => new Promise(resolve => { const c = document.createElement('canvas'); c.width = 64; c.height = 32; c.getContext('2d').fillRect(0, 0, 64, 32); c.toBlob(blob => resolve(new File([blob], 'photo.png', { type:'image/png' }))); }));
async function paste() {
  const file = await png();
  await page.evaluate(file => { const data = new DataTransfer(); data.items.add(file); document.dispatchEvent(new ClipboardEvent('paste', { clipboardData:data, bubbles:true })); }, file);
  await page.waitForTimeout(250);
}
try {
  await page.goto(`http://127.0.0.1:${server.address().port}/`); await page.waitForFunction(() => document.querySelectorAll('#strip .slide-item').length === 3);
  await page.evaluate(() => window.__mock.commands.length = 0);

  // Paste into a selected rectangle: same id, box and stacking; one Apply.
  await page.locator('#add-shape').click(); await settle();
  const shape = (await nodes()).find(n => n.kind === 'shape');
  let before = await applies();
  await paste();
  let image = await node(shape.id);
  assert.equal(image.kind, 'image'); assert.equal(image.fit, 'cover'); assert.ok(image.src.startsWith('assets/pasted-'));
  assert.deepEqual([image.position, image.sizing], [shape.position, shape.sizing]);
  assert.equal(image.mask, undefined); assert.equal(image.fill, undefined);
  assert.equal(await applies(), before + 1, 'one Apply, so one undo step');
  await page.locator('#undo').dispatchEvent('click'); await settle();
  assert.equal((await node(shape.id)).kind, 'shape', 'undo brings the shape back');

  // A square ellipse takes a circle mask.
  await page.evaluate(id => window.__mock.externalEdit([{ op:'update_node', id, fields:{ shape:'ellipse', sizing:{ w:420, h:420 } }, base_hash:'mock' }]), shape.id); await settle();
  await page.locator(`#canvas [data-id="${shape.id}"]`).click(); await settle();
  await paste();
  assert.equal((await node(shape.id)).mask, 'circle');
  assert.match(await page.locator(`#canvas [data-id="${shape.id}"] .image-content`).evaluate(n => n.style.clipPath), /^circle\(/);

  // The mask grid, with the image selected.
  await page.locator('#more-properties').dispatchEvent('click'); await settle();
  assert.equal(await page.locator('.mask-tile[aria-pressed="true"]').getAttribute('aria-label'), 'Circle mask');
  await page.getByRole('button', { name:'Arch mask' }).click(); await settle();
  assert.equal((await node(shape.id)).mask, 'arch');
  assert.equal(await page.locator(`#canvas [data-id="${shape.id}"] .image-content`).evaluate(n => n.style.clipPath), 'inset(0px round 9999px 9999px 0px 0px)');
  await page.locator('#properties-panel').evaluate(n => { n.scrollTop = n.querySelector('.mask-grid').offsetTop - 120; });
  await shot('mask-grid');
  await page.getByRole('button', { name:'Rounded mask' }).click(); await settle();
  image = await node(shape.id); assert.equal(image.mask, undefined); assert.equal(image.radius, 24);
  await page.getByRole('button', { name:'None mask' }).click(); await settle();
  image = await node(shape.id); assert.equal(image.mask, undefined); assert.equal(image.radius, undefined);
  await page.getByRole('button', { name:'Blob mask' }).click(); await settle();
  assert.match(await page.locator(`#canvas [data-id="${shape.id}"] .image-content`).evaluate(n => n.style.clipPath), /^polygon\(/);
  // Fill or fit, and the focal point.
  await page.locator('.image-fit').getByRole('button', { name:'Fit' }).click(); await settle();
  assert.equal((await node(shape.id)).fit, 'contain');
  await page.getByRole('slider', { name:'Focal point' }).focus(); await page.keyboard.press('ArrowRight'); await settle();
  assert.equal((await node(shape.id)).focus.x, .52);
  before = await applies();
  const pad = await page.getByRole('slider', { name:'Focal point' }).boundingBox();
  await page.mouse.move(pad.x + pad.width * .5, pad.y + pad.height * .5); await page.mouse.down();
  await page.mouse.move(pad.x + pad.width * .1, pad.y + pad.height * .9, { steps:6 }); await page.mouse.up(); await settle();
  image = await node(shape.id);
  assert.ok(Math.abs(image.focus.x - .1) < .02 && Math.abs(image.focus.y - .9) < .02, JSON.stringify(image.focus));
  assert.equal(await applies(), before + 1, 'a drag is one Apply');

  // A new shape over the image masks it from the context menu.
  await page.keyboard.press('Escape');
  await page.locator('#add-shape').click(); await settle();
  const cover = (await nodes()).filter(n => n.kind === 'shape').at(-1);
  await page.locator(`#canvas [data-id="${cover.id}"]`).click({ button:'right' }); await settle();
  await shot('context-menu');
  await page.locator('.canvas-menu').getByRole('button', { name:'Use as mask for image below' }).click(); await settle();
  image = await node(shape.id);
  assert.equal(await node(cover.id), undefined, 'the mask shape is used up');
  assert.deepEqual([image.position, image.sizing, image.fit], [cover.position, cover.sizing, 'cover']);
  await page.locator('#undo').dispatchEvent('click'); await settle();
  assert.ok(await node(cover.id), 'one undo step brings the shape back');

  // Fill with image… uses the native picker; a cancel changes nothing.
  await page.locator(`#canvas [data-id="${cover.id}"]`).click({ button:'right' }); await settle();
  await page.evaluate(() => { window.__mock.pickCancels = true; });
  before = await applies();
  await page.locator('.canvas-menu').getByRole('button', { name:'Fill with image…' }).click(); await settle();
  assert.equal(await applies(), before); assert.equal((await node(cover.id)).kind, 'shape');
  await page.evaluate(() => { window.__mock.pickCancels = false; });
  await page.locator(`#canvas [data-id="${cover.id}"]`).click({ button:'right' }); await settle();
  await page.locator('.canvas-menu').getByRole('button', { name:'Fill with image…' }).click(); await settle();
  assert.equal((await node(cover.id)).src, 'assets/picked.png');

  // Dropping a file on an empty part of the slide adds an image there.
  await page.keyboard.press('Escape'); await settle();
  const count = (await nodes()).length, file = await png(), box = await page.locator('#canvas').boundingBox();
  await page.evaluate(([file, x, y]) => { const data = new DataTransfer(); data.items.add(file); const target = document.elementFromPoint(x, y); target.dispatchEvent(new DragEvent('drop', { dataTransfer:data, bubbles:true, cancelable:true, clientX:x, clientY:y })); }, [file, box.x + 20, box.y + box.height - 20]);
  await page.waitForTimeout(250);
  const added = (await nodes()).at(-1);
  assert.equal((await nodes()).length, count + 1); assert.equal(added.kind, 'image'); assert.equal(Math.round(added.sizing.w / added.sizing.h), 2, 'keeps the 2:1 image ratio');
  await shot('done');
  assert.deepEqual(errors, []);
  console.log('PASS: paste into shape (same id/box, one undo), circle from a square ellipse, mask grid (arch/rounded/none/blob), fill/fit, focal keys and drag (one Apply), mask image below from the menu (one undo), picker cancel and pick, drop onto the slide.');
} finally { await browser.close(); server.close(); }
