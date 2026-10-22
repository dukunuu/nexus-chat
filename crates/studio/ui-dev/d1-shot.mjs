// D1 terminal identity + comment regression harness. No dependencies installed.
// Run: node crates/studio/ui-dev/d1-shot.mjs
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const installed = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/d1-comments.json'), 'utf8'));
const mock = fs.readFileSync(path.join(here, 'mock.js'), 'utf8');
const ui = path.resolve(here, '../ui'), out = path.join(here, 'shots');
fs.mkdirSync(out, { recursive:true });
const types = { '.html':'text/html', '.js':'text/javascript', '.css':'text/css' };
const server = http.createServer((req, res) => {
  const name = decodeURIComponent(req.url.split('?')[0]), target = path.resolve(ui, name === '/' ? 'index.html' : `.${name}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target) || !fs.statSync(target).isFile()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type':types[path.extname(target)] || 'application/octet-stream', 'Content-Security-Policy':"default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'" });
  fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch(), errors = [];
const geometry = [];
try {
  for (const theme of ['default', 'light']) {
    const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${theme}.json`), 'utf8'));
    // Deliberately use the *opposite* OS preference: the terminal palette wins.
    const page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1, colorScheme:theme === 'default' ? 'light' : 'dark', reducedMotion:'reduce' });
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', request => { if (!request.url().startsWith(`http://127.0.0.1:${server.address().port}/`)) errors.push(`Unexpected request: ${request.url()}`); });
    await page.addInitScript(value => { window.__studioFixture = value; }, fixture);
    await page.addInitScript({ content:mock });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => document.querySelectorAll('#canvas .block').length === 3);
    await page.evaluate(colors => window.studioAsyncEvent({ event:'Theme', colors }), colors);
    assert.equal(await page.locator('html').evaluate(n => getComputedStyle(n).getPropertyValue('--bg')), colors.bg);
    assert.equal(await page.locator('html').evaluate(n => getComputedStyle(n).colorScheme), theme === 'default' ? 'dark' : 'light');
    assert.equal(await page.locator('#canvas').evaluate(n => getComputedStyle(n).backgroundColor), 'rgb(11, 16, 32)');
    await page.locator('#comment-mode').click();
    assert.match(await page.locator('#canvas').evaluate(n => getComputedStyle(n).cursor), /data:image\/svg\+xml/);
    await page.locator('#pins .pin').first().click();
    await page.locator('#thread-popover .edit-card.pending').waitFor();
    assert.deepEqual(await page.locator('.thread-message .message-text code').allTextContents(), ['m', 'l']);
    assert.equal(await page.locator('.thread-message .message-text strong').textContent(), 'larger size');
    assert.equal(await page.locator('.thread-message .message-text em').textContent(), 'muted color');
    assert.match(await page.locator('.edit-kicker').textContent(), /size 42 → 58 px/);
    assert.doesNotMatch(await page.locator('.edit-card').textContent(), /Suggested edit/);
    assert.equal(await page.locator('.message-author.model .avatar').textContent(), '✦');
    assert.equal(await page.locator('.message-author.model .avatar').evaluate(n => getComputedStyle(n).color), theme === 'default' ? 'rgb(205, 0, 205)' : 'rgb(128, 86, 176)');
    for (const zoom of [.5, 1, 2]) {
      await page.keyboard.press('Shift+Digit0');
      assert.ok(Math.abs((await page.locator('#canvas').boundingBox()).width - 1920) < 1, 'Shift+0 is native 100%, not fit');
      const button = zoom < 1 ? '#zoom-out' : '#zoom-in';
      for (let i = 0; i < Math.abs(zoom - 1) * 10; i++) await page.locator(button).click();
      await page.waitForTimeout(30);
      const rects = await page.evaluate(() => {
        const rect = node => { const r = node.getBoundingClientRect(); return { x:r.x, y:r.y, w:r.width, h:r.height }; };
        return { pins:[...document.querySelectorAll('#pins .pin')].map(rect), blocks:[...document.querySelectorAll('#canvas .block')].map(rect) };
      });
      for (const pin of rects.pins) {
        assert.ok(Math.abs(pin.w - 24) < .1 && Math.abs(pin.h - 24) < .1, `Pin size at zoom ${zoom}: ${JSON.stringify(pin)}`);
        for (const block of rects.blocks) assert.ok(pin.x + pin.w <= block.x || pin.x >= block.x + block.w || pin.y + pin.h <= block.y || pin.y >= block.y + block.h, `Pin overlaps content at zoom ${zoom}`);
      }
      geometry.push({ theme, zoom, widths:rects.pins.map(pin => Number(pin.w.toFixed(2))) });
    }
    await page.keyboard.press('Shift+Digit1');
    assert.equal(await page.locator('#zoom-fit').textContent(), 'Fit');
    const beforePan = await page.locator('#canvas').boundingBox();
    await page.keyboard.down('Space');
    await page.mouse.move(beforePan.x + beforePan.width * .4, beforePan.y + beforePan.height * .4);
    await page.mouse.down(); await page.mouse.move(beforePan.x + beforePan.width * .4 + 60, beforePan.y + beforePan.height * .4 + 40); await page.mouse.up();
    await page.keyboard.up('Space');
    const afterPan = await page.locator('#canvas').boundingBox();
    assert.ok(Math.abs(afterPan.x - beforePan.x - 60) < .1 && Math.abs(afterPan.y - beforePan.y - 40) < .1, 'Space-drag pans without moving slide content');
    const pointer = { x:afterPan.x + afterPan.width * .4, y:afterPan.y + afterPan.height * .4 };
    await page.mouse.move(pointer.x, pointer.y);
    await page.keyboard.down('Control'); await page.mouse.wheel(0, -20); await page.keyboard.up('Control'); await page.waitForTimeout(60);
    const zoomed = await page.locator('#canvas').boundingBox();
    assert.ok(zoomed.width > afterPan.width, 'Ctrl+scroll zooms');
    assert.ok(Math.abs(zoomed.x + zoomed.width * .4 - pointer.x) < 1 && Math.abs(zoomed.y + zoomed.height * .4 - pointer.y) < 1, 'Zoom keeps the point under the pointer stationary');
    await page.mouse.wheel(60, 90); await page.waitForTimeout(60);
    const scrolled = await page.locator('#canvas').boundingBox();
    assert.ok(Math.abs(scrolled.x - zoomed.x + 60) < 1 && Math.abs(scrolled.y - zoomed.y + 90) < 1, 'Two-axis scroll pans');
    await page.keyboard.press('Shift+Digit1');
    const fitted = await page.locator('#canvas').boundingBox();
    assert.ok(Math.abs(fitted.x - beforePan.x) < 1 && Math.abs(fitted.y - beforePan.y) < 1, 'Fit resets pan');
    await page.screenshot({ path:path.join(out, `d1-comments-${theme}.png`) });
    const noteSize = await page.locator('#canvas [data-id="note"]').evaluate(n => n.style.fontSize);
    await page.getByRole('button', { name:'Preview', exact:true }).click();
    await page.locator('#canvas .ghost-changed').waitFor();
    await page.locator('#preview-apply').click();
    await page.locator('.edit-card.applied').waitFor();
    assert.equal(await page.locator('.edit-status').textContent(), 'Applied · undo with Cmd+Z');
    await page.screenshot({ path:path.join(out, `d1-applied-${theme}.png`) });
    await page.locator('#undo').click();
    await page.waitForFunction(size => document.querySelector('#canvas [data-id="note"]').style.fontSize === size, noteSize);
    // Untrusted Markdown stays readable text and cannot create executable DOM.
    await page.locator('#thread-popover textarea').fill('Check `source()` and [reference](javascript:alert(1)). <img src=x onerror=alert(1)>');
    await page.getByRole('button', { name:'Note', exact:true }).click();
    await page.waitForFunction(() => document.querySelector('.thread-message:last-of-type .message-text code')?.textContent === 'source()');
    assert.equal(await page.locator('#thread-popover img,#thread-popover a,#thread-popover script').count(), 0);
    assert.match(await page.locator('.thread-message:last-of-type .message-text').textContent(), /<img src=x onerror=alert\(1\)>/);
    await page.getByRole('button', { name:'Close thread' }).click();
    // A free-point draft is offset above/right, with no pointer-following badge.
    const canvas = await page.locator('#canvas').boundingBox();
    await page.mouse.click(canvas.x + canvas.width * .5, canvas.y + canvas.height * .16);
    await page.locator('#thread-popover textarea').waitFor();
    const draft = await page.locator('#pins span.pin').boundingBox();
    const reframed = await page.locator('#canvas').boundingBox();
    const anchor = { x:reframed.x + Math.floor(.5 * 1920) / 1920 * reframed.width, y:reframed.y + Math.floor(.16 * 1080) / 1080 * reframed.height };
    assert.ok(draft.x > anchor.x && draft.y + draft.height < anchor.y, 'A free-point badge is outside the anchor, above/right after the thread reflows the canvas');
    const count = await page.locator('#pins .pin').count();
    await page.mouse.move(400, 200);
    assert.equal(await page.locator('#pins .pin').count(), count);
    await page.getByRole('button', { name:'Close thread' }).click();
    await page.locator('#edit-mode').click();
    await page.locator('#canvas [data-id="title"]').click();
    await page.locator('#toggle-properties').click();
    await page.screenshot({ path:path.join(out, `d1-layers-${theme}.png`) });
    await page.close();
  }
  assert.deepEqual(errors, []);
  fs.writeFileSync(path.join(out, 'd1-pin-geometry.json'), `${JSON.stringify(geometry, null, 2)}\n`);
  console.log(JSON.stringify(geometry));
  console.log('6 screenshots at 1440×900 · terminal themes override OS preference · pins 24px at 0.5/1/2× native zoom · no node overlap · safe Markdown · real edit summary · preview/apply/undo · pointer-anchored zoom · space-drag/scroll pan · fit/100%');
  console.log(out);
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
