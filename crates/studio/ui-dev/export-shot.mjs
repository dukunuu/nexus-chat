// Run: node crates/studio/ui-dev/export-shot.mjs
// Export-menu assertions, 1440x900 terminal palettes, and offline file:// playback.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath, pathToFileURL } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const installed = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing read-only Playwright installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const ui = path.resolve(here, '../ui'), out = path.join(here, 'shots');
const deck = JSON.parse(fs.readFileSync(path.join(root, 'crates/core/tests/fixtures/deck/export-motion.deck.json'), 'utf8'));
const defaults = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/d1-comments.json'), 'utf8')).deck.theme;
deck.theme = defaults;
const mock = `(() => {
  const commands = []; window.__exportCommands = commands;
  window.ipc = { postMessage(source) {
    const command = JSON.parse(source); commands.push(command);
    const event = command.command === 'Export' ? { event:'Exported', path:'/space/files/exports/export-motion.pptx', warnings:['Chart exported as a PNG, not a native chart.', 'Smart transition becomes fade.', 'PowerPoint groups do not clip children.'] }
      : command.command === 'Reveal' ? { event:'Revealed' }
      : { event:'Deck', deck:window.__deck, hashes:{}, images:{}, threads:[], linked:false, revision:'mock', changed:[] };
    setTimeout(() => window.studioEvent(event), 5);
  }};
})();`;
const server = http.createServer((req, res) => {
  const target = path.resolve(ui, req.url === '/' ? 'index.html' : `.${req.url}`);
  if (!target.startsWith(ui + path.sep) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type': ({ '.html':'text/html', '.js':'text/javascript', '.css':'text/css' })[path.extname(target)] || 'application/octet-stream' });
  fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch(), errors = [], outputs = [];
try {
  for (const palette of ['default', 'light']) {
    const page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1 });
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(value => window.__deck = value, deck);
    await page.addInitScript({ content:mock });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.locator('#canvas .block').first().waitFor();
    const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${palette}.json`), 'utf8'));
    await page.evaluate(colors => window.studioAsyncEvent({ event:'Theme', colors }), colors);
    await page.locator('#export').focus(); await page.keyboard.press('Enter');
    assert.equal(await page.getByRole('menuitem').count(), 6);
    assert.equal(await page.locator('#export').getAttribute('aria-expanded'), 'true');
    await page.keyboard.press('End'); assert.equal(await page.locator('#export-to').evaluate(e => e === document.activeElement), true);
    await page.keyboard.press('Home');
    assert.equal(await page.getByRole('menuitem', { name:'PowerPoint (.pptx)', exact:true }).evaluate(e => e === document.activeElement), true);
    const shot = async name => { const file = path.join(out, `export-${name}-${palette}.png`); await page.screenshot({ path:file }); outputs.push(file); };
    await shot('menu');
    await page.keyboard.press('Enter');
    await page.getByRole('button', { name:'Reveal', exact:true }).waitFor();
    assert.match(await page.locator('#status').textContent(), /Exported export-motion.pptx/);
    assert.match(await page.locator('#status').textContent(), /3 approximations · Details/);
    assert.deepEqual(await page.evaluate(() => window.__exportCommands.find(c => c.command === 'Export')), { command:'Export', format:'pptx' });
    await shot('toast');
    await page.getByRole('button', { name:'Reveal', exact:true }).click();
    await page.waitForFunction(() => window.__exportCommands.some(c => c.command === 'Reveal'));
    await page.getByRole('button', { name:'3 approximations · Details', exact:true }).click();
    assert.equal(await page.locator('#export-warnings li').count(), 3);
    await shot('details');
    await page.getByRole('button', { name:'Close', exact:true }).click();
    await page.locator('#export').click(); await page.keyboard.press('Escape');
    assert.equal(await page.locator('#export').evaluate(e => e === document.activeElement), true);
    await page.locator('#export').click(); await page.locator('#export-to').click();
    await page.getByLabel('Destination path').fill('/tmp/custom.pptx');
    await page.locator('#path-confirm').click();
    await page.waitForFunction(() => window.__exportCommands.some(c => c.command === 'Export' && c.path === '/tmp/custom.pptx'));
    await page.close();
  }
  // Real exported HTML must run directly from disk, without even a loopback server.
  for (const name of ['classic', 'auto-motion']) {
    const file = path.join(out, 'exports', `${name}.html`);
    assert.ok(fs.existsSync(file), 'Generate reviewer samples with NEXUS_EXPORT_OUT during scripts/check.sh first.');
    const page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1 });
    page.on('pageerror', error => errors.push(error.message));
    const network = [];
    page.on('request', request => { if (/^https?:/.test(request.url())) network.push(request.url()); });
    await page.goto(pathToFileURL(file).href);
    await page.locator('#present-stage .slide').waitFor().catch(error => { throw new Error(`${error.message}\nPage errors: ${errors.join('; ')}`); });
    assert.match(await page.locator('#present-counter').textContent(), /1 \/ 2/);
    assert.equal(await page.locator('#present-notes').textContent(), '');
    await page.keyboard.press('End');
    assert.equal(await page.locator('#present-counter').textContent(), '2 / 2');
    await page.keyboard.press('Home');
    if (name === 'auto-motion') {
      assert.equal(await page.locator('#present-stage [data-id="headline"]').evaluate(e => getComputedStyle(e).opacity), '0');
      await page.keyboard.press('ArrowRight');
      await page.waitForTimeout(1400);
      assert.equal(await page.locator('#present-stage [data-id="headline"]').evaluate(e => getComputedStyle(e).opacity), '1');
    }
    assert.deepEqual(network, []);
    const shot = path.join(out, `export-html-${name}.png`); await page.screenshot({ path:shot }); outputs.push(shot);
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log('Export harness: menu, keyboard, formats, toast, Reveal, warning details, custom path and offline playback passed.');
  console.log(outputs.join('\n'));
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
