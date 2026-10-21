// Run: node crates/studio/ui-dev/shot.mjs
// Uses the existing web dependency installation; no package install or app build.
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
try {
  for (const colorScheme of ['light','dark']) {
    for (const scenario of ['editing','thread','preview','options','review','properties']) {
      const page = await browser.newPage({ viewport:{ width:1440,height:900 }, deviceScaleFactor:1, colorScheme, reducedMotion:'reduce' });
      page.on('pageerror', error => errors.push(`${scenario}: ${error.message}`));
      await page.addInitScript(value => { window.__studioFixture = value; }, fixture); await page.addInitScript({ content:mock });
      await page.goto(`http://127.0.0.1:${server.address().port}/`); await page.waitForFunction(() => document.querySelectorAll('#strip .slide-item').length === 3);
      if (scenario === 'editing' || scenario === 'properties') {
        await page.locator('#canvas .block[data-id="b1"]').click();
        if (scenario === 'properties') await page.locator('#toggle-properties').click();
        else await page.locator('#floating-toolbar').waitFor({ state:'visible' });
      } else if (scenario === 'thread' || scenario === 'preview') {
        await page.locator('#comment-mode').click(); await page.locator('#pins .pin').first().click(); await page.locator('#thread-popover .edit-card').waitFor();
        if (scenario === 'preview') {
          await page.getByRole('button', { name:'Preview', exact:true }).click(); await page.locator('#canvas .ghost-changed').waitFor();
          const before = await page.locator('#canvas').textContent(); await page.keyboard.press('b'); assert.notEqual(await page.locator('#canvas').textContent(), before); await page.keyboard.press('b');
        }
      } else if (scenario === 'options') {
        await page.locator('#chat-input').fill('Give me two options for this slide.'); await page.locator('#chat-send').click(); await page.locator('.options-grid .edit-card').first().waitFor();
        await page.locator('#chat-messages').evaluate(node => node.scrollTop = node.scrollHeight);
      } else if (scenario === 'review') {
        await page.locator('#review').click(); await page.waitForFunction(() => document.querySelector('#comment-count').textContent === '4'); await page.locator('#toggle-comments').click();
      }
      await page.waitForTimeout(180);
      const output = path.join(out, `${scenario}-${colorScheme}.png`); await page.screenshot({ path:output }); console.log(output);
      if (scenario === 'preview') {
        await page.locator('#preview-apply').click(); await page.locator('#preview-bar').waitFor({ state:'hidden' }); assert.match(await page.locator('#canvas').textContent(), /Find your focus/);
        await page.locator('#undo').click(); await page.waitForFunction(() => document.querySelector('#canvas').textContent.includes('Less noise. More room'));
      }
      if (scenario === 'thread') {
        await page.getByRole('button', { name:'Resolve thread', exact:true }).click(); await page.getByRole('button', { name:'Reopen thread', exact:true }).waitFor(); await page.getByRole('button', { name:'Reopen thread', exact:true }).click();
      }
      if (scenario === 'options') { await page.getByRole('button', { name:'Use this option', exact:true }).first().click(); await page.waitForFunction(() => document.querySelector('#canvas').textContent.includes('More possibility')); }
      await page.close();
    }
  }
  assert.deepEqual(errors, []); console.log('12 screenshots · light + dark · preview/before-after/apply/undo, resolve/reopen and choose-option interactions passed');
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
