// Run: node crates/studio/ui-dev/toolbar-shot.mjs
// Dense-card toolbar geometry, real keyboard/gesture edits, and before/after shots.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..'), ui = path.resolve(here, '../ui');
const installed = [path.join(root, 'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing read-only Playwright installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/d1-comments.json'), 'utf8'));
fixture.threads = []; fixture.deck.title = 'Research / source library';
fixture.deck.theme.colors = { bg:'#faf9f6', fg:'#172943', accent:'#006fc9', muted:'#67758b' };
const children = [];
for (let row = 0; row < 3; row++) for (let col = 0; col < 3; col++) {
  const key = `${row}-${col}`, x = 64 + col * 600, y = 64 + row * 320;
  children.push(
    { id:`card-${key}`, kind:'shape', name:`Card ${key}`, shape:'rect', fill:'#ffffff', stroke:{color:'muted',width:1}, radius:12, position:{x,y}, sizing:{w:560,h:280} },
    { id:`title-${key}`, kind:'text', name:`Title ${key}`, text:['TypeSafe AI docs · Jev','Pomodoro','OpenAI Decisions API','Luna vs Jev results'][((row * 3) + col) % 4], style:'body', font:{size:30,weight:700}, position:{x:x+24,y:y+24}, sizing:{w:400,h:46} },
    { id:`body-${key}`, kind:'text', name:`Summary ${key}`, text:'Evidence, connections and ideas.\nKeep the useful sources close.', style:'body', font:{size:26}, position:{x:x+24,y:y+96}, sizing:{w:500,h:88} },
    { id:`url-${key}`, kind:'text', name:`URL ${key}`, text:'github.com/research/source-notes', style:'code', font:{size:22,color:'muted'}, position:{x:x+24,y:y+220}, sizing:{w:500,h:36} },
  );
}
fixture.deck.slides = [{id:'s1',layout:{mode:'none'},children,notes:''}];
const baseline = new Map(['app.js','canvas.js','panel.js','index.html','studio.css'].map(name => [name, execFileSync('git', ['show', `HEAD:crates/studio/ui/${name}`], {cwd:root})]));
const mock = fs.readFileSync(path.join(here, 'd1b-mock.js'), 'utf8'), out = path.join(here, 'shots');
fs.mkdirSync(out, {recursive:true});
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost'), before = url.pathname.startsWith('/before/');
  const name = url.pathname.replace(/^\/(before|after)\//, '').replace(/^\//, '') || 'index.html', target = path.resolve(ui, name);
  if (!target.startsWith(ui + path.sep) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, {'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css'})[path.extname(target)] || 'application/octet-stream', 'Content-Security-Policy':"default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'none'; object-src 'none'"});
  const bytes = before && baseline.has(name) ? baseline.get(name) : fs.readFileSync(target);
  res.end(name === 'index.html' ? bytes.toString().replace(/(href|src)="\//g, `$1="/${before ? 'before' : 'after'}/`) : bytes);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch(), errors = [], outputs = [], geometry = [];
let page, selectedId = 'title-1-1';
const toolbar = () => page.locator('#floating-toolbar');
const selected = () => page.locator(`#canvas [data-id="${selectedId}"]`);
const settle = () => page.waitForTimeout(90);
const data = () => page.evaluate(id => window.__d1.node(id), selectedId);
async function fresh(width, theme, mode = 'after') {
  selectedId = 'title-1-1';
  await page?.close(); page = await browser.newPage({viewport:{width,height:900},deviceScaleFactor:1,reducedMotion:'reduce',colorScheme:theme === 'default' ? 'light' : 'dark'});
  page.on('pageerror', e => errors.push(e.stack));
  page.on('request', r => { if (!r.url().startsWith(`http://127.0.0.1:${server.address().port}/`)) errors.push(`Unexpected request ${r.url()}`); });
  await page.addInitScript(value => window.__studioFixture = value, fixture); await page.addInitScript({content:mock});
  await page.goto(`http://127.0.0.1:${server.address().port}/${mode}/`); await selected().waitFor();
  const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${theme}.json`), 'utf8'));
  await page.evaluate(colors => window.studioAsyncEvent({event:'Theme',colors}), colors);
  await page.locator(`[data-layer="${selectedId}"] .layer-name`).click(); await settle();
  assert.equal(await page.locator('html').evaluate(n => getComputedStyle(n).getPropertyValue('--bg')), colors.bg);
}
async function frame(position) {
  const b = await selected().boundingBox(), s = await page.locator('#stage').boundingBox();
  const x = position === 'right' ? s.x + s.width - b.width / 2 - 16 : s.x + s.width / 2;
  const y = position === 'top' ? s.y + b.height / 2 + 12 : s.y + s.height / 2;
  await page.locator('#stage').dispatchEvent('wheel', {deltaX:b.x + b.width / 2 - x,deltaY:b.y + b.height / 2 - y,deltaMode:0}); await settle();
}
const overlap = (a,b) => a.x < b.x+b.width && a.x+a.width > b.x && a.y < b.y+b.height && a.y+a.height > b.y;
async function checkGeometry(label) {
  await toolbar().waitFor({state:'visible'});
  const t = await toolbar().boundingBox(), b = await selected().boundingBox(), s = await page.locator('#stage').boundingBox();
  assert.ok(t.height <= 34 && t.height === 32, `${label}: one 32px row`);
  assert.ok(!overlap(t,b), `${label}: no selection overlap`);
  assert.ok(t.x >= s.x && t.x+t.width <= s.x+s.width+.1 && t.y >= s.y && t.y+t.height <= s.y+s.height+.1, `${label}: inside visible stage`);
  const centeredLeft = b.x + (b.width - t.width) / 2;
  if (centeredLeft >= s.x + 8 && centeredLeft + t.width <= s.x + s.width - 8) assert.ok(Math.abs(t.x + t.width / 2 - b.x - b.width / 2) <= 4, `${label}: toolbar centered on selection when clamping is unnecessary`);
  else assert.ok(Math.abs(t.x - (centeredLeft < s.x + 8 ? s.x + 8 : s.x + s.width - 8 - t.width)) <= 4, `${label}: horizontal shift stops at the stage edge`);
  for (const handle of await page.locator('.resize-handle').all()) assert.ok(!overlap(t,await handle.boundingBox()), `${label}: no handle overlap`);
  const controls = await toolbar().locator('button,input').evaluateAll(nodes => nodes.filter(n => n.getClientRects().length).map(n => ({height:n.getBoundingClientRect().height,y:n.getBoundingClientRect().y})));
  assert.ok(controls.every(c => c.height <= 26 && c.y >= t.y && c.y+c.height <= t.y+t.height), `${label}: controls ≤26px, same row`);
  const size = await toolbar().getByLabel('Text size in px',{exact:true}).boundingBox();
  assert.ok(size.width >= 44 && size.width <= 52, `${label}: compact numeric field`);
  geometry.push({label,width:t.width,height:t.height,side:t.y < b.y ? 'above' : 'below'});
  return t;
}
async function shot(name) { const file = path.join(out,`${name}.png`); await page.screenshot({path:file}); outputs.push(file); }
async function keyboardMenu(label, name) {
  const trigger = toolbar().getByRole('button',{name:label,exact:true}); await trigger.focus(); await page.keyboard.press('Enter');
  await page.locator(`[data-toolbar-menu="${name}"]`).waitFor(); await page.keyboard.press('End'); await page.keyboard.press('Home'); await page.keyboard.press('Escape');
  assert.equal(await page.locator('[data-toolbar-menu]').count(),0); assert.ok(await trigger.evaluate(n => n === document.activeElement), 'Escape restores trigger focus without clearing selection');
}
try {
  for (const theme of ['default','light']) {
    await fresh(1150,theme,'before'); await shot(`toolbar-before-1150-${theme}`);
    for (const width of [1150,1280,1440]) {
      await fresh(width,theme);
      for (const zoom of ['fit',.5,2]) {
        if (zoom === 'fit') await page.locator('#zoom-fit').click();
        else {
          await page.locator('#canvas').focus(); await page.keyboard.press('Shift+Digit0');
          for (let i = 0; i < Math.round(Math.abs(zoom-1)*10); i++) await page.locator(zoom < 1 ? '#zoom-out' : '#zoom-in').click();
        }
        for (const position of ['middle','top','right']) {
          // Fit shots select actual slide locations without panning the fitted slide.
          selectedId = zoom === 'fit' && position === 'top' ? 'title-0-1' : zoom === 'fit' && position === 'right' ? 'title-1-2' : 'title-1-1';
          await page.locator(`[data-layer="${selectedId}"] .layer-name`).click(); await settle();
          if (zoom !== 'fit') await frame(position);
          const label = `${width}-${theme}-${position}-${zoom}`;
          if (zoom === 'fit') {
            assert.equal(await page.locator('#zoom-fit').textContent(),'Fit');
            const slide = await page.locator('#canvas').boundingBox(), stage = await page.locator('#stage').boundingBox();
            assert.ok(slide.x >= stage.x && slide.x + slide.width <= stage.x + stage.width + .1 && slide.y >= stage.y && slide.y + slide.height <= stage.y + stage.height + .1, `${label}: Fit keeps the entire slide inside the stage: ${JSON.stringify({slide,stage})}`);
          }
          const t = await checkGeometry(label); if (zoom !== 'fit' && position === 'top') assert.ok(t.y > (await selected().boundingBox()).y, 'no room above: place below');
          await shot(`toolbar-after-review-${label}`);
        }
      }
    }
    await fresh(1150,theme); await frame('middle');
    for (const [label,name] of [['Selection text style','Text styles'],['Text size presets','Text size presets'],['Text alignment','Text alignment'],['Selection color','Selection color'],['Selection actions','Selection actions']]) await keyboardMenu(label,name);
    const input = toolbar().getByLabel('Text size in px',{exact:true});
    await input.fill('24*2'); await input.press('Enter'); await settle(); assert.equal((await data()).font.size,48);
    await input.press('ArrowUp'); await settle(); assert.equal((await data()).font.size,49);
    await input.press('Shift+ArrowDown'); await settle(); assert.equal((await data()).font.size,39);
    await toolbar().getByRole('button',{name:'Text size presets',exact:true}).focus(); await page.keyboard.press('Enter'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Enter'); await settle(); assert.equal((await data()).font.size,14);
    await toolbar().getByRole('button',{name:'Bold',exact:true}).click(); await settle(); assert.equal((await data()).font.weight,400);
    assert.equal(await toolbar().getByRole('button',{name:'Bold',exact:true}).getAttribute('aria-pressed'),'false');
    await page.locator('#canvas').focus(); await page.keyboard.press('Control+b'); await settle(); assert.equal((await data()).font.weight,700);
    await toolbar().getByRole('button',{name:'Italic',exact:true}).click(); await settle(); assert.equal((await data()).font.italic,true);
    assert.equal(await toolbar().getByRole('button',{name:'Italic',exact:true}).getAttribute('aria-pressed'),'true');
    await toolbar().getByRole('button',{name:'Text alignment',exact:true}).click(); await page.getByRole('menuitemradio',{name:'Align center',exact:true}).click(); await settle(); assert.equal((await data()).font.align,'center');
    assert.equal(await toolbar().getByRole('button',{name:'Text alignment',exact:true}).locator('path').getAttribute('d'),'M3 5h18M6 10h12M3 15h18M6 20h12');
    await toolbar().getByRole('button',{name:'Selection text style',exact:true}).click(); await page.getByRole('menuitemradio',{name:'Body',exact:true}).click(); await settle(); assert.equal((await data()).style,'body'); assert.equal((await data()).font,undefined);
    await toolbar().getByRole('button',{name:'Selection color',exact:true}).click();
    assert.equal(await page.locator('.toolbar-swatches button').count(),4);
    await page.getByLabel('Hex color',{exact:true}).fill('#123abc'); await page.getByLabel('Hex color',{exact:true}).press('Enter'); await settle(); assert.equal((await data()).font.color,'#123abc');
    await toolbar().getByRole('button',{name:'Selection color',exact:true}).click(); assert.equal(await page.getByRole('button',{name:'#123abc',exact:true}).count(),1);
    await page.keyboard.press('Escape');
    // Tab follows the visible DOM order; menus close when focus leaves them.
    await input.focus(); await page.keyboard.press('Tab'); assert.equal(await page.evaluate(() => document.activeElement.getAttribute('aria-label')),'Text size presets');
    await page.keyboard.press('Enter'); await page.keyboard.press('Escape');
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.mouse.click(225,140); assert.equal(await page.locator('[data-toolbar-menu]').count(),0);
    // Scrub keeps its pointer capture while the bar is invisible; one Apply/undo.
    const caption = toolbar().locator('.scrub-label'), r = await caption.boundingBox();
    let before = await page.evaluate(() => ({depth:window.__d1.undoDepth,count:window.__d1.commands.filter(c => c.command === 'Apply').length}));
    const sizeBefore = Number(await input.inputValue());
    await page.mouse.move(r.x+r.width/2,r.y+r.height/2); await page.mouse.down(); await page.mouse.move(r.x+r.width/2+20,r.y+r.height/2,{steps:5});
    assert.ok(await toolbar().isHidden()); assert.equal(await page.evaluate(() => window.__d1.commands.filter(c => c.command === 'Apply').length),before.count);
    await page.mouse.up(); await settle(); assert.equal((await data()).font.size,sizeBefore+20); assert.equal(await page.evaluate(() => window.__d1.undoDepth),before.depth+1); assert.equal(await page.evaluate(() => window.__d1.commands.filter(c => c.command === 'Apply').length),before.count+1);
    await frame('middle');
    for (const resize of [false,true]) {
      const b = resize ? await page.locator('.handle-se').boundingBox() : await selected().boundingBox();
      await page.mouse.move(b.x+b.width/2,b.y+b.height/2); await page.mouse.down(); await page.mouse.move(b.x+b.width/2+20,b.y+b.height/2+10,{steps:3}); assert.ok(await toolbar().isHidden());
      await page.keyboard.press('Escape'); await page.mouse.up(); await settle(); await checkGeometry(`cancel-${resize}-${theme}`);
    }
    await selected().dblclick(); assert.ok(await toolbar().isHidden()); await page.keyboard.press('Escape'); await settle(); await checkGeometry(`edited-${theme}`);
    // Screen-space geometry follows scrolling and live resize without changing selection.
    await page.locator('#stage').dispatchEvent('wheel',{deltaX:31,deltaY:17}); await settle(); await checkGeometry(`scroll-${theme}`);
    const collapseOrder = ['order','duplicate','align','colors','style']; let last = 0;
    for (const width of [700,650,600,550,500,460,420]) {
      await page.setViewportSize({width,height:900}); await frame('middle'); await checkGeometry(`narrow-${width}-${theme}`);
      const hidden = await toolbar().locator('[data-toolbar-group]').evaluateAll(groups => groups.filter(n => n.hidden).map(n => n.dataset.toolbarGroup));
      assert.deepEqual(new Set(hidden),new Set(collapseOrder.slice(0,hidden.length)), 'collapse follows the specified priority'); assert.ok(hidden.length >= last); last = hidden.length;
    }
    assert.equal(last,5,'all optional groups move to overflow at minimum width');
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).focus(); await page.keyboard.press('Enter');
    assert.deepEqual(await page.locator('[data-toolbar-menu] > button').allTextContents(), ['Text style','Text alignment','Selection color','Bring forward⌘/Ctrl+]','Send back⌘/Ctrl+[','Duplicate⌘/Ctrl+D','Delete⌫ / Delete','Comment on selectionC']);
    await shot(`toolbar-overflow-420-${theme}`);
    await page.getByRole('menuitem',{name:'Text alignment',exact:true}).click(); await page.getByRole('menuitemradio',{name:'Align right',exact:true}).click(); await settle(); assert.equal((await data()).font.align,'right');
    await page.setViewportSize({width:1150,height:900}); await frame('middle');
    // Non-typographic kinds omit typography; code/table keep size and emphasis.
    for (const id of ['card-1-1','url-1-1']) {
      await page.locator(`[data-layer="${id}"] .layer-name`).click(); await settle();
      assert.equal(await toolbar().locator('[data-toolbar-group=size]').count(),id.startsWith('card') ? 0 : 1);
      assert.equal(await toolbar().locator('[data-toolbar-group=emphasis]').count(),id.startsWith('card') ? 0 : 1);
    }
    await fresh(1150,theme);
    const index = () => page.evaluate(id => window.__d1.deck.slides[0].children.findIndex(n => n.id === id),selectedId);
    const originalIndex = await index();
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.getByRole('menuitem',{name:'Bring forward',exact:true}).click(); await settle(); assert.equal(await index(),originalIndex+1);
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.getByRole('menuitem',{name:'Send back',exact:true}).click(); await settle(); assert.equal(await index(),originalIndex);
    await toolbar().getByRole('button',{name:'Bold',exact:true}).focus(); await page.keyboard.press('Control+b'); await settle(); assert.equal((await data()).font.weight,400,'advertised shortcuts also work with toolbar focus');
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.getByRole('menuitem',{name:'Duplicate',exact:true}).click(); await settle();
    assert.equal(await page.evaluate(() => window.__d1.deck.slides[0].children.length),children.length+1);
    assert.equal(await page.locator('#canvas .block.selected').textContent(),fixture.deck.slides[0].children.find(n => n.id === selectedId).text);
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.getByRole('menuitem',{name:'Delete',exact:true}).click(); await settle();
    assert.equal(await page.evaluate(() => window.__d1.deck.slides[0].children.length),children.length); assert.ok(await toolbar().isHidden());
    await page.locator(`[data-layer="${selectedId}"] .layer-name`).click();
    await toolbar().getByRole('button',{name:'Selection actions',exact:true}).click(); await page.getByRole('menuitem',{name:'Comment on selection',exact:true}).click();
    await page.locator('#thread-popover textarea').waitFor(); assert.ok(await toolbar().isHidden());
    await page.getByRole('button',{name:'Close thread',exact:true}).click();
    await page.locator(`[data-layer="${selectedId}"] .layer-name`).click(); await page.locator('[data-layer="card-1-1"] .layer-name').click({modifiers:['Shift']}); await settle();
    const mixedInput = toolbar().getByLabel('Text size in px',{exact:true}); await mixedInput.fill('36'); await mixedInput.press('Enter'); await settle();
    assert.equal((await data()).font.size,36); assert.equal(await page.evaluate(() => window.__d1.node('card-1-1').font),undefined,'mixed typography does not write font fields to shapes');
    await toolbar().getByRole('button',{name:'Selection color',exact:true}).click(); await page.getByRole('button',{name:'accent color',exact:true}).click(); await settle();
    assert.equal((await data()).font.color,'accent'); assert.equal(await page.evaluate(() => window.__d1.node('card-1-1').fill),'accent');
  }
  assert.deepEqual(errors,[]);
  fs.writeFileSync(path.join(out,'toolbar-geometry.json'),`${JSON.stringify(geometry,null,2)}\n`);
  console.log(`PASS: ${geometry.length} geometry assertions; 1150/1280/1440px; both terminal themes; middle/top/right; fit/50%/200%; 32px row; ≤26px controls; 48px input; entire slide inside stage at Fit; toolbar center within 4px unless clamped; stage/selection/handle bounds; ordered collapse; overflow; keyboard/presets/styles/alignment/colors/recent/hex; active toggles; one-undo scrub; drag/resize/edit hiding; scroll/resize; layer order/duplicate/delete/comment; mixed selection.`);
  console.log(outputs.join('\n'));
} catch (error) { await page?.screenshot({path:path.join(out,'toolbar-failure.png')}); console.error(errors); throw error; }
finally { await page?.close(); await browser.close(); await new Promise(resolve => server.close(resolve)); }
