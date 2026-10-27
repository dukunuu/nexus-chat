// The same chooser-byte IPC as font import; no local paths reach the page.
export async function importTemplateFile(file, command, toast) {
  if (file.size > 250 * 1024 * 1024) { toast('Template exceeds the 250 MB size limit.'); return; }
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const hex = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, '0')), parts = [];
    for (let i = 0; i < bytes.length; i += 16384) parts.push(Array.from(bytes.subarray(i, i + 16384), b => hex[b]).join(''));
    command({ command: 'ImportTemplateData', name: file.name, data: parts.join('') });
  } catch (error) { toast(error.message); }
}
export function chooseTemplate(command, toast) {
  const input = document.createElement('input');
  input.type = 'file'; input.accept = '.pptx,.potx,.docx,.dotx'; input.hidden = true;
  document.body.append(input);
  input.oncancel = () => input.remove();
  input.onchange = () => { const file = input.files[0]; input.remove(); if (file) importTemplateFile(file, command, toast); };
  input.click();
}
export function templateDrop(command, toast, signal, active = () => true) {
  document.addEventListener('dragover', event => {
    if (active() && event.dataTransfer?.types.includes('Files')) event.preventDefault();
  }, { capture: true, signal });
  document.addEventListener('drop', event => {
    if (!active()) return;
    const file = [...(event.dataTransfer?.files || [])].find(file => /\.(pptx|potx|docx|dotx|pptm|potm|docm|dotm)$/i.test(file.name));
    if (!file) return;
    event.preventDefault(); event.stopImmediatePropagation();
    importTemplateFile(file, command, toast);
  }, { capture: true, signal });
}
