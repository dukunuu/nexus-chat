import { renderSlide, element } from './render.js';
import { buildPlan, stepEngine, preparePlayback, applyStep, playGroup, transitionSlides, cancelAnimations } from './animations.js';

/** One window, one playback cursor; editor images always remain fully built. */
export function playbackController(api) {
  const overlay = document.querySelector('#presentation'), stage = document.querySelector('#present-stage');
  let engine, active = false, presenter = false, animations = [], generation = 0, digits = '', started = 0, timer;
  const slide = () => api.state.deck.slides[engine.cursor.index];
  const cancel = () => { generation++; cancelAnimations(animations); animations = []; };
  function makeSlide(parent, value, step) {
    const root = element('div','slide playback-slide'); parent.append(root);
    renderSlide(root,value,api.state.deck.theme,api.state.images); preparePlayback(root,value); applyStep(root,value,step); return root;
  }
  function extras() {
    document.querySelector('#present-counter').textContent = `${engine.cursor.index + 1} / ${api.state.deck.slides.length}`;
    document.querySelector('#present-step').textContent = `build ${engine.cursor.step}/${engine.total()}`;
    document.querySelector('#present-jump').textContent = digits ? `Go to slide ${digits} · Enter` : '';
    overlay.classList.toggle('presenter-view',presenter);
    const next = document.querySelector('#present-next'); next.replaceChildren();
    if (presenter) {
      const peek = stepEngine(api.state.deck.slides,engine.cursor.index,engine.cursor.step); peek.next();
      makeSlide(next,api.state.deck.slides[peek.cursor.index],peek.cursor.step);
      document.querySelector('#present-notes').textContent = slide().notes || 'No speaker notes.';
    }
    api.state.playStep = engine.cursor.step;
  }
  function draw() { stage.replaceChildren(); const root = makeSlide(stage,slide(),engine.cursor.step); extras(); return root; }
  function complete(list, done) {
    animations = list; const token = generation;
    Promise.all(list.map(a => a.finished.catch(() => {}))).then(() => { if (token === generation && active) { cancel(); done(); } });
  }
  function automatic(root) {
    const entries = buildPlan(slide()).automatic;
    if (entries.length) complete(playGroup(root,entries),draw);
  }
  function next() {
    const before = slide(); cancel(); const old = draw(), change = engine.next(); extras();
    if (change.group) complete(playGroup(old,change.group),draw);
    else if (change.transition) {
      const incoming = makeSlide(stage,slide(),0);
      complete(transitionSlides(old,incoming,before,slide(),slide().transition || {}),() => automatic(draw()));
    }
  }
  function back() { cancel(); engine.back(); draw(); }
  function jump(index, step = 0) { cancel(); engine.jump(index,step); draw(); }
  function start(fromHere = false) {
    if (!api.state.deck?.slides.length) return;
    active = true; started = Date.now(); digits = ''; overlay.hidden = false;
    engine = stepEngine(api.state.deck.slides,Math.max(0,api.state.deck.slides.findIndex(s => s.id === api.state.slideId)),fromHere ? api.state.playStep || 0 : 0);
    engine.jump(engine.cursor.index,engine.cursor.step); const root = draw(); automatic(root);
    clearInterval(timer); timer = setInterval(() => { const seconds = Math.floor((Date.now()-started)/1000); document.querySelector('#present-timer').textContent = `${String(Math.floor(seconds/60)).padStart(2,'0')}:${String(seconds%60).padStart(2,'0')}`; },250);
    const request = overlay.requestFullscreen || overlay.webkitRequestFullscreen;
    if (request) Promise.resolve(request.call(overlay)).catch(() => api.toast('Presenting in window.'));
  }
  function exit() {
    active = false; cancel(); clearInterval(timer); overlay.hidden = true;
    if (engine) { api.state.slideId = slide().id; api.state.playStep = engine.cursor.step; }
    if (document.fullscreenElement) document.exitFullscreen(); else if (document.webkitFullscreenElement) document.webkitExitFullscreen();
    api.render();
  }
  function key(event) {
    if (!active) return false;
    if (event.key === 'Escape') exit();
    else if (/^\d$/.test(event.key)) { digits = (digits + event.key).slice(-3); extras(); }
    else if (event.key === 'Enter' && digits) { jump(Number(digits)-1); digits = ''; extras(); }
    else if (['ArrowRight','ArrowDown',' '].includes(event.key)) next();
    else if (['ArrowLeft','ArrowUp','Backspace'].includes(event.key)) back();
    else if (event.key === 'Home') jump(0);
    else if (event.key === 'End') jump(api.state.deck.slides.length-1,Infinity);
    else if (event.key.toLowerCase() === 'p') { presenter = !presenter; extras(); }
    event.preventDefault(); return true;
  }
  overlay.onclick = event => { if (!event.target.closest('button,#present-notes,#present-next')) next(); };
  document.querySelector('#present').onclick = () => start();
  document.querySelector('#play-from-here').onclick = () => start(true);
  document.querySelector('#exit-present').onclick = exit;
  document.querySelector('#presenter-toggle').onclick = () => { presenter = !presenter; extras(); };
  for (const name of ['fullscreenchange','webkitfullscreenchange']) document.addEventListener(name,() => { if (!document.fullscreenElement && !document.webkitFullscreenElement && active) exit(); });
  return { start, key, refresh() { if (active) { const { index,step } = engine.cursor; cancel(); engine = stepEngine(api.state.deck.slides); engine.jump(index,step); draw(); } } };
}
