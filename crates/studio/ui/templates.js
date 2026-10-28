// A dropped file travels as chooser bytes, the same IPC as font import; no local paths reach the page.
export async function importTemplateFile(file, command, toast) {
  if (file.size > 250 * 1024 * 1024) { toast('Template exceeds the 250 MB size limit.'); return; }
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const hex = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, '0')), parts = [];
    for (let i = 0; i < bytes.length; i += 16384) parts.push(Array.from(bytes.subarray(i, i + 16384), b => hex[b]).join(''));
    command({ command: 'ImportTemplateData', name: file.name, data: parts.join('') });
  } catch (error) { toast(error.message); }
}
// The operating system's own Open dialog: the page never sees the path or the bytes.
export function chooseTemplate(command) {
  command({ command: 'PickTemplate' });
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
