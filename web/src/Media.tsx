import { For, Show, createEffect, createSignal, onCleanup, untrack } from "solid-js";
import type { NexusStore } from "./state";

type Kind = "files" | "images" | "videos" | "scripts";
interface MediaEntry { name: string; size: number; modified: string; mime: string; kind: string; hash?: string }
interface Catalog { space_id: string; files: MediaEntry[]; images: MediaEntry[]; videos: MediaEntry[]; scripts: MediaEntry[] }

/** The media workspace mirrors the TUI's files, images, and scripts flows. */
export function MediaPage(props: { store: NexusStore; onAttach: (markdown: string) => void }) {
  const [kind, setKind] = createSignal<Kind>("files");
  const [catalog, setCatalog] = createSignal<Catalog>();
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [preview, setPreview] = createSignal<{ url: string; name: string }>();
  const [editing, setEditing] = createSignal<{ name: string; content: string; original: string; hash?: string }>();
  const [output, setOutput] = createSignal("");
  const [prompt, setPrompt] = createSignal("");
  let refreshSerial = 0;
  const activeSpace = () => props.store.state.snapshot?.active_space_id;
  const stillCurrent = (space: string) => activeSpace() === space;
  async function refresh() {
    const space = activeSpace(); if (!space) return;
    const serial = ++refreshSerial; setBusy(true); setError("");
    try { const next = await props.store.api.request<Catalog>(`/v1/media?space_id=${encodeURIComponent(space)}`); if (serial === refreshSerial && stillCurrent(space)) setCatalog(next); }
    catch (reason) { if (serial === refreshSerial && stillCurrent(space)) setError(String(reason)); }
    finally { if (serial === refreshSerial) setBusy(false); }
  }
  let lastSpace: string | undefined;
  createEffect(() => {
    const space = activeSpace();
    if (space !== lastSpace) {
      lastSpace = space;
      setCatalog(undefined); setEditing(undefined); setOutput("");
      const value = untrack(preview);
      if (value) URL.revokeObjectURL(value.url);
      setPreview(undefined);
    }
    void refresh();
  });
  onCleanup(props.store.guardNavigation(() => { const value = editing(); return !value || value.content === value.original || window.confirm("Discard unsaved script changes?"); }));
  onCleanup(() => { const value = preview(); if (value) URL.revokeObjectURL(value.url); refreshSerial += 1; });
  const rows = () => catalog()?.[kind()] ?? [];
  async function upload(file: File) {
    const space = activeSpace(); if (!space) return;
    if (file.size > 64 * 1024 * 1024) { setError("Files must be 64 MiB or smaller."); return; }
    setBusy(true); setError("");
    try { await props.store.api.request(`/v1/media?space_id=${encodeURIComponent(space)}&kind=${kind() === "images" ? "image" : kind() === "scripts" ? "script" : "file"}&name=${encodeURIComponent(file.name)}`, { method: "PUT", body: file, headers: { "Content-Type": "application/octet-stream" } }); if (stillCurrent(space)) await refresh(); }
    catch (reason) { if (stillCurrent(space)) setError(String(reason)); } finally { setBusy(false); }
  }
  async function remove(entry: MediaEntry) {
    const space = activeSpace(); if (!space || !window.confirm(`Delete ${entry.name}?`)) return;
    try { await props.store.api.request(`/v1/media?space_id=${encodeURIComponent(space)}&kind=${entry.kind}&name=${encodeURIComponent(entry.name)}`, { method: "DELETE" }); if (stillCurrent(space)) await refresh(); } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function rename(entry: MediaEntry) {
    const space = activeSpace(); const next = window.prompt("Rename to", entry.name); if (!space || !next || next === entry.name) return;
    try { await props.store.api.request(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "rename", kind: entry.kind, from: entry.name, to: next }) }); if (stillCurrent(space)) await refresh(); } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function ocr(entry: MediaEntry) {
    const space = activeSpace(); if (!space) return;
    try { await props.store.api.request(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "ocr", kind: "file", name: entry.name }) }); if (stillCurrent(space)) { setOutput(`OCR queued for ${entry.name}`); await refresh(); } } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function attach(entry: MediaEntry) {
    const space = activeSpace(); if (!space) return;
    try { const result = await props.store.api.request<{ markdown: string; data_url: string }>(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "attach", kind: entry.kind, name: entry.name }) }); if (!stillCurrent(space)) return; props.onAttach(result.markdown); setOutput(`Attached ${entry.name}`); if (entry.kind === "image") { const previous = preview(); if (previous) URL.revokeObjectURL(previous.url); setPreview({ url: result.data_url, name: entry.name }); } } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function download(entry: MediaEntry) {
    const space = activeSpace(); if (!space) return;
    try { const blob = await props.store.api.binary(`/v1/media/blob?space_id=${encodeURIComponent(space)}&kind=${encodeURIComponent(entry.kind)}&name=${encodeURIComponent(entry.name)}`); if (!stillCurrent(space)) return; const url = URL.createObjectURL(blob); if (entry.kind === "image") { const previous = preview(); if (previous) URL.revokeObjectURL(previous.url); setPreview({ url, name: entry.name }); } else { const link = document.createElement("a"); link.href = url; link.download = entry.name; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); } } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function readScript(entry: MediaEntry) {
    const space = activeSpace(); if (!space) return;
    try { const value = await props.store.api.request<{ content: string; hash: string }>(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "read_script", kind: "script", name: entry.name }) }); if (stillCurrent(space)) setEditing({ name: entry.name, content: value.content, original: value.content, hash: value.hash }); } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function saveScript() {
    const space = activeSpace(); const value = editing(); if (!space || !value) return;
    try { await props.store.api.request(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "write_script", kind: "script", name: value.name, content: value.content, hash: value.hash }) }); if (stillCurrent(space)) { setEditing(undefined); await refresh(); } } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function runScript(entry: MediaEntry) {
    const space = activeSpace(); if (!space || !window.confirm(`Run ${entry.name}?`)) return;
    try { const result = await props.store.api.request<{ code?: number | null; stdout?: string; stderr?: string; program?: string }>(`/v1/media/action?space_id=${encodeURIComponent(space)}`, { method: "POST", body: JSON.stringify({ action: "run_script", kind: "script", name: entry.name, args: [] }) }); if (stillCurrent(space)) setOutput(result.code === undefined ? `Execution queued (${result.program ?? "script"}). Watch the workspace status for completion.` : `${result.stdout ?? ""}${result.stderr ? `\n${result.stderr}` : ""}\n(exit ${result.code ?? "signal"})`); } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function runTool(name: string, args: Record<string, unknown>, label: string, needsPrompt = true) {
    const space = activeSpace(); if (!space || (needsPrompt && !prompt().trim())) { setError("Enter a prompt first."); return; }
    setBusy(true); setError("");
    try { const result = await props.store.api.runTool(space, name, needsPrompt ? { ...args, prompt: prompt().trim() } : args); if (stillCurrent(space)) { setOutput(`${label}: ${result}`); await refresh(); } } catch (reason) { if (stillCurrent(space)) setError(String(reason)); } finally { setBusy(false); }
  }
  async function transcribe(entry: MediaEntry) {
    const space = activeSpace(); if (!space) return;
    try { const result = await props.store.api.runTool(space, "files", { action: "read", name: entry.name }); if (stillCurrent(space)) setOutput(result); } catch (reason) { if (stillCurrent(space)) setError(String(reason)); }
  }
  async function transformVideo(entry: MediaEntry) {
    const value = window.prompt("Playback speed (for example 1.5)", "1.5");
    const speed = value ? Number(value) : Number.NaN;
    if (!Number.isFinite(speed) || speed <= 0) { setError("Enter a positive playback speed."); return; }
    await runTool("video_transform", { action: "edit", video_id: entry.name, speed }, "Video transformed", false);
  }
  return <section class="workspace-page"><h1>Media workspace</h1><p class="muted">Files, images, and scripts in {props.store.state.snapshot?.active_space_name}.</p>
    <div class="media-tools"><label>Prompt<input value={prompt()} onInput={(event) => setPrompt(event.currentTarget.value)} placeholder="Describe an image or video" /></label><button onClick={() => void runTool("media", { action: "generate_image" }, "Image generated")} disabled={busy()}>Generate image</button><button onClick={() => void runTool("media", { action: "generate_video" }, "Video generated")} disabled={busy()}>Generate video</button></div>
    <div role="tablist" aria-label="Media type"><For each={(["files", "images", "videos", "scripts"] as Kind[])}>{(value) => <button role="tab" aria-selected={kind() === value} onClick={() => { setKind(value); setEditing(undefined); }}>{value[0].toUpperCase() + value.slice(1)}</button>}</For></div>
    <label class="upload-button">Upload<input type="file" disabled={busy()} onChange={(event) => { const file = event.currentTarget.files?.[0]; if (file) void upload(file); event.currentTarget.value = ""; }} /></label><button onClick={() => void refresh()} disabled={busy()}>Refresh</button>
    <Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show>
    <For each={rows()} fallback={<Show when={!busy()}><p class="muted">No {kind()} yet.</p></Show>}>{(entry) => <article class="file-row"><div><strong>{entry.name}</strong><small>{new Intl.NumberFormat().format(entry.size)} bytes · {entry.modified}</small></div><div><Show when={entry.kind === "image"}><button onClick={() => void download(entry)}>Preview</button><button onClick={() => void attach(entry)}>Attach</button></Show><Show when={entry.kind === "video"}><button onClick={() => void download(entry)}>Download</button><button onClick={() => void attach(entry)}>Attach</button><button onClick={() => void transformVideo(entry)} disabled={busy()}>Transform</button></Show><Show when={entry.kind === "file"}><button onClick={() => void download(entry)}>Download</button><button onClick={() => void ocr(entry)}>OCR</button><button onClick={() => void transcribe(entry)}>Read text</button></Show><Show when={entry.kind === "script"}><button onClick={() => void readScript(entry)}>Edit</button><button onClick={() => void runScript(entry)}>Run</button></Show><button onClick={() => void rename(entry)}>Rename</button><button onClick={() => void remove(entry)}>Delete</button></div></article>}</For>
    <Show when={preview()}>{(value) => <figure><button onClick={() => { URL.revokeObjectURL(value().url); setPreview(undefined); }}>Close preview</button><img class="file-preview" src={value().url} alt={value().name} /><figcaption>{value().name}</figcaption></figure>}</Show>
    <Show when={editing()}>{(value) => <form onSubmit={(event) => { event.preventDefault(); void saveScript(); }}><h2>Edit {value().name}</h2><textarea value={value().content} onInput={(event) => setEditing({ name: value().name, content: event.currentTarget.value, original: value().original, hash: value().hash })} /><button type="submit">Save</button><button type="button" onClick={() => setEditing(undefined)}>Cancel</button></form>}</Show>
    <Show when={output()}><pre class="tool-output">{output()}</pre></Show>
  </section>;
}
