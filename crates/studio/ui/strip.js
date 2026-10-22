import { layoutSlide, layouts, id, withNewIds, allNodes, slideSize } from './helpers.js';
import { signature, slideAssets } from './local.js';
import { element, renderSlide } from './render.js';
import { icon } from './icons.js';
import { previewThumbnail } from './animate-panel.js';

export function stripController(api) {
  const strip = document.querySelector('#strip');
  const menu = document.querySelector('#layout-menu');
  function addSlide(layout) {
    const slide = layoutSlide(layout);
    const after = api.slide()?.id || null;
    api.apply(() => [{ op: 'add_slide', after, slide }]);
    api.state.slideId = slide.id; api.state.selected.clear();
    menu.hidden = true;
  }
  for (const layout of layouts) {
    const button = element('button', '', layout);
    button.onclick = () => addSlide(layout);
    document.querySelector('#layout-options').append(button);
  }
  document.querySelector('#add-slide').onclick = () => { menu.hidden = !menu.hidden; };
  document.querySelector('#empty-add').onclick = () => { menu.hidden = false; };
  document.addEventListener('pointerdown', event => { if (!event.target.closest('#layout-menu, #add-slide, #empty-add')) menu.hidden = true; });
  const cached = new Map();
  function render() {
    const live = new Set(api.state.deck.slides.map(s => s.id));
    for (const [id, entry] of cached) if (!live.has(id)) { entry.item.remove(); cached.delete(id); }
    api.state.deck.slides.forEach((slide, index) => {
      const context = signature([api.state.deck.theme, slideAssets(slide, api.state.deck.theme, api.state.images)]);
      const old = cached.get(slide.id), key = signature(slide);
      if (old && old.context === context) {
        if (old.index !== index) {
          old.item.querySelector('.thumbnail').setAttribute('aria-label', `Slide ${index + 1}`);
          old.item.querySelector('.slide-caption > span').textContent = String(index + 1).padStart(2, '0');
          old.index = index;
        }
        if (old.key !== key) {
          const withoutPosition = s => { const copy = structuredClone(s); for (const n of allNodes(copy)) delete n.position; return signature(copy); };
          const before = allNodes(old.slide), after = allNodes(slide);
          const positionsOnly = withoutPosition(old.slide) === withoutPosition(slide) && after.every((n, i) => signature(n.position) === signature(before[i].position) || typeof n.position === 'object' && typeof before[i].position === 'object');
          if (positionsOnly) {
            const width = slideSize(api.state.deck.theme).w;
            for (const n of after) if (typeof n.position === 'object') {
              const el = [...old.item.querySelectorAll('[data-id]')].find(el => el.dataset.id === n.id);
              el.style.left = `${(n.position.x / width * 100).toFixed(4)}cqw`; el.style.top = `${(n.position.y / width * 100).toFixed(4)}cqw`;
            }
            old.key = key; old.slide = structuredClone(slide);
          }
        }
        if (old.key === key) { old.item.classList.toggle('active', slide.id === api.state.slideId); return; }
      }
      const item = element('div', 'slide-item');
      item.classList.toggle('active', slide.id === api.state.slideId);
      item.draggable = true;
      const thumb = element('div', 'slide thumbnail');
      thumb.tabIndex = 0; thumb.setAttribute('role', 'button'); thumb.setAttribute('aria-label', `Slide ${index + 1}`);
      renderSlide(thumb, slide, api.state.deck.theme, api.state.images);
      thumb.onpointerenter = () => { const index = api.state.deck.slides.findIndex(s => s.id === slide.id); previewThumbnail(thumb,api.state.deck.slides[index-1],api.state.deck.slides[index],api.state.deck.theme,api.state.images,renderSlide); };
      if (slide.transition && slide.transition.type !== 'none') {
        const glyph = element('span','transition-glyph'); glyph.append(icon('transition')); glyph.title = `${slide.transition.type || 'none'} transition`; item.append(glyph);
      }
      const select = () => { api.clearPreview(); api.state.openThread = null; api.state.draftAnchor = null; api.state.slideId = slide.id; api.state.playStep = 0; api.state.selected.clear(); api.state.scopeSlide = true; api.render(); };
      thumb.onclick = select; thumb.onkeydown = event => { if (event.key === 'Enter') select(); };
      const caption = element('div', 'slide-caption');
      caption.append(element('span', '', `${String(index + 1).padStart(2, '0')}`));
      const actions = element('details', 'slide-actions');
      const summary = element('summary', '', '⋯'); summary.title = 'Slide actions';
      actions.append(summary);
      const duplicate = element('button', '', 'Duplicate');
      duplicate.onclick = () => {
        const copy = structuredClone(api.state.deck.slides.find(s => s.id === slide.id)); copy.id = id('s'); copy.children = copy.children.map(node => withNewIds(node, 'b'));
        api.apply(() => [{ op: 'add_slide', after: slide.id, slide: copy }]);
        api.state.slideId = copy.id; api.state.selected.clear();
      };
      const remove = element('button', '', 'Delete');
      remove.onclick = () => api.apply(() => [{ op: 'delete_slide', id: slide.id }]);
      actions.append(duplicate, remove); caption.append(actions);
      item.oncontextmenu = event => { event.preventDefault(); actions.open = !actions.open; };
      item.ondragstart = event => { event.dataTransfer.setData('text/plain', slide.id); event.dataTransfer.effectAllowed = 'move'; };
      item.ondragover = event => { event.preventDefault(); item.classList.add('drop-target'); };
      item.ondragleave = () => item.classList.remove('drop-target');
      item.ondrop = event => {
        event.preventDefault(); item.classList.remove('drop-target');
        const source = event.dataTransfer.getData('text/plain');
        if (api.state.deck.slides.some(s => s.id === source)) api.apply(() => [{ op: 'move_slide', id: source, to: api.state.deck.slides.findIndex(s => s.id === slide.id) }]);
      };
      item.append(thumb, caption);
      if (old) old.item.replaceWith(item); else strip.append(item);
      cached.set(slide.id, { item, key, context, index, slide:structuredClone(slide) });
    });
    api.state.deck.slides.forEach((slide, index) => { const item = cached.get(slide.id).item; if (strip.children[index] !== item) strip.insertBefore(item, strip.children[index] || null); });
  }
  return { render };
}
