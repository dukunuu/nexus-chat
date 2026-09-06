import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import type { NexusStore } from "./state";

export const workspacePages = ["Chat", "Conversations", "Research activity", "Files", "Images/scripts", "Apps", "Management", "Usage", "Settings", "Sync"] as const;
export type WorkspacePage = typeof workspacePages[number];
interface Space { id: string; name: string }
interface WorkspaceFile { id: string; name: string; size: number; status: string }

export function SpacePicker(props: { store: NexusStore }) {
  const [spaces, setSpaces] = createSignal<Space[]>([]);
  const [name, setName] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  async function refresh() {
    try { setSpaces((await props.store.api.request<{ spaces: Space[] }>("/v1/spaces")).spaces); }
    catch (error) { setError(String(error)); }
  }
  createEffect(() => { props.store.state.snapshot?.active_space_id; void refresh(); });
  async function create() {
    if (!name().trim()) return;
    setBusy(true); setError("");
    try {
      await props.store.api.request(`/v1/spaces?name=${encodeURIComponent(name().trim())}`, { method: "POST" });
      await props.store.command({ SwitchSpace: { name: name().trim() } });
      setName(""); await refresh();
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  return <div class="space-picker"><label>Space<select aria-label="Space" value={props.store.state.snapshot?.active_space_id ?? ""} onChange={(event) => {
    const space = spaces().find((space) => space.id === event.currentTarget.value);
    if (space) void props.store.command({ SwitchSpace: { name: space.name } });
  }}><For each={spaces()}>{(space) => <option value={space.id}>{space.name}</option>}</For></select></label>
    <details><summary>Create space</summary><form onSubmit={(event) => { event.preventDefault(); void create(); }}><input aria-label="New space name" placeholder="Space name" value={name()} onInput={(event) => setName(event.currentTarget.value)} /><button disabled={busy()}>Create</button></form></details>
    <Show when={error()}><p role="alert">{error()}</p></Show>
  </div>;
}

export function FilesPage(props: { store: NexusStore }) {
  const [files, setFiles] = createSignal<WorkspaceFile[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [preview, setPreview] = createSignal<{ url: string; name: string }>();
  let generation = 0;
  function clearPreview() { const previous = preview(); if (previous) URL.revokeObjectURL(previous.url); setPreview(undefined); }
  onCleanup(() => { generation++; clearPreview(); });
  async function refresh(spaceId: string) {
    const current = ++generation;
    setBusy(true); setError(""); clearPreview(); setFiles([]);
    try {
      const result = await props.store.api.request<{ files: WorkspaceFile[] }>(`/v1/files?space_id=${encodeURIComponent(spaceId)}`);
      if (current === generation) setFiles(result.files);
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  createEffect(() => { const id = props.store.state.snapshot?.active_space_id; if (id) void refresh(id); });
  async function upload(file: File) {
    const spaceId = props.store.state.snapshot?.active_space_id;
    if (!spaceId) return;
    setBusy(true); setError("");
    try {
      if (file.size > 64 * 1024 * 1024) throw new Error("Files must be 64 MiB or smaller.");
      await props.store.api.request(`/v1/files?space_id=${encodeURIComponent(spaceId)}&name=${encodeURIComponent(file.name)}`, { method: "PUT", body: file, headers: { "Content-Type": "application/octet-stream" } });
      if (spaceId === props.store.state.snapshot?.active_space_id) await refresh(spaceId);
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  async function download(file: WorkspaceFile, showImage = false) {
    const spaceId = props.store.state.snapshot?.active_space_id;
    if (!spaceId) return;
    const current = generation;
    try {
      const blob = await props.store.api.blob(spaceId, file.name);
      if (current !== generation) return;
      const url = URL.createObjectURL(showImage ? new Blob([blob], { type: imageType(file.name) }) : blob);
      if (showImage) { clearPreview(); setPreview({ url, name: file.name }); }
      else { const link = document.createElement("a"); link.href = url; link.download = file.name; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); }
    } catch (error) { setError(String(error)); }
  }
  return <section class="workspace-page"><h1>Files</h1><p class="muted">Files in {props.store.state.snapshot?.active_space_name}. Uploads preserve existing files with the same name.</p>
    <label class="upload-button">Upload file<input type="file" disabled={busy()} onChange={(event) => { const file = event.currentTarget.files?.[0]; if (file) void upload(file); event.currentTarget.value = ""; }} /></label>
    <button disabled={busy()} onClick={() => { const id = props.store.state.snapshot?.active_space_id; if (id) void refresh(id); }}>Refresh</button>
    <Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show>
    <Show when={busy()}><p role="status">Loading…</p></Show>
    <For each={files()} fallback={<Show when={!busy()}><p class="muted">No files in this space yet.</p></Show>}>{(file) => <article class="file-row"><div><strong>{file.name}</strong><small>{new Intl.NumberFormat().format(file.size)} bytes · {file.status}</small></div><div><Show when={imageType(file.name)}><button onClick={() => void download(file, true)}>Preview</button></Show><button onClick={() => void download(file)}>Download</button></div></article>}</For>
    <Show when={preview()}>{(value) => <figure><button onClick={clearPreview}>Close preview</button><img class="file-preview" src={value().url} alt={value().name} /><figcaption>{value().name}</figcaption></figure>}</Show>
  </section>;
}
function imageType(name: string) { return ({ png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp" } as Record<string, string>)[name.split(".").pop()?.toLowerCase() ?? ""]; }
