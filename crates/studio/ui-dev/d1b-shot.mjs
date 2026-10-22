// Run: node crates/studio/ui-dev/d1b-shot.mjs
// D1 editor interactions + twelve 1440×900 shots. No network or dependencies added.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here, '../../..');
const installed = [path.join(root,'web/node_modules/playwright-core/index.mjs'), '/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const fixture = JSON.parse(fs.readFileSync(path.join(here,'fixtures/d1-comments.json'),'utf8'));
fixture.threads = [];
fixture.deck.title = 'Research systems / auto-layout';
fixture.deck.theme.font_files = [];
const shape = (id,x,y,w=200,h=160,extra={}) => ({ id, name:`Evidence ${id.toUpperCase()}`, kind:'shape', shape:'rect', fill:'accent', radius:16, position:{x,y}, sizing:{w,h}, ...extra });
fixture.deck.slides[0].children = [
  { id:'heading',name:'Research systems',kind:'text',text:'Clarity, by design.',style:'heading',font:{size:72},position:{x:160,y:50},sizing:{w:1500,h:100} },
  { id:'row',name:'Evidence / row',kind:'frame',position:{x:160,y:210},sizing:{w:1600,h:260},fill:'muted',radius:24,layout:{mode:'row',gap:40,padding:[32,32,32,32],cross:'center'},children:[
    { id:'r1',name:'01 / Sources',kind:'text',text:'01 / Sources\nFind the signal.',style:'body',sizing:{w:420,h:180} },
    { id:'r2',name:'02 / Synthesis',kind:'text',text:'02 / Synthesis\nMake connections.',style:'body',sizing:{w:420,h:180} },
    { id:'r3',name:'03 / Insight',kind:'text',text:'03 / Insight\nKeep the thread.',style:'body',sizing:{w:420,h:180} },
  ]},
  shape('a',180,650), shape('b',580,700,200,120), shape('c',1020,650),
  { id:'destination',name:'Next / column',kind:'frame',position:{x:1400,y:700},sizing:{w:340,h:260},fill:'muted',radius:16,layout:{mode:'column',padding:[24,24,24,24],gap:12},children:[] },
  shape('locked',1440,500,220,130,{name:'Locked / reference',locked:true,fill:'fg',opacity:.3}),
  shape('hidden',1480,1000,150,60,{name:'Hidden / alternate',hidden:true}),
];
const ui = path.resolve(here,'../ui'), out = path.join(here,'shots'), mock = fs.readFileSync(path.join(here,'d1b-mock.js'),'utf8');
fs.mkdirSync(out,{recursive:true});
const types = {'.html':'text/html','.js':'text/javascript','.css':'text/css'};
const server = http.createServer((req,res) => {
  const name = decodeURIComponent(req.url.split('?')[0]), target = path.resolve(ui,name === '/' ? 'index.html' : `.${name}`);
  if (!target.startsWith(`${ui}${path.sep}`) || !fs.existsSync(target) || !fs.statSync(target).isFile()) { res.writeHead(404);res.end();return; }
  res.writeHead(200,{'Content-Type':types[path.extname(target)] || 'application/octet-stream','Content-Security-Policy':"default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"});fs.createReadStream(target).pipe(res);
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch(), errors = [], outputs = [];
let page;
const wait = () => page.waitForTimeout(65);
const node = id => page.locator(`#canvas [data-id="${id}"]`).first();
const layer = id => page.locator(`[data-layer="${id}"]`);
const data = id => page.evaluate(id => window.__d1.node(id),id);
const depth = () => page.evaluate(() => window.__d1.undoDepth);
const applyCount = () => page.evaluate(() => window.__d1.commands.filter(c => c.command === 'Apply').length);
const shot = async (name,theme) => { const file = path.join(out,`d1b-${name}-${theme}.png`); await page.screenshot({path:file}); outputs.push(file); };
const rect = async id => {
  const c = await page.locator('#canvas').boundingBox(), b = await node(id).boundingBox(), scale = c.width/1920;
  return {x:(b.x-c.x)/scale,y:(b.y-c.y)/scale,w:b.width/scale,h:b.height/scale};
};
const point = async (x,y) => { const c = await page.locator('#canvas').boundingBox();return {x:c.x+x*c.width/1920,y:c.y+y*c.width/1920}; };
const moveTo = async p => page.mouse.move(p.x,p.y,{steps:8});
async function select(ids) {
  for (let i=0;i<ids.length;i++) await layer(ids[i]).locator('.layer-name').click({modifiers:i ? ['Shift'] : []});
}
async function fresh(theme) {
  if (page) await page.close();
  page = await browser.newPage({viewport:{width:1440,height:900},deviceScaleFactor:1,colorScheme:theme === 'default' ? 'light' : 'dark',reducedMotion:'reduce'});
  page.on('pageerror',e => errors.push(e.message));
  page.on('request',req => { if (!req.url().startsWith(`http://127.0.0.1:${server.address().port}/`)) errors.push(`Unexpected request ${req.url()}`); });
  await page.addInitScript(value => window.__studioFixture=value,fixture);await page.addInitScript({content:mock});
  await page.goto(`http://127.0.0.1:${server.address().port}/`);await node('row').waitFor();
  const colors = JSON.parse(fs.readFileSync(path.join(here,`fixtures/theme-${theme}.json`),'utf8'));
  await page.evaluate(colors => window.studioAsyncEvent({event:'Theme',colors}),colors);
  await page.locator('#toggle-properties').click();await wait();
}
async function set(label,value) { await page.getByLabel(label,{exact:true}).fill(value);await page.getByLabel(label,{exact:true}).press('Enter');await page.getByLabel(label,{exact:true}).blur();await wait(); }
try {
  for (const theme of ['default','light']) {
    await fresh(theme);
    assert.deepEqual(await page.evaluate(async () => {
      const {renderNode}=await import('/render.js'), theme=window.__d1.deck.theme;
      return ['table','code'].map(kind=>{ const n=renderNode({id:`hidden-${kind}`,kind,hidden:true,rows:[['Hidden']],source:'Hidden',style:'body',font:{valign:'middle'}},theme);document.body.append(n);const r=n.getBoundingClientRect();n.remove();return [r.width,r.height]; });
    }),[[0,0],[0,0]],'hidden typography stays hidden even with vertical alignment');
    await select(['row']);
    await page.getByRole('button',{name:'Align bottom right',exact:true}).click();await wait();
    assert.deepEqual((({main,cross})=>({main,cross}))((await data('row')).layout),{main:'end',cross:'end'});
    await page.getByRole('button',{name:'Align middle center',exact:true}).click();await wait();
    assert.equal(await page.getByRole('button',{name:'Align middle center',exact:true}).getAttribute('aria-pressed'),'true');
    await page.getByLabel('Gap mode',{exact:true}).selectOption('auto');await wait();assert.equal((await data('row')).layout.main,'space_between');
    await page.getByLabel('Gap mode',{exact:true}).selectOption('fixed');await wait();
    await page.getByRole('button',{name:'Wrap',exact:true}).click();await wait();
    await set('Cross gap','24*2');assert.equal((await data('row')).layout.gap_cross,48);
    await page.getByRole('button',{name:'Per-side padding',exact:true}).click();await set('Padding left','+8');assert.equal((await data('row')).layout.padding[3],40);
    await page.getByRole('button',{name:'Row',exact:true}).click();await wait();
    await page.getByRole('button',{name:'Align middle left',exact:true}).click();await wait();
    await page.getByLabel('Clip content',{exact:true}).check();await wait();assert.equal((await data('row')).clip,true);
    await page.getByRole('button',{name:'+ min/max',exact:true}).click();await set('MIN W','800');assert.equal((await data('row')).sizing.min_w,800);
    await page.getByRole('button',{name:'− min/max',exact:true}).click();
    await page.getByRole('button',{name:'Align middle left',exact:true}).scrollIntoViewIfNeeded();
    await shot('auto-layout',theme);

    await fresh(theme);await select(['a']);
    let before = await depth(), count = await applyCount();
    let start = await point(240,710), end = await point(634,710);
    await moveTo(start);await page.mouse.down();await moveTo(end);
    assert.ok(await page.locator('.guide-line').count()>0);assert.ok(await page.locator('.distance-label').count()>0);
    assert.ok(await page.locator('.guide-line').evaluateAll(lines => lines.some(n => n.getBoundingClientRect().width===1 || n.getBoundingClientRect().height===1)));
    assert.equal(await applyCount(),count,'drag preview does not send operations');
    await shot('guides',theme);await page.mouse.up();await wait();
    assert.ok(Math.abs((await data('a')).position.x-580)<=1,'snap position within one slide pixel');
    assert.equal(await depth(),before+1);assert.equal(await applyCount(),count+1);
    await page.locator('#undo').click();await wait();assert.equal((await data('a')).position.x,180);
    start=await point(240,710);end=await point(204,710);await moveTo(start);await page.mouse.down();await moveTo(end);
    assert.ok(await page.locator('.equal-spacing').count()>0,'matching gaps display equal-spacing brackets');await page.mouse.up();await wait();
    assert.ok(Math.abs((await data('a')).position.x-140)<=1);
    await page.keyboard.down('Alt');const hover=await point(1100,700);await moveTo(hover);assert.ok(await page.locator('.distance-label').count()>0);await moveTo(await point(1550,560));assert.equal(await page.locator('.distance-label').count(),2,'Alt measures diagonal neighbours on both axes');await page.keyboard.up('Alt');
    const beforeNudge=(await data('a')).position.x;await page.keyboard.press('Shift+ArrowRight');await wait();assert.equal((await data('a')).position.x,beforeNudge+10);

    await fresh(theme);await select(['r1']);before=await depth();count=await applyCount();
    let r=await rect('r1');start=await point(r.x+r.w/2,r.y+r.h/2);r=await rect('r3');end=await point(r.x+r.w*.9,r.y+r.h/2);
    await moveTo(start);await page.mouse.down();await moveTo(end);
    assert.equal(await page.locator('.insertion-line').count(),1);assert.equal((await page.locator('.insertion-line').boundingBox()).width,2);
    assert.equal(await page.locator('#canvas .drag-ghost').count(),1);assert.equal(await applyCount(),count);
    await shot('reorder',theme);await page.mouse.up();await wait();
    assert.deepEqual((await data('row')).children.map(n=>n.id),['r2','r3','r1']);assert.equal(await depth(),before+1);assert.equal(await applyCount(),count+1);
    await page.locator('#undo').click();await wait();assert.deepEqual((await data('row')).children.map(n=>n.id),['r1','r2','r3']);
    r=await rect('r1');start=await point(r.x+r.w/2,r.y+r.h/2);end=await point(1530,800);
    await moveTo(start);await page.mouse.down();await moveTo(end);await page.mouse.up();await wait();
    assert.deepEqual((await data('destination')).children.map(n=>n.id),['r1']);assert.equal(await depth(),before+1);
    await page.locator('#undo').click();await wait();
    r=await rect('r1');start=await point(r.x+r.w/2,r.y+r.h/2);end=await point(900,950);
    await moveTo(start);await page.mouse.down();await page.keyboard.down('Control');await moveTo(end);await page.mouse.up();await page.keyboard.up('Control');await wait();
    assert.ok(await page.evaluate(()=>window.__d1.deck.slides[0].children.some(n=>n.id==='r1' && typeof n.position==='object')));assert.equal(await depth(),before+1);

    await fresh(theme);await select(['heading']);
    await page.getByRole('button',{name:'Font family',exact:true}).click();await page.locator('#font-picker').waitFor();
    assert.deepEqual(await page.locator('.font-group').allTextContents(),['In this deck','Installed']);
    await page.waitForFunction(()=>[...document.querySelectorAll('.font-row')].some(n=>n.style.fontFamily.includes('Helvetica')));
    await shot('fonts',theme);
    await page.getByLabel('Search fonts').fill('Georgia');await page.keyboard.press('ArrowDown');await page.keyboard.press('Enter');await wait();assert.equal((await data('heading')).font.family,'Georgia');
    assert.deepEqual(await page.getByLabel('Weight',{exact:true}).locator('option').evaluateAll(options=>options.map(o=>o.value)),['400','700']);
    await set('Size (px)','24*2');assert.equal((await data('heading')).font.size,48);
    await page.getByRole('button',{name:'Detach',exact:true}).click();await wait();assert.equal((await data('heading')).style,'body');assert.equal(await page.getByLabel('Text style',{exact:true}).inputValue(),'detached');assert.equal((await data('heading')).font.size,48);
    await page.getByRole('button',{name:'Create style',exact:true}).click();await page.locator('#path-input').fill('Evidence');await page.locator('#path-confirm').click();await wait();assert.equal((await data('heading')).style,'evidence');
    await set('Size (px)','64');await page.getByRole('button',{name:'Update style',exact:true}).click();await wait();assert.equal(await page.evaluate(()=>window.__d1.deck.theme.text_styles.evidence.size),64);
    const caption=page.locator('.property > .scrub-label').filter({hasText:/^Size \(px\)$/});await caption.scrollIntoViewIfNeeded();const label=await caption.boundingBox();
    before=await depth();count=await applyCount();await page.mouse.move(label.x+10,label.y+label.height/2);await page.mouse.down();await page.mouse.move(label.x+30,label.y+label.height/2,{steps:10});assert.equal(await applyCount(),count);await page.mouse.up();await wait();
    assert.equal((await data('heading')).font.size,84);assert.equal(await depth(),before+1);assert.equal(await applyCount(),count+1);await page.locator('#undo').click();await wait();assert.equal((await data('heading')).font.size,64);
    await page.getByLabel('Size (px)',{exact:true}).press('Shift+ArrowUp');await wait();assert.equal((await data('heading')).font.size,74);
    await page.getByRole('button',{name:'Font family',exact:true}).click();const choosing=page.waitForEvent('filechooser');await page.getByRole('button',{name:'Import font…',exact:true}).click();const chooser=await choosing;
    const fontPath=['/System/Library/Fonts/Supplemental/Arial.ttf','/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf'].find(p=>fs.existsSync(p));assert.ok(fontPath,'system font fixture available');const fontBytes=fs.readFileSync(fontPath);
    await chooser.setFiles({name:'Picked font.ttf',mimeType:'font/ttf',buffer:fontBytes});await page.waitForFunction(()=>window.__d1.commands.some(c=>c.command==='ImportFontData'));
    const imported=await page.evaluate(()=>window.__d1.commands.find(c=>c.command==='ImportFontData'));assert.equal(imported.name,'Picked font.ttf');assert.equal(imported.data,fontBytes.toString('hex'));assert.equal(imported.path,undefined);

    await fresh(theme);await select(['a']);await page.locator('#properties-panel').evaluate(n=>n.scrollTop=0);
    assert.equal(await layer('hidden').getByRole('button',{name:'Show layer',exact:true}).count(),1);
    assert.equal(await layer('locked').getByRole('button',{name:'Unlock layer',exact:true}).count(),1);
    await node('locked').click();assert.equal(await node('a').getAttribute('class').then(s=>s.includes('selected')),true,'locked canvas nodes cannot be selected');
    await select(['locked']);assert.ok((await node('locked').getAttribute('class')).includes('selected'),'layers can select a locked node');assert.equal(await page.locator('.resize-handle').count(),0);
    await layer('a').locator('.layer-name').dblclick();await layer('a').locator('input').fill('Evidence / renamed');await layer('a').locator('input').press('Enter');await wait();assert.equal((await data('a')).name,'Evidence / renamed');
    await layer('row').getByRole('button',{name:'Collapse frame',exact:true}).click();assert.equal(await layer('r1').count(),0);await layer('row').getByRole('button',{name:'Expand frame',exact:true}).click();assert.equal(await layer('r1').count(),1);
    await layer('hidden').getByRole('button',{name:'Show layer',exact:true}).click();await wait();assert.equal((await data('hidden')).hidden,false);await layer('hidden').getByRole('button',{name:'Hide layer',exact:true}).click();await wait();
    await layer('locked').getByRole('button',{name:'Unlock layer',exact:true}).click();await wait();assert.equal((await data('locked')).locked,false);await layer('locked').getByRole('button',{name:'Lock layer',exact:true}).click();await wait();
    await select(['a']);await page.locator('#properties-panel').evaluate(n=>n.scrollTop=0);await shot('layers',theme);
    const dt=await page.evaluateHandle(()=>new DataTransfer());await dt.evaluate(d=>d.setData('text/plain','a'));
    const destBox=await layer('destination').boundingBox();await layer('destination').dispatchEvent('dragover',{dataTransfer:dt,clientY:destBox.y+destBox.height/2});before=await depth();await layer('destination').dispatchEvent('drop',{dataTransfer:dt});await wait();assert.ok((await data('destination')).children.some(n=>n.id==='a'));assert.equal(await depth(),before+1);

    await fresh(theme);await select(['a','b','c']);
    assert.equal(await page.getByLabel('Y',{exact:true}).getAttribute('placeholder'),'Mixed');
    before=await depth();await page.keyboard.press('Alt+KeyW');await wait();const alignedY=await Promise.all(['a','b','c'].map(async id=>(await data(id)).position.y));assert.ok(alignedY.every(y=>y===alignedY[0] && Math.abs(y-650)<1));assert.equal(await depth(),before+1);
    await page.getByRole('button',{name:'Distribute horizontal spacing',exact:true}).click();await wait();const a=await data('a'),b=await data('b'),c=await data('c');assert.ok(Math.abs((b.position.x-a.position.x-a.sizing.w)-(c.position.x-b.position.x-b.sizing.w))<1);
    await page.locator('#properties-panel').evaluate(n=>n.scrollTop=0);await shot('align',theme);
    await page.getByRole('button',{name:'Tidy up',exact:true}).click();await wait();
    await page.keyboard.press('Shift+KeyA');await wait();const wrapped=await page.evaluate(()=>window.__d1.deck.slides[0].children.find(n=>n.kind==='frame'&&n.children.some(c=>c.id==='a')));assert.equal(wrapped.layout.mode,'row');assert.equal(wrapped.layout.gap,220);
    await page.keyboard.press('Alt+Shift+KeyA');await wait();assert.ok(await page.evaluate(()=>window.__d1.deck.slides[0].children.some(n=>n.id==='a')));
    await select(['r1']);await page.keyboard.press('Alt+KeyD');await wait();assert.equal((await data('row')).layout.main,'end');
    await select(['row']);await page.getByRole('button',{name:'Column',exact:true}).click();await wait();await select(['r1','r2','r3']);before=await depth();await page.getByRole('button',{name:'Tidy up',exact:true}).click();await wait();assert.equal((await data('row')).layout.main,'space_between');assert.equal(await depth(),before+1);
    // Resizing hug/fill changes only touched axes to fixed; modifiers preserve
    // aspect ratio and center, and cancellation emits no edit.
    await fresh(theme);await select(['a']);await page.getByLabel('Width mode',{exact:true}).selectOption('hug');await wait();before=await depth();
    let box=await rect('a'), handle=await page.locator('.handle-e').boundingBox();await page.mouse.move(handle.x+handle.width/2,handle.y+handle.height/2);await page.mouse.down();await page.keyboard.down('Alt');await page.mouse.move(handle.x+handle.width/2+20,handle.y+handle.height/2);await page.mouse.up();await page.keyboard.up('Alt');await wait();
    let resized=await rect('a');assert.equal(typeof (await data('a')).sizing.w,'number');assert.ok(Math.abs(resized.x+resized.w/2-box.x-box.w/2)<1);assert.equal(await depth(),before+1);
    box=await rect('a');handle=await page.locator('.handle-se').boundingBox();await page.mouse.move(handle.x+handle.width/2,handle.y+handle.height/2);await page.mouse.down();await page.keyboard.down('Shift');await page.mouse.move(handle.x+30,handle.y+12);await page.mouse.up();await page.keyboard.up('Shift');await wait();resized=await rect('a');assert.ok(Math.abs(resized.w/resized.h-box.w/box.h)<.02);
    before=await depth();count=await applyCount();box=await rect('a');start=await point(box.x+box.w/2,box.y+box.h/2);await moveTo(start);await page.mouse.down();await page.mouse.move(start.x+30,start.y+15);await page.keyboard.press('Escape');await page.mouse.up();await wait();assert.equal(await depth(),before);assert.equal(await applyCount(),count);
  }
  assert.deepEqual(errors,[]);
  console.log('12 screenshots at 1440×900 · default nexus + light Alacritty');
  console.log('PASS: wrap/unwrap, layout grid/modes/padding/min/max/clip, snap ≤1px, 1px guides/distances/equal gaps, reorder/reparent/absolute escape, align/distribute/tidy, mixed values, arithmetic/steps/scrub, font search/keyboard/weights/styles/native chooser transport, rename/hide/lock/collapse/layers drop, hug resize/aspect/center/cancel; gestures send one Apply and one undo step.');
  console.log(outputs.join('\n'));
} finally { await browser.close();await new Promise(resolve=>server.close(resolve)); }
