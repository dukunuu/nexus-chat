// Run: node crates/studio/ui-dev/activity-shot.mjs
// Hermetic Activity lifecycle assertions and 1440x900 terminal-theme captures.
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
fixture.threads[0].messages = fixture.threads[0].messages.slice(0, 1);
fixture.deck.slides.push({ id:'s2', children:[], notes:'' });
const mock = fs.readFileSync(path.join(here, 'mock.js'), 'utf8');
const ui = path.resolve(here, '../ui'), out = path.join(here, 'shots');
fs.mkdirSync(out, { recursive:true });
const server = http.createServer((req, res) => {
  const target = path.resolve(ui, req.url === '/' ? 'index.html' : `.${req.url}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type':({ '.html':'text/html', '.js':'text/javascript', '.css':'text/css' })[path.extname(target)] || 'application/octet-stream' });
  fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch(), errors = [], outputs = [];
let page;
const activity = (state, detail, reason, thread = 't1') => page.evaluate(({ thread, state, detail, reason }) => window.__mock.activity(thread, state, detail, reason), { thread, state, detail, reason });
const text = () => page.locator('.thread-activity').textContent();
const shot = async (state, theme) => { const file = path.join(out, `activity-${state}-${theme}.png`); await page.screenshot({ path:file }); outputs.push(file); };
try {
  for (const theme of ['default', 'light']) {
    page = await browser.newPage({ viewport:{ width:1440, height:900 }, deviceScaleFactor:1 });
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', request => { if (!request.url().startsWith(`http://127.0.0.1:${server.address().port}/`)) errors.push(`Unexpected request: ${request.url()}`); });
    await page.addInitScript(value => window.__studioFixture = value, fixture);
    await page.addInitScript({ content:mock });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.locator('#pins .pin').first().waitFor();
    const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${theme}.json`), 'utf8'));
    await page.evaluate(colors => window.studioAsyncEvent({ event:'Theme', colors }), colors);
    await page.locator('#pins .pin').first().click();
    for (const [reason, expected] of [
      ['draft', 'Waiting — you have a draft in nexus'],
      ['streaming', 'Waiting — nexus is answering something else'],
      ['no_model', 'Waiting — no model selected'],
    ]) {
      await activity('queued', undefined, reason);
      assert.equal(await text(), expected);
      assert.equal(await page.locator('#link-status').textContent(), 'Linked · 1 waiting');
      assert.equal(await page.locator('#pins .activity-busy').count(), 1);
      assert.equal(await page.locator('#pins .activity-busy').evaluate(n => getComputedStyle(n, '::before').animationName), 'activity-spin');
    }
    await activity('queued', undefined, 'draft', 't2');
    assert.equal(await page.locator('#link-status').textContent(), 'Linked · 2 waiting');
    await activity('working');
    assert.equal(await text(), 'nexus is working…');
    assert.equal(await page.locator('#link-status').textContent(), 'nexus is working');
    assert.ok((await page.locator('#link-status').boundingBox()).height < 32, 'working pill stays on one line');
    assert.equal(await page.locator('#link-status').evaluate(n => getComputedStyle(n, '::before').animationName), 'activity-pulse');
    await shot('working', theme);
    await page.evaluate(() => window.__mock.activitySequence('t1', [
      { state:'progress', detail:'edited slide 1' },
      { state:'progress', detail:'replied in this thread' },
    ]));
    assert.deepEqual(await page.locator('.activity-progress').allTextContents(), ['edited slide 1', 'replied in this thread']);
    assert.equal(await page.locator('#pins .activity-busy').count(), 2);
    await activity('done', 'Which **audience** should this target? I can keep the `metrics` and simplify the story.');
    assert.match(await text(), /Which audience should this target\?/);
    assert.equal(await page.locator('.activity-detail strong').textContent(), 'audience');
    assert.equal(await page.locator('.activity-detail code').textContent(), 'metrics');
    assert.equal(await page.locator('.thread-activity button').count(), 0, 'no Undo for an answer-only turn');
    assert.equal(await page.locator('#pins .activity-done').count(), 1);
    assert.equal(await page.locator('#link-status').textContent(), 'Linked · 1 waiting');
    await shot('done-excerpt', theme);
    await page.waitForTimeout(4100);
    assert.equal(await page.locator('#pins .activity-done').count(), 0, 'check clears after four seconds');
    await activity('done', '<img src=x onerror=alert(1)> [unsafe](javascript:alert(1))');
    assert.equal(await page.locator('.activity-detail img,.activity-detail a').count(), 0);
    assert.match(await page.locator('.activity-detail').textContent(), /<img/);
    await activity('failed', 'Request timed out. Please try again.');
    assert.match(await text(), /FailedRequest timed out/);
    assert.equal(await page.locator('.thread-activity').evaluate(n => getComputedStyle(n).color), theme === 'default' ? 'rgb(205, 0, 0)' : 'rgb(200, 50, 66)');
    await shot('failed', theme);
    const sent = await page.evaluate(() => window.__mock.sent.filter(m => m.type === 'Reply').length);
    await page.getByRole('button', { name:'Send again', exact:true }).click();
    await page.waitForFunction(count => window.__mock.sent.filter(m => m.type === 'Reply').length === count + 1, sent);
    assert.equal(await page.evaluate(() => window.__mock.sent.findLast(m => m.type === 'Reply').text), fixture.threads[0].messages[0].text);
    await activity('stopped', 'Response stopped');
    assert.match(await text(), /StoppedResponse stoppedSend again/);
    await activity('working');
    await page.emulateMedia({ reducedMotion:'reduce' });
    assert.equal(await page.locator('.activity-dot').evaluate(n => getComputedStyle(n).animationName), 'none');
    assert.equal(await page.locator('#pins .activity-busy').first().evaluate(n => getComputedStyle(n, '::before').animationName), 'none');
    await page.emulateMedia({ reducedMotion:'no-preference' });
    const before = await page.locator('#canvas .block[data-id="note"]').textContent();
    await page.evaluate(() => window.__mock.externalEdit([{ op:'update_node', id:'note', fields:{ text:'A clearer supporting line.' } }]));
    await activity('done', 'edited slide 1');
    await page.getByRole('button', { name:'Undo', exact:true }).last().click();
    await page.waitForFunction(before => document.querySelector('#canvas .block[data-id="note"]').textContent === before, before);
    await page.getByRole('button', { name:'Close thread', exact:true }).click();
    await page.evaluate(() => window.studioAsyncEvent({ event:'Focus', slide:'s2' }));
    await activity('working');
    await activity('done', 'edited slide 1');
    assert.equal(await page.locator('#status').textContent(), 'nexus edited slide 1 · View');
    await page.locator('#status').getByRole('button', { name:'View', exact:true }).click();
    assert.match(await page.locator('#slide-label').textContent(), /Slide 1 of/);
    assert.match(await page.locator('#thread-popover').textContent(), /Comment 1/);
    await activity('future-state', 'ignore me');
    assert.doesNotMatch(await page.locator('#thread-popover').textContent(), /ignore me/);
    await page.close(); page = null;
  }
  assert.deepEqual(errors, []);
  console.log('PASS: all waiting reasons; working/progress/done/failed/stopped; pin ring and four-second check; pill priority/count; safe Markdown excerpt; retry resends; external-change Undo; finish toast View focuses and opens thread; reduced-motion static dots; unknown state ignored.');
  console.log(outputs.join('\n'));
} finally { await page?.close(); await browser.close(); await new Promise(resolve => server.close(resolve)); }
