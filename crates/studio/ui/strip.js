import { layoutSlide, layouts, id } from './helpers.js';
import { element, renderSlide } from './render.js';

export function stripController(api) {
  const strip = document.querySelector('#strip');
  const menu = document.querySelector('#layout-dialog');
  function addSlide(layout) {
    const slide = layoutSlide(layout);
    const after = api.slide()?.id || null;
    api.apply(() => [{ op: 'add_slide', after, slide }]);
    api.state.slideId = slide.id; api.state.selected.clear();
    menu.close();
  }
  for (const layout of layouts) {
    const button = element('button', '', layout);
    button.onclick = () => addSlide(layout);
    document.querySelector('#layout-options').append(button);
  }
  document.querySelector('#add-slide').onclick = () => menu.showModal();
  document.querySelector('#cancel-layout').onclick = () => menu.close();
  function render() {
    strip.replaceChildren();
    api.state.deck.slides.forEach((slide, index) => {
      const item = element('div', 'slide-item');
      item.classList.toggle('active', slide.id === api.state.slideId);
      item.draggable = true;
      const thumb = element('div', 'slide thumbnail');
      thumb.tabIndex = 0; thumb.setAttribute('role', 'button'); thumb.setAttribute('aria-label', `Slide ${index + 1}`);
      renderSlide(thumb, slide, api.state.deck.theme, api.state.images);
      const select = () => { api.state.slideId = slide.id; api.state.selected.clear(); api.render(); };
      thumb.onclick = select; thumb.onkeydown = event => { if (event.key === 'Enter') select(); };
      const caption = element('div', 'slide-caption');
      caption.append(element('span', '', `${index + 1} · ${slide.layout}`));
      const actions = element('details', 'slide-actions');
      const summary = element('summary', '', '⋯'); summary.title = 'Slide actions';
      actions.append(summary);
      const duplicate = element('button', '', 'Duplicate');
      duplicate.onclick = () => {
        const copy = structuredClone(slide); copy.id = id('s'); copy.blocks.forEach(b => b.id = id('b'));
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
        if (api.state.deck.slides.some(s => s.id === source)) api.apply(() => [{ op: 'move_slide', id: source, to: index }]);
      };
      item.append(thumb, caption); strip.append(item);
    });
  }
  return { render };
}
