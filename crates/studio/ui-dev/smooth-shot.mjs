// Run: node crates/studio/ui-dev/smooth-shot.mjs
// Hermetic 150 ms IPC / 4x CPU proof, including the HEAD (pre-fix) baseline.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..'), ui = path.resolve(here, '../ui');
const installed = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/workspace.json'), 'utf8'));
fixture.threads = [];
fixture.deck.slides = Array.from({ length:7 }, (_, s) => ({ id:`s${s}`, notes:'', children:Array.from({ length:s ? 14 : 16 }, (_, i) => ({
  id:`n${s}-${i}`, kind:'shape', name:`Layer ${s}-${i}`, shape:'rect', fill:'accent', position:i === 0 && s === 0 ? { x:200, y:300 } : { x:900 + i % 4 * 200, y:100 + Math.floor(i / 4) * 200 }, sizing:{ w:140, h:80 },
})) }));
const baseline = new Map(['app.js','canvas.js','strip.js','render.js'].map(name => [name, execFileSync('git', ['show', `HEAD:crates/studio/ui/${name}`], { cwd:root })]));
const mock = fs.readFileSync(path.join(here, 'mock.js'), 'utf8');
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost'), before = url.pathname.startsWith('/before/');
  const name = url.pathname.replace(/^\/(before|after)\//, '').replace(/^\//, '') || 'index.html', target = path.resolve(ui, name);
  if (!target.startsWith(ui + path.sep) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type':({ '.html':'text/html', '.js':'text/javascript', '.css':'text/css' })[path.extname(target)] || 'application/octet-stream' });
  const bytes = before && baseline.has(name) ? baseline.get(name) : fs.readFileSync(target);
  res.end(name === 'index.html' ? bytes.toString().replace(/(href|src)="\//g, `$1="/${before ? 'before' : 'after'}/`) : bytes);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch(), errors = [];
let page;
const selector = '#canvas [data-id="n0-0"]';
async function fresh(mode) {
  await page?.close();
  page = await browser.newPage({ viewport:{ width:1440, height:900 }, reducedMotion:'reduce' });
  page.on('pageerror', e => errors.push(e.message));
  page.on('request', r => { if (!r.url().startsWith(`http://127.0.0.1:${server.address().port}/`)) errors.push(`Unexpected request ${r.url()}`); });
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Emulation.setCPUThrottlingRate', { rate:4 });
  await page.addInitScript(value => { window.__studioFixture = value; window.__studioMockOptions = { latency:150 }; }, fixture);
  await page.addInitScript({ content:mock });
  await page.goto(`http://127.0.0.1:${server.address().port}/${mode}/`);
  await page.waitForFunction(() => document.querySelectorAll('#strip .slide-item').length === 7);
  await page.locator(selector).click();
  await page.waitForTimeout(400); // Settle the initial focus/list-font commands.
}
async function drag(reject = false, resize = false) {
  const block = await page.locator(selector).boundingBox();
  const handle = resize ? await page.locator('#canvas .handle-se').boundingBox() : block;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle.x + handle.width / 2 + 53, handle.y + handle.height / 2 + 27, { steps:4 });
  await page.evaluate(({ reject, selector }) => {
    window.__mock.rejectNextApply = reject;
    const rect = () => { const b = document.querySelector(selector).getBoundingClientRect(); return { x:b.x, y:b.y, w:b.width, h:b.height }; };
    const p = window.__probe = { frames:[], replacements:{ rail:0, layers:0, canvas:0 }, drop:rect(), done:false };
    const observers = [];
    for (const [key, target] of [['rail','#strip'], ['layers','#layers'], ['canvas','#canvas']]) {
      const observer = new MutationObserver(records => { for (const r of records) for (const n of r.removedNodes) if (key !== 'canvas' || n.classList?.contains('slide-root')) p.replacements[key]++; });
      observer.observe(document.querySelector(target), { childList:true, subtree:true }); observers.push(observer);
    }
    const original = window.studioEvent;
    window.studioEvent = event => { original(event); if (event.event === 'Deck' || event.event === 'Error') p.reply = performance.now(); };
    document.querySelector('#canvas').addEventListener('pointerup', () => {
      p.start = performance.now(); let afterReply = 0;
      const sample = () => {
        const now = performance.now(); p.frames.push({ t:now - p.start, ...rect(), replied:Boolean(p.reply) });
        if (p.reply && ++afterReply >= 3) {
          observers.forEach(o => o.disconnect()); window.studioEvent = original; p.done = true;
        } else requestAnimationFrame(sample);
      };
      requestAnimationFrame(sample);
    }, { once:true, capture:true });
  }, { reject, selector });
  await page.mouse.up();
  await page.waitForFunction(() => window.__probe.done);
  return { ...(await page.evaluate(() => window.__probe)), startRect:{ x:block.x, y:block.y, w:block.width, h:block.height } };
}
const near = (a, b) => ['x','y','w','h'].every(k => Math.abs(a[k] - b[k]) < 1);
const paintTime = p => { const i = p.frames.findIndex(f => near(f, p.drop)); return i < 0 ? null : p.frames[Math.min(i + 1, p.frames.length - 1)].t; };
try {
  await fresh('before');
  const before = await drag();
  assert.ok(before.frames.some(f => !f.replied && near(f, before.startRect)), 'baseline reproduces visible snap-back while waiting');
  await fresh('after');
  const after = await drag();
  assert.ok(near(after.frames[0], after.drop), 'drop position is present in the first animation frame');
  assert.ok(after.frames.every(f => near(f, after.drop)), 'no frame returns to the old position, including acknowledgement');
  assert.equal(after.replacements.canvas, 1, 'optimistic canvas only; matching acknowledgement does not redraw');
  assert.equal(after.replacements.rail, 0, 'position-only thumbnails update in place');
  assert.equal(after.replacements.layers, 0, 'position changes do not replace layer rows');
  const resized = await drag(false, true);
  assert.ok(resized.frames.every(f => near(f, resized.drop)), 'resize release never snaps back');
  assert.equal(resized.replacements.canvas, 1);
  const rejected = await drag(true);
  assert.ok(near(rejected.frames[0], rejected.drop), 'rejected edit was initially optimistic');
  assert.ok(rejected.frames.filter(f => !f.replied).every(f => near(f, rejected.drop)));
  assert.ok(near(rejected.frames.at(-1), rejected.startRect), 'rejected apply rolls back to the authoritative deck');
  assert.match(await page.locator('#status').textContent(), /Mock rejected apply/);
  // Three key repeats paint together, not one per 150 ms round trip.
  const queued = await page.evaluate(selector => {
    const el = () => document.querySelector(selector), start = el().getBoundingClientRect().x;
    window.__queueFrames = []; window.__queueDone = false;
    for (let i = 0; i < 3; i++) document.querySelector('#canvas').dispatchEvent(new KeyboardEvent('keydown', { key:'ArrowRight', bubbles:true }));
    const final = el().getBoundingClientRect().x, stop = performance.now() + 650;
    const sample = () => { window.__queueFrames.push(el().getBoundingClientRect().x); if (performance.now() < stop) requestAnimationFrame(sample); else window.__queueDone = true; }; requestAnimationFrame(sample);
    return { start, final, scale:document.querySelector('#canvas').getBoundingClientRect().width / 1920 };
  }, selector);
  assert.ok(Math.abs(queued.final - queued.start - 3 * queued.scale) < .1);
  await page.waitForFunction(() => window.__queueDone);
  assert.ok((await page.evaluate(() => window.__queueFrames)).every(x => Math.abs(x - queued.final) < .1), 'older replies never erase later key repeats');
  const barrier = await page.evaluate(() => {
    const x = window.__mock.deck.slides[0].children[0].position.x;
    document.querySelector('#undo').click();
    document.querySelector('#canvas').dispatchEvent(new KeyboardEvent('keydown', { key:'ArrowRight', bubbles:true }));
    return x;
  });
  await page.waitForTimeout(700);
  assert.equal(await page.evaluate(() => window.__mock.deck.slides[0].children[0].position.x), barrier, 'edits behind Undo resolve against its authoritative result');
  const incremental = await page.evaluate(async () => {
    const canvas = document.querySelector('#canvas .slide-root'), layers = document.querySelector('#layers').firstChild, panel = document.querySelector('#properties').firstChild;
    const thumbs = [...document.querySelectorAll('#strip .thumbnail')];
    window.__mock.externalEdit([{ op:'update_node', id:'n1-0', fields:{ fill:'fg' } }]);
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    return { canvas:canvas === document.querySelector('#canvas .slide-root'), layers:layers === document.querySelector('#layers').firstChild, panel:panel === document.querySelector('#properties').firstChild, thumbs:thumbs.map((t, i) => t === document.querySelectorAll('#strip .thumbnail')[i]) };
  });
  assert.deepEqual(incremental, { canvas:true, layers:true, panel:true, thumbs:[true,false,true,true,true,true,true] });
  const reordered = await page.evaluate(async () => {
    const thumbs = [...document.querySelectorAll('#strip .thumbnail')];
    window.__mock.externalEdit([{ op:'move_slide', id:'s1', to:6 }]);
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    return [...document.querySelectorAll('#strip .thumbnail')].map(t => thumbs.indexOf(t));
  });
  assert.deepEqual(reordered, [0,2,3,4,5,6,1], 'slide reordering preserves thumbnail DOM');
  // Unknown ops are all-or-nothing, merge patches replace scalar containers,
  // and tree edits/notes leave their input document untouched.
  const pure = await page.evaluate(async () => {
    const { applyLocal } = await import('./local.js');
    const deck = { slides:[{ id:'s', children:[{ id:'a', kind:'shape', position:'auto', sizing:{ w:10, h:20 } }, { id:'f', kind:'frame', layout:{ mode:'row' }, children:[] }] }] };
    const before = JSON.stringify(deck);
    const next = applyLocal(deck, [{ op:'update_node', id:'a', fields:{ position:{ x:12, y:34 }, sizing:{ w:null } } }, { op:'move_node', id:'a', parent:'f' }, { op:'insert_node', parent:'s', index:0, node:{ id:'b', kind:'text', text:'new' } }, { op:'set_notes', slide:'s', notes:'Speaker notes' }, { op:'delete_node', id:'b' }]);
    return { unchanged:before === JSON.stringify(deck), children:next.slides[0].children.map(n => n.id), moved:next.slides[0].children[0].children[0], notes:next.slides[0].notes, unknown:applyLocal(deck, [{ op:'set_notes', slide:'s', notes:'not applied' }, { op:'unknown' }]) };
  });
  assert.equal(pure.unchanged, true); assert.deepEqual(pure.children, ['f']); assert.deepEqual(pure.moved, { id:'a', kind:'shape', position:'auto', sizing:{ h:20 } }); assert.equal(pure.notes, 'Speaker notes'); assert.equal(pure.unknown, null);
  assert.deepEqual(errors, []);
  console.log(`PASS: 100 nodes / 7 slides; IPC 150 ms; Chromium CPU 4x. Pointer-up -> drop paint opportunity: before ${paintTime(before).toFixed(1)} ms, after ${paintTime(after).toFixed(1)} ms (first frame ${after.frames[0].t.toFixed(1)} ms).`);
  console.log(`PASS: sampled ${after.frames.length} drag frames; no snap-back; resize; rejection rollback; 3 queued nudges; matching ACK skipped. DOM removals: canvas ${after.replacements.canvas}, rail ${after.replacements.rail}, layers ${after.replacements.layers}. Background slide changed only its thumbnail; pure local ops/unknown fallback passed.`);
} finally { await page?.close(); await browser.close(); await new Promise(resolve => server.close(resolve)); }
