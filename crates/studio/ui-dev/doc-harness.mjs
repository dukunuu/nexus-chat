// Shared by doc-shot.mjs and doc-e2e.mjs: a static server for the UI, Chromium from the
// existing web/node_modules playwright-core, and the mock backend in doc-mock.js.
import fs from 'node:fs';
import http from 'node:http';
import zlib from 'node:zlib';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const candidates = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'];
const installed = candidates.find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const ui = path.resolve(here, '../ui'), out = path.join(here, 'shots'); fs.mkdirSync(out, { recursive: true });
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/doc-sample.json'), 'utf8'));
const mock = fs.readFileSync(path.join(here, 'doc-mock.js'), 'utf8');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.png': 'image/png' };

function png(width, height, pixel) {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) { raw[y * (width * 4 + 1)] = 0; for (let x = 0; x < width; x++) raw.set(pixel(x, y), y * (width * 4 + 1) + 1 + x * 4); }
  const crcTable = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = buf => { let c = 0xffffffff; for (const b of buf) c = crcTable[(c ^ b) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (type, data) => { const len = Buffer.alloc(4); len.writeUInt32BE(data.length); const body = Buffer.concat([Buffer.from(type), data]); const sum = Buffer.alloc(4); sum.writeUInt32BE(crc(body)); return Buffer.concat([len, body, sum]); };
  const header = Buffer.alloc(13); header.writeUInt32BE(width, 0); header.writeUInt32BE(height, 4); header[8] = 8; header[9] = 6;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', header), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
const chartPng = png(640, 280, (x, y) => { const bar = Math.floor(x / 80), h = 60 + (bar * 37) % 170; return y > 280 - h && x % 80 > 12 && x % 80 < 68 ? [174, 89, 62, 255] : [236, 232, 224, 255]; });
const pagePng = png(420, 594, (x, y) => (y > 60 && y < 520 && x > 50 && x < 370 && y % 22 < 8 && !(y > 130 && y < 170) ? [190, 190, 184, 255] : [255, 255, 255, 255]));

/** A little slide (a palette per seed) or page, standing in for a library thumbnail. */
const palettes = [[[22, 26, 38], [122, 162, 247], [232, 230, 227]], [[250, 246, 238], [193, 84, 55], [48, 40, 36]], [[18, 52, 48], [96, 208, 170], [236, 246, 242]], [[36, 28, 58], [214, 120, 232], [240, 232, 250]], [[238, 241, 246], [38, 99, 235], [24, 32, 48]]];
function thumbnail(kind, seed) {
  const [bg, accent, ink] = palettes[(seed - 1) % palettes.length];
  if (kind === 'doc') {
    return png(420, 594, (x, y) => {
      const on = (a, b, c, d) => x >= a && x < b && y >= c && y < d;
      if (on(56, 230, 66, 84)) return [...ink, 255];
      if (on(56, 130, 96, 104)) return [...accent, 255];
      if (y > 140 && y < 470 && x > 56 && x < 364 && y % 20 < 7 && !on(250 + (y % 3) * 40, 364, 0, 600)) return [190, 190, 184, 255];
      if (on(56, 364, 490, 560)) return [...accent.map(v => Math.round(v * 0.25 + 191)), 255];
      return [255, 255, 255, 255];
    });
  }
  return png(640, 360, (x, y) => {
    const on = (a, b, c, d) => x >= a && x < b && y >= c && y < d;
    if (on(48, 330, 128, 168)) return [...ink, 255];
    if (on(48, 250, 184, 198)) return [...ink.map((v, i) => Math.round(v * 0.55 + bg[i] * 0.45)), 255];
    if (on(48, 120, 96, 104)) return [...accent, 255];
    if (on(380, 592, 64, 296) && (x + y) % 2 === 0) return [...accent, 255];
    if (on(380, 592, 64, 296)) return [...accent.map((v, i) => Math.round(v * 0.82 + bg[i] * 0.18)), 255];
    return [...bg, 255];
  });
}
const server = http.createServer((req, res) => {
  const name = decodeURIComponent(req.url.split('?')[0]);
  const lib = /^\/library\/(deck|doc)-(\d+)\.png$/.exec(name);
  if (lib) { res.writeHead(200, { 'Content-Type': 'image/png' }); res.end(thumbnail(lib[1], Number(lib[2]))); return; }
  if (name.startsWith('/assets/')) { res.writeHead(200, { 'Content-Type': 'image/png' }); res.end(chartPng); return; }
  if (name.startsWith('/pages/')) { res.writeHead(200, { 'Content-Type': 'image/png' }); res.end(pagePng); return; }
  const target = path.resolve(ui, name === '/' ? 'index.html' : `.${name}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target) || !fs.statSync(target).isFile()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type': types[path.extname(target)] || 'application/octet-stream', 'Content-Security-Policy': "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'" });
  fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}/`;
export { base };
export const browser = await chromium.launch();
export const errors = [];

export async function open(scheme = 'dark', viewport = { width: 1440, height: 900 }, { fixture: override, reducedMotion = 'reduce', scripts = [], ...extra } = {}) {
  const page = await browser.newPage({ viewport, deviceScaleFactor: 1, colorScheme: scheme, reducedMotion, ...extra });
  page.setDefaultTimeout(6000);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${scheme === 'dark' ? 'default' : 'light'}.json`), 'utf8'));
  await page.addInitScript(([f, c]) => { window.__docFixture = f; window.__docColors = c; }, [override ?? fixture, colors]);
  await page.addInitScript({ content: mock });
  for (const content of scripts) await page.addInitScript({ content });
  await page.goto(base);
  await page.waitForSelector('#doc-surface .blk');
  await page.waitForTimeout(150);
  return page;
}
export const shot = async (page, name) => { const file = path.join(out, `${name}.png`); await page.screenshot({ path: file }); console.log(file); return file; };

export async function finish() { await browser.close(); server.close(); if (errors.length) { console.error(errors.join('\n')); process.exitCode = 1; } }
export const libraryMock = fs.readFileSync(path.join(here, 'library-mock.js'), 'utf8');
export { out, fixture };
