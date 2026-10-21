// Run: node crates/studio/ui-dev/shot.mjs
// Uses the existing web dependency installation; no package install or app build.
// Writes 1440x900 light and dark screenshots to crates/studio/ui-dev/shots/ and
// drives the interactions that matter: an external (model) edit is one undo
// step, a model comment's suggested edit previews and applies, and comments
// are only sent to nexus while the link is up.
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
const scenarios = ['editing', 'external', 'thread', 'preview', 'comments', 'properties'];
const modelEdit = [
  { op:'update_block', slide:'s1', id:'b1', fields:{ text:'Room to do your\nbest work.' }, base_hash:'mock' },
  { op:'update_block', slide:'s1', id:'subtitle', fields:{ text:'Less noise. More room for the work that matters most.' }, base_hash:'mock' },
];
try {
  for (const colorScheme of ['light', 'dark']) {
    for (const scenario of scenarios) {
      const page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1, colorScheme, reducedMotion:'reduce' });
      page.on('pageerror', error => errors.push(`${scenario}: ${error.message}`));
      await page.addInitScript(value => { window.__studioFixture = value; }, fixture); await page.addInitScript({ content:mock });
      await page.goto(`http://127.0.0.1:${server.address().port}/`); await page.waitForFunction(() => document.querySelectorAll('#strip .slide-item').length === 3);
      // Studio is a pure canvas: no chat pane, no host connection, and it says it is linked.
      assert.equal(await page.locator('#chat-pane, #chat-input, #host-status, #review').count(), 0);
      assert.equal((await page.locator('#link-status').textContent()).trim(), 'linked to nexus');
      if (scenario === 'editing' || scenario === 'properties') {
        await page.locator('#canvas .block[data-id="b1"]').click();
        if (scenario === 'properties') await page.locator('#toggle-properties').click();
        else await page.locator('#floating-toolbar').waitFor({ state:'visible' });
      } else if (scenario === 'external') {
        const before = await page.locator('#canvas').textContent();
        await page.evaluate(ops => window.__mock.externalEdit(ops, 's1'), modelEdit);
        await page.locator('#canvas .block.external-flash').first().waitFor();
        assert.deepEqual(await page.locator('#canvas .block.external-flash').evaluateAll(nodes => nodes.map(n => n.dataset.id).sort()), ['b1', 'subtitle']);
        assert.match(await page.locator('#status').textContent(), /changed on disk/);
        assert.match(await page.locator('#canvas').textContent(), /Room to do your/);
        await page.waitForTimeout(120);
        await page.screenshot({ path:path.join(out, `external-${colorScheme}.png`) }); console.log(path.join(out, `external-${colorScheme}.png`));
        // Cmd+Z takes the whole model edit back in one step; redo brings it back.
        await page.locator('#undo').click(); await page.waitForFunction(text => document.querySelector('#canvas').textContent === text, before);
        await page.locator('#redo').click(); await page.waitForFunction(() => document.querySelector('#canvas').textContent.includes('Room to do your'));
        await page.close(); continue;
      } else if (scenario === 'thread' || scenario === 'preview') {
        await page.locator('#comment-mode').click(); await page.locator('#pins .pin').first().click(); await page.locator('#thread-popover .edit-card').waitFor();
        assert.equal(await page.getByRole('button', { name:'Send to nexus' }).isEnabled(), true);
        if (scenario === 'preview') {
          await page.getByRole('button', { name:'Preview', exact:true }).click(); await page.locator('#canvas .ghost-changed').waitFor();
          const before = await page.locator('#canvas').textContent(); await page.keyboard.press('b'); assert.notEqual(await page.locator('#canvas').textContent(), before); await page.keyboard.press('b');
        }
      } else if (scenario === 'comments') {
        await page.locator('#toggle-comments').click();
        assert.equal((await page.locator('#comment-count').textContent()).trim(), '3');
        assert.equal(await page.locator('#comments-list .comment-row').count(), 3);
      }
      await page.waitForTimeout(180);
      const output = path.join(out, `${scenario}-${colorScheme}.png`); await page.screenshot({ path:output }); console.log(output);
      if (scenario === 'preview') {
        await page.locator('#preview-apply').click(); await page.locator('#preview-bar').waitFor({ state:'hidden' }); assert.match(await page.locator('#canvas').textContent(), /Find your focus/);
        await page.locator('#undo').click(); await page.waitForFunction(() => document.querySelector('#canvas').textContent.includes('Less noise. More room'));
      }
      if (scenario === 'thread') {
        // A reply goes to nexus over the link; resolve and reopen stay local.
        await page.locator('#thread-popover textarea').fill('Yes, keep the quiet tone.'); await page.getByRole('button', { name:'Send to nexus' }).click();
        await page.waitForFunction(() => window.__mock.sent.some(m => m.type === 'Reply'));
        await page.getByRole('button', { name:'Resolve thread', exact:true }).click(); await page.getByRole('button', { name:'Reopen thread', exact:true }).waitFor(); await page.getByRole('button', { name:'Reopen thread', exact:true }).click();
        assert.equal(await page.evaluate(() => window.__mock.sent.filter(m => m.type === 'Comment').length), 0);
        // Not linked: the same button is disabled and says why.
        await page.evaluate(() => window.__mock.emitCurrent({ linked:false }));
        await page.waitForFunction(() => document.querySelector('#link-status').textContent === 'not linked');
        assert.equal(await page.getByRole('button', { name:'Send to nexus' }).isDisabled(), true);
        await page.locator('#thread-popover .hint', { hasText:'/studio' }).waitFor();
      }
      if (scenario === 'editing') {
        // Selecting and switching slides tells nexus what is on screen, debounced.
        await page.locator('#strip .slide-item').nth(1).click(); await page.waitForFunction(() => window.__mock.sent.some(m => m.type === 'Focus' && m.slide === 's2'));
      }
      await page.close();
    }
  }
  assert.deepEqual(errors, []); console.log('12 screenshots · light + dark · external edit is one undo step, suggested edit preview/apply/undo, link-gated Send, focus reporting');
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
