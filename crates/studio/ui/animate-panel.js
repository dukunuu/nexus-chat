import { allNodes, nodeLabel } from './helpers.js';
import { element } from './render.js';
import { fields, compact } from './fields.js';
import { button, icon } from './icons.js';
import { buildPlan, slots, easings, effects, preparePlayback, playGroup, cancelAnimations, transitionSlides } from './animations.js';

export function animatePanelController(api) {
  const field = fields(api), badgesLayer = document.querySelector('#build-badges');
  let preview = [], opened = 'build_in';
  const defaults = { effect:'fade',trigger:'click',duration_ms:400,delay_ms:0,easing:'ease_out',by:'whole',stagger_ms:60 };
  const pair = (...children) => { const row = element('div','field-pair'); row.append(...children); return row; };
  const numeric = (label,value,change,caption) => compact(field(label,value,change,null,'number'),caption);
  const sides = [['left','Left'],['right','Right'],['up','Up'],['down','Down']];
  const curves = [['linear','Linear'],['ease','Ease'],['ease_in','In'],['ease_out','Out'],['ease_in_out','In-out'],['spring','Spring']];
  const short = (row, caption) => compact(row, caption);
  const clamp = (v,max) => Math.round(Math.max(0,Math.min(max,v)));
  function section(panel,title) { const s = element('section','design-section'), h = element('div','section-heading',title), body = element('div','section-body'); s.append(h,body); panel.append(s); return body; }
  function render(panel) {
    const slide = api.slide(), node = allNodes(slide).find(n => api.state.selected.has(n.id)); if (!slide) return;
    const transition = { type:'none',duration_ms:400,easing:'ease_out',direction:'left',...slide.transition };
    const writeTransition = patch => api.apply(() => [{ op:'set_transition',slide:slide.id,transition:{ ...transition,...patch } }]);
    const body = section(panel,'Slide transition'), picker = element('div','transition-picker');
    for (const [type,glyph] of [['none','minus'],['fade','opacity'],['push','layout-row'],['slide','forward'],['cover','layers'],['zoom','absolute'],['smart','sparkle']]) {
      const control = button(glyph,`${type} transition`,() => writeTransition({ type })); control.append(element('span','',type)); control.setAttribute('aria-pressed',String(transition.type === type)); picker.append(control);
    }
    body.append(picker,pair(short(field('Direction',transition.direction,v => writeTransition({direction:v}),sides),'Dir'),numeric('Transition duration',transition.duration_ms,v => writeTransition({duration_ms:clamp(v,5000)}),'Dur')),short(field('Easing',transition.easing,v => writeTransition({easing:v}),curves),'Ease'));
    if (node) {
      panel.append(element('div','selection-name',nodeLabel(node)));
      for (const slot of slots) {
        const details = element('details','build-controls'), summary = element('summary','',slot.replaceAll('_',' ')); details.open = opened === slot;
        details.ontoggle = () => { if (details.open) opened = slot; };
        const content = element('div','section-body'), current = node.animation?.[slot], build = { ...defaults,effect:slot === 'emphasis' ? 'pulse' : 'fade',...current };
        const write = patch => api.apply(() => [{ op:'set_animation',id:node.id,slot,build:patch === null ? null : { ...build,...patch } }]);
        const available = slot === 'emphasis' ? ['pulse','shake'] : effects.filter(e => !['pulse','shake'].includes(e) && (node.kind === 'text' || e !== 'typewriter'));
        summary.append(element('span','build-summary',current?.effect || 'none')); details.append(summary,content); panel.append(details);
        content.append(short(field(`${slot} effect`,current?.effect || 'none',v => write(v === 'none' ? null : {effect:v}),['none',...available]),'Effect'));
        if (!current) continue;
        content.append(short(field(`${slot} trigger`,build.trigger,v => write({trigger:v}),[['click','Click'],['with_previous','With previous'],['after_previous','After previous']]),'Start'),pair(numeric(`${slot} duration`,build.duration_ms,v => write({duration_ms:clamp(v,5000)}),'Dur'),numeric(`${slot} delay`,build.delay_ms,v => write({delay_ms:clamp(v,10000)}),'Delay')),pair(short(field(`${slot} direction`,build.direction || 'left',v => write({direction:v}),sides),'Dir'),short(field(`${slot} easing`,build.easing,v => write({easing:v}),curves),'Ease')));
        if (node.kind === 'text') content.append(pair(short(field(`${slot} by`,build.by,v => write({by:v}),[['whole','Whole'],['word','Word'],['line','Line'],['item','Item']]),'By'),numeric(`${slot} stagger`,build.stagger_ms ?? 60,v => write({stagger_ms:clamp(v,10000)}),'Gap')));
        const play = button('play',`Preview ${slot}`,() => {
          cancelAnimations(preview); api.renderCanvas();
          const root = document.querySelector('#canvas'); preparePlayback(root,slide);
          preview = playGroup(root,[{id:node.id,slot,build}]);
          const list = preview; Promise.all(list.map(a => a.finished.catch(() => {}))).then(() => { if (preview === list) { cancelAnimations(preview); preview = []; api.renderCanvas(); } });
        }); play.append(element('span','','Preview')); content.append(play);
      }
    } else panel.append(element('p','hint','Select a layer to add a build.'));
    const order = section(panel,'Build order'), plan = buildPlan(slide), nodes = new Map(allNodes(slide).map(n => [n.id,n]));
    let click = 0;
    for (const [index,entry] of plan.entries.entries()) {
      if ((entry.build.trigger || 'click') === 'click') click++;
      const row = element('button','build-order-row'); row.dataset.buildNode = entry.id; row.dataset.slot = entry.slot; row.draggable = true;
      row.classList.toggle('active',api.state.selected.has(entry.id));
      const glyph = ({ click:'cursor',with_previous:'layers',after_previous:'forward' })[entry.build.trigger || 'click'];
      row.append(element('span','build-number',String(index+1)),icon(glyph),element('span','build-effect',entry.build.effect),element('span','build-node-name',nodeLabel(nodes.get(entry.id))));
      row.title = `${entry.slot} · ${entry.build.trigger || 'click'} · ${nodeLabel(nodes.get(entry.id))}`;
      const step = Math.max(0,click-1);
      row.onclick = () => { api.state.selected = new Set([entry.id]); api.state.playStep = step; api.selectionChanged(); };
      row.ondragstart = event => event.dataTransfer.setData('text/build-node',entry.id);
      row.ondragover = event => { event.preventDefault(); row.classList.add('drop-target'); };
      row.ondragleave = () => row.classList.remove('drop-target');
      row.ondrop = event => {
        event.preventDefault(); const source = event.dataTransfer.getData('text/build-node');
        if (source === entry.id || !nodes.has(source)) return;
        const ids = [...new Set(plan.entries.map(e => e.id))].filter(id => id !== source); ids.splice(ids.indexOf(entry.id),0,source);
        api.apply(() => [{op:'reorder_builds',slide:slide.id,ids}]);
      };
      order.append(row);
    }
    if (!plan.entries.length) order.append(element('p','hint','No builds. All layers are visible.'));
  }
  function badges() {
    badgesLayer.replaceChildren();
    if (!api.state.properties || api.state.panelTab !== 'animate') return;
    const canvas = document.querySelector('#canvas'), bounds = canvas.getBoundingClientRect(), wrap = document.querySelector('#slide-wrap').getBoundingClientRect();
    if (!bounds.width) return;
    const rects = [...canvas.querySelectorAll('.block')].map(el => ({id:el.dataset.id,r:el.getBoundingClientRect()})), placed = [], seen = new Set();
    for (const [index,entry] of buildPlan(api.slide()).entries.entries()) {
      if (seen.has(entry.id)) continue; seen.add(entry.id);
      const target = rects.find(n => n.id === entry.id)?.r; if (!target || !target.width) continue;
      const candidates = [[target.left-27,target.top-27],[target.right+5,target.top-27],[target.left-27,target.bottom+5],[target.right+5,target.bottom+5]];
      const overlap = (x,y,r) => x < r.right && x+22 > r.left && y < r.bottom && y+22 > r.top;
      const [x,y] = candidates.sort((a,b) => {
        const score = ([x,y]) => rects.filter(n => overlap(x,y,n.r)).length + placed.filter(r => overlap(x,y,r)).length * 10;
        return score(a)-score(b);
      })[0]; placed.push({left:x,top:y,right:x+22,bottom:y+22});
      const badge = element('button','build-badge',String(index+1)); badge.title = `Build ${index+1}`;
      badge.style.left = `${(x+11-bounds.left)/bounds.width*100}%`; badge.style.top = `${(y+11-bounds.top)/bounds.height*100}%`;
      badge.style.setProperty('--badge-scale',String(document.querySelector('#slide-wrap').offsetWidth/Math.max(1,wrap.width)));
      badge.onclick = () => { api.state.selected = new Set([entry.id]); api.selectionChanged(); };
      badgesLayer.append(badge);
    }
  }
  return { render,badges };
}

/** One-second thumbnail transition; the temporary playback tree is never saved. */
export function previewThumbnail(thumb, before, after, theme, images, renderSlide) {
  if (!after.transition || after.transition.type === 'none' || thumb.dataset.previewing) return;
  thumb.dataset.previewing = 'true';
  const old = element('div','slide thumbnail-playback'), next = element('div','slide thumbnail-playback'); thumb.replaceChildren(old,next);
  renderSlide(old,before || after,theme,images); renderSlide(next,after,theme,images);
  const animations = transitionSlides(old,next,before || after,after,{...after.transition,duration_ms:1000});
  Promise.all(animations.map(a => a.finished.catch(() => {}))).then(() => { cancelAnimations(animations); delete thumb.dataset.previewing; renderSlide(thumb,after,theme,images); });
}
