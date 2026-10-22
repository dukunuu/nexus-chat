// Run: node crates/studio/ui-dev/rects.mjs
// Lays the parity decks (fixtures/parity-*.deck.json) out with the editor's own
// renderer (ui/render.js + studio.css, real Chromium flexbox) and writes every
// node's rectangle in slide px to fixtures/rects-<name>.json. The core test
// `taffy_matches_the_browser_rects_for_the_parity_decks` compares taffy's
// `deck::layout` against them (frames within 2 px, text-sized boxes within 6 px).
//
// Text sizes depend on the font. The decks name Helvetica Neue; the file records
// what Chromium actually used so the core test can skip text boxes on machines
// that lack it.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const candidates = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'];
const installed = candidates.find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const ui = path.resolve(here, '../ui'), fixtures = path.join(here, 'fixtures');
const page_html = '<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="/studio.css"><body style="overflow:visible"><div id="s" class="slide" style="width:1920px"></div><script type="module">import { renderSlide } from "/render.js"; window.__render = (slide, theme) => { renderSlide(document.querySelector("#s"), slide, theme, {}); };</script>';
const types = { '.html':'text/html', '.js':'text/javascript', '.css':'text/css' };
const server = http.createServer((req, res) => {
  const name = decodeURIComponent(req.url.split('?')[0]);
  if (name === '/') { res.writeHead(200, { 'Content-Type':'text/html' }); res.end(page_html); return; }
  const target = path.resolve(ui, `.${name}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type':types[path.extname(target)] || 'application/octet-stream' }); fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch();
const round = value => Math.round(value * 100) / 100;
try {
  for (const file of fs.readdirSync(fixtures).filter(f => /^parity-.*\.deck\.json$/.test(f)).sort()) {
    const name = file.replace(/^parity-/, '').replace(/\.deck\.json$/, '');
    const deck = JSON.parse(fs.readFileSync(path.join(fixtures, file), 'utf8'));
    const page = await browser.newPage({ viewport:{ width:2000, height:1200 }, deviceScaleFactor:1 });
    page.on('pageerror', error => { throw error; });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => typeof window.__render === 'function');
    const slides = {};
    let used = '';
    for (const slide of deck.slides) {
      const result = await page.evaluate(({ slide, theme }) => {
        window.__render(slide, theme);
        const container = document.querySelector('#s'), origin = container.getBoundingClientRect(), out = {};
        for (const node of container.querySelectorAll('.block')) {
          const box = node.getBoundingClientRect();
          // Hidden nodes have no box; the core reports a zero rectangle.
          out[node.dataset.id] = [box.left - origin.left, box.top - origin.top, box.width, box.height];
        }
        const sample = container.querySelector('.block-text');
        return { out, font: sample ? getComputedStyle(sample).fontFamily : '' };
      }, { slide, theme:deck.theme });
      slides[slide.id] = Object.fromEntries(Object.entries(result.out).map(([id, box]) => [id, box.map(round)]));
      used = used || result.font;
    }
    await page.close();
    const requested = deck.theme.fonts.body;
    fs.writeFileSync(path.join(fixtures, `rects-${name}.json`), `${JSON.stringify({ deck:file, generator:'crates/studio/ui-dev/rects.mjs (Chromium, ui/render.js)', font:requested, css_font:used, slides }, null, 1)}\n`);
    console.log(`rects-${name}.json · ${Object.values(slides).reduce((n, s) => n + Object.keys(s).length, 0)} nodes`);
  }
} finally { await browser.close(); server.close(); }
