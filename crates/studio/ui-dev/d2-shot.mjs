// Run: node crates/studio/ui-dev/d2-shot.mjs
// Hermetic D2 playback/editor assertions and 1440x900 terminal-theme captures.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const here = path.dirname(fileURLToPath(import.meta.url)), root = path.resolve(here,'../../..');
const installed = [path.join(root,'web/node_modules/playwright-core/index.mjs'),'/Users/dukunuu/work/nexus-chat/web/node_modules/playwright-core/index.mjs'].find(p => fs.existsSync(p));
if (!installed) throw new Error('Use the existing web/node_modules/playwright-core installation.');
const { chromium } = await import(pathToFileURL(installed).href);
const theme = JSON.parse(fs.readFileSync(path.join(here,'fixtures/d1-comments.json'),'utf8')).deck.theme;
const shape = (id,name,x,y,fill='accent',w=320,h=240) => ({id,name,kind:'shape',shape:'rect',position:{x,y},sizing:{w,h},fill,radius:20});
const text = (id,body,x,y) => ({id,name:'Headline',kind:'text',text:body,style:'heading',position:{x,y},sizing:{w:1600,h:130}});
const deck = {version:2,title:'Motion / a clearer story',theme,slides:[
  {id:'s1',notes:'Pause after the evidence. Let the audience see the connection.',builds:['a','b','c','d','title'],children:[
    {...text('title','Ideas, in motion.',160,100),animation:{emphasis:{effect:'pulse',trigger:'click'}}},
    {...shape('a','Evidence',180,390),animation:{build_in:{effect:'fade',duration_ms:200}}},
    {...shape('b','Connection',580,390,'fg'),animation:{build_in:{effect:'rise',trigger:'with_previous',duration_ms:200}}},
    {...shape('c','Insight',980,390,'muted'),animation:{build_in:{effect:'scale',trigger:'after_previous',duration_ms:200}}},
    {...text('d','- Find the signal\n- Make connections\n- Keep the thread',180,760),style:'body',sizing:{w:1500,h:280},animation:{build_in:{effect:'wipe',by:'item',stagger_ms:70,duration_ms:200},build_out:{effect:'fade',duration_ms:200}}},
  ]},
  {id:'s2',notes:'The evidence becomes a system.',transition:{type:'push',direction:'left',duration_ms:1200},children:[text('t2','A system, taking shape.',160,100),shape('hero2','Hero',180,340,'accent',600,460),shape('detail2','Detail',1100,490,'muted',400,240)]},
  {id:'s3',notes:'Same named layers, a new perspective. P keeps these notes private in this window only.',transition:{type:'smart',duration_ms:1200},children:[{...text('t3','A new perspective.',160,200),font:{size:94,color:'accent'}},{...shape('hero3','Hero',980,460,'fg',500,320),rotation:18,opacity:.8},shape('detail3','Detail',240,530,'accent',300,260)]},
]};
const mock = `(() => {
  let deck=structuredClone(window.__fixture), sequence=0; const undo=[],redo=[],commands=[];
  const nodes=s=>(s.children||[]).flatMap(n=>[n,...nodes(n)]);
  const event=()=>({event:'Deck',deck,revision:String(sequence),hashes:Object.fromEntries(deck.slides.flatMap(nodes).map(n=>[n.id,'mock'])),images:{},threads:[],linked:false,changed:[]});
  window.__d2={commands,get deck(){return structuredClone(deck)},get undoDepth(){return undo.length}};
  window.ipc={postMessage(source){const c=JSON.parse(source);commands.push(c);setTimeout(()=>{
    if(c.command==='Apply') { const next=structuredClone(deck);
      for(const op of c.ops){ const s=next.slides.find(s=>s.id===op.slide), n=next.slides.flatMap(nodes).find(n=>n.id===op.id);
        if(op.op==='set_transition')s.transition=op.transition;
        else if(op.op==='set_animation'){n.animation ||= {};if(op.build)n.animation[op.slot]=op.build;else delete n.animation[op.slot];}
        else if(op.op==='reorder_builds')s.builds=op.ids;
        else throw new Error('Unsupported D2 mock op '+op.op);
      }
      if(JSON.stringify(next)!==JSON.stringify(deck)){undo.push(deck);redo.length=0;deck=next;sequence++;}
    } else if(c.command==='Undo' && undo.length){redo.push(deck);deck=undo.pop();sequence++;}
    else if(c.command==='Redo' && redo.length){undo.push(deck);deck=redo.pop();sequence++;}
    window.studioEvent(event());
  },5)}};
})();`;
const ui = path.resolve(here,'../ui'), out = path.join(here,'shots'); fs.mkdirSync(out,{recursive:true});
const server = http.createServer((req,res) => {
  const target=path.resolve(ui,req.url==='/'?'index.html':`.${req.url}`);
  if(!target.startsWith(ui+path.sep)||!fs.existsSync(target)){res.writeHead(404);res.end();return;}
  res.writeHead(200,{'Content-Type':({'.html':'text/html','.js':'text/javascript','.css':'text/css'})[path.extname(target)]||'application/octet-stream'});fs.createReadStream(target).pipe(res);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch(), errors=[], outputs=[]; let page;
const wait=()=>page.waitForTimeout(80);
const shot=async(name,palette)=>{const file=path.join(out,`d2-${name}-${palette}.png`);await page.screenshot({path:file});outputs.push(file);};
const depth=()=>page.evaluate(()=>window.__d2.undoDepth);
const key=async value=>{await page.keyboard.press(value);await wait();};
const opacity=id=>page.locator(`#present-stage .block[data-id="${id}"]`).evaluate(el=>Number(getComputedStyle(el).opacity));
async function fresh(palette,reduce=false){
  await page?.close();page=await browser.newPage({viewport:{width:1440,height:900},deviceScaleFactor:1,reducedMotion:reduce?'reduce':'no-preference'});
  page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>{if(!r.url().startsWith(`http://127.0.0.1:${server.address().port}/`))errors.push(`Unexpected request ${r.url()}`);});
  await page.addInitScript(value=>window.__fixture=value,deck);await page.addInitScript({content:mock});
  await page.goto(`http://127.0.0.1:${server.address().port}/`);await page.locator('#canvas [data-id="a"]').waitFor();
  const colors=JSON.parse(fs.readFileSync(path.join(here,`fixtures/theme-${palette}.json`),'utf8'));await page.evaluate(colors=>window.studioAsyncEvent({event:'Theme',colors}),colors);
}
async function freezeTransition(){
  await page.waitForFunction(()=>document.querySelectorAll('#present-stage > .slide').length===2);
  return page.evaluate(()=>{const animations=document.querySelector('#present-stage').getAnimations({subtree:true});for(const a of animations){a.pause();a.currentTime=a.effect.getTiming().duration*.5;}return animations.length;});
}
try {
  for(const palette of ['default','light']){
    await fresh(palette);
    const pure=await page.evaluate(async()=>{
      const {buildPlan,stepEngine,smartMatches}=await import('/animations.js');
      const slides=window.__d2.deck.slides,plan=buildPlan(slides[0]),e=stepEngine(slides),order=[];
      order.push(e.next().group.map(x=>x.id));order.push(e.next().group.map(x=>x.id));e.back();order.push(e.cursor.step);e.jump(2);order.push({...e.cursor});e.back();order.push({...e.cursor});
      const match=smartMatches({children:[{id:'stable',name:'old'},{id:'old-id',name:'same'},{id:'unused'}]},{children:[{id:'stable',name:'new'},{id:'new-id',name:'same'},{id:'extra'}]}).map(([a,b])=>[a.id,b.id]);
      const leading=buildPlan({children:[{id:'auto',animation:{build_in:{effect:'fade',trigger:'with_previous'}}},{id:'click',animation:{build_in:{effect:'fade'}}}]});
      return {order,groups:plan.groups.length,match,automatic:leading.automatic.map(x=>x.id)};
    });
    assert.deepEqual(pure.order,[['a','b','c'],['d'],1,{index:2,step:0},{index:1,step:0}]);assert.equal(pure.groups,4);assert.deepEqual(pure.match,[['stable','stable'],['old-id','new-id']]);assert.deepEqual(pure.automatic,['auto']);
    await page.locator('#more-properties').dispatchEvent('click');await page.locator('#tab-animate').click();
    await page.locator('[data-layer="a"] .layer-name').click();await wait();
    assert.equal(await page.locator('.build-order-row').count(),6);assert.equal(await page.locator('.build-badge').count(),5);
    const outside=await page.evaluate(()=>[...document.querySelectorAll('.build-badge')].every(b=>{const n=document.querySelector('#canvas [data-id="'+({1:'a',2:'b',3:'c',4:'d',6:'title'}[b.textContent])+'"]');if(!n)return true;const r=n.getBoundingClientRect(),p=b.getBoundingClientRect();return p.right<=r.left||p.left>=r.right||p.bottom<=r.top||p.top>=r.bottom;}));assert.ok(outside,'badges are outside their animated layer');
    await shot('animate',palette);await shot('badges',palette);
    // Rail: a glyph on each slide that has a transition, and a one second hover preview in the thumbnail.
    assert.equal(await page.locator('.transition-glyph').count(),2);
    await page.locator('#strip .thumbnail').nth(1).hover();assert.equal(await page.locator('#strip .thumbnail').nth(1).getAttribute('data-previewing'),'true');
    await page.waitForFunction(()=>!document.querySelector('#strip .thumbnail[data-previewing]'),null,{timeout:4000});
    let before=await depth();await page.getByRole('button',{name:'fade transition',exact:true}).click();await wait();assert.equal(await depth(),before+1);assert.equal(await page.evaluate(()=>window.__d2.deck.slides[0].transition.type),'fade');
    await page.locator('#undo').dispatchEvent('click');await wait();assert.equal(await depth(),before);
    before=await depth();await page.getByLabel('build_in effect',{exact:true}).selectOption('fly');await wait();assert.equal(await depth(),before+1);assert.equal(await page.evaluate(()=>window.__d2.deck.slides[0].children.find(n=>n.id==='a').animation.build_in.effect),'fly');
    await page.locator('#undo').dispatchEvent('click');await wait();
    // Scrub is a preview, then exactly one transaction on release.
    const label=page.getByLabel('build_in duration',{exact:true}).locator('..').locator('.scrub-label'),box=await label.boundingBox();before=await depth();const count=await page.evaluate(()=>window.__d2.commands.length);
    await page.mouse.move(box.x+5,box.y+box.height/2);await page.mouse.down();await page.mouse.move(box.x+25,box.y+box.height/2,{steps:5});assert.equal(await page.evaluate(()=>window.__d2.commands.length),count);await page.mouse.up();await wait();assert.equal(await depth(),before+1);await page.locator('#undo').dispatchEvent('click');await wait();
    const dt=await page.evaluateHandle(()=>new DataTransfer());await dt.evaluate(d=>d.setData('text/build-node','d'));before=await depth();await page.locator('[data-build-node="a"]').dispatchEvent('drop',{dataTransfer:dt});await wait();assert.equal(await depth(),before+1);assert.equal(await page.evaluate(()=>window.__d2.deck.slides[0].builds[0]),'d');await page.locator('#undo').dispatchEvent('click');await wait();
    before=await depth();await page.getByRole('button',{name:'Preview build_in',exact:true}).click();await page.waitForTimeout(300);assert.equal(await depth(),before,'preview never writes');
    await page.locator('#play-from-here').dispatchEvent('click');await wait();assert.equal(await page.locator('#present-step').textContent(),'build 0/4');assert.equal(await opacity('a'),0);
    await key('Space');await page.waitForTimeout(450);assert.equal(await page.locator('#present-step').textContent(),'build 1/4');for(const id of ['a','b','c'])assert.ok(await opacity(id)>.99);assert.equal(await opacity('d'),0);
    await key('ArrowRight');await page.waitForTimeout(400);assert.ok(await opacity('d')>.99);await key('Backspace');assert.equal(await opacity('d'),0);await key('ArrowLeft');assert.equal(await opacity('a'),0);
    await key('End');assert.equal(await page.locator('#present-counter').textContent(),'3 / 3');await key('Home');assert.equal(await page.locator('#present-counter').textContent(),'1 / 3');
    await key('2');await page.keyboard.press('Enter');await wait();assert.equal(await page.locator('#present-counter').textContent(),'2 / 3');
    await key('Home');for(let i=0;i<4;i++)await key('ArrowRight');await page.waitForTimeout(300);await page.keyboard.press('ArrowRight');assert.ok(await freezeTransition()>=2);await shot('push',palette);
    await key('3');await page.keyboard.press('Enter');await wait();await key('ArrowLeft');assert.equal(await page.locator('#present-counter').textContent(),'2 / 3');await page.keyboard.press('ArrowRight');assert.ok(await freezeTransition()>=4);
    // Same-named layers (Hero, Detail, Headline) on different ids morph from where they were.
    const morphs=await page.evaluate(()=>Object.fromEntries(['hero3','detail3','t3'].map(id=>{const a=document.querySelector(`#present-stage .block[data-id="${id}"]`).getAnimations()[0];return [id,a.effect.getKeyframes()[0].transform];})));
    for(const id of ['hero3','detail3','t3'])assert.match(morphs[id],/translate\(/,`${id} morphs from its previous position`);
    assert.match(morphs.hero3,/scale\(/,'shapes morph size');assert.doesNotMatch(morphs.t3,/scale\(/,'text is moved, never scaled');
    await shot('smart',palette);
    await key('End');await key('p');await shot('presenter',palette);assert.ok(await page.locator('#present-notes').textContent().then(t=>t.includes('Same named layers')));assert.ok(await page.locator('#present-next .slide').count());
    await key('Escape');assert.ok(await page.locator('#presentation').isHidden());
    // Selecting a build picks its previous click boundary for Play from here.
    await page.locator('#strip .thumbnail').first().click();await page.locator('[data-build-node="d"][data-slot="build_in"]').click();await key('Shift+Enter');assert.equal(await page.locator('#present-step').textContent(),'build 1/4');await key('Escape');
  }
  await fresh('default',true);
  const reduced=await page.evaluate(async()=>{
    const {transitionSlides,playGroup,preparePlayback}=await import('/animations.js'),{renderSlide}=await import('/render.js');const deck=window.__d2.deck;
    const old=document.createElement('div'),next=document.createElement('div');old.className=next.className='slide';document.body.append(old,next);renderSlide(old,deck.slides[1],deck.theme,{});renderSlide(next,deck.slides[2],deck.theme,{});
    const animations=transitionSlides(old,next,deck.slides[1],deck.slides[2],{type:'smart',duration_ms:1000});
    const transition=animations.map(a=>({duration:a.effect.getTiming().duration,keys:a.effect.getKeyframes().map(k=>Object.keys(k).filter(k=>!['offset','computedOffset','easing','composite'].includes(k)))}));animations.forEach(a=>a.cancel());
    preparePlayback(next,deck.slides[2]);const builds=playGroup(next,[{id:'hero3',slot:'build_in',build:{effect:'fly',duration_ms:800}}]);const build=builds.map(a=>({duration:a.effect.getTiming().duration,transforms:a.effect.getKeyframes().map(k=>k.transform)}));builds.forEach(a=>a.cancel());const shake=playGroup(next,[{id:'hero3',slot:'emphasis',build:{effect:'shake',duration_ms:600}}]);const emphasis=shake.map(a=>({duration:a.effect.getTiming().duration,transforms:new Set(a.effect.getKeyframes().map(k=>k.transform)).size,opacities:a.effect.getKeyframes().map(k=>Number(k.opacity))}));shake.forEach(a=>a.cancel());old.remove();next.remove();return {transition,build,emphasis};
  });
  assert.ok(reduced.transition.every(a=>a.duration===500&&a.keys.every(k=>k.length===1&&k[0]==='opacity')));assert.equal(reduced.build[0].duration,400);assert.equal(reduced.build[0].transforms[0],reduced.build[0].transforms[1]);
  assert.ok(reduced.emphasis.every(a=>a.duration===300&&a.transforms===1&&Math.min(...a.opacities)<Math.max(...a.opacities)),'reduced emphasis is a quiet opacity dip, nothing moves');
  assert.deepEqual(errors,[]);
  console.log('PASS: click/with/after groups; automatic leading builds; back/Home/End/numeric jump; id/name one-to-one smart matching; reduced fades at half duration; animation edits/reorder/scrub one undo; previews no writes; outside badges; Play from here; presenter notes/next step.');console.log(outputs.join('\n'));
} finally { await page?.close();await browser.close();await new Promise(resolve=>server.close(resolve)); }
