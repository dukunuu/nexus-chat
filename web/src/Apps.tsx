import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import type { NexusStore } from "./state";
interface AppEntry { name: string; url: string | null; served_from: string | null }
interface Source { content: string; hash: string }

export function AppsPage(props: { store: NexusStore }) {
  const [apps, setApps] = createSignal<AppEntry[]>([]);
  const [selected, setSelected] = createSignal<AppEntry>();
  const [files, setFiles] = createSignal<string[]>([]);
  const [path, setPath] = createSignal("");
  const [source, setSource] = createSignal<Source>();
  const [text, setText] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [preview, setPreview] = createSignal(false);
  const [revision, setRevision] = createSignal(0);
  const [buildOutput, setBuildOutput] = createSignal("");
  let generation = 0;
  const dirty = () => source() !== undefined && source()!.content !== text();
  const discard = () => !dirty() || window.confirm("Discard unsaved app edits?");
  onCleanup(props.store.guardNavigation(discard));
  const beforeUnload = (event: BeforeUnloadEvent) => { if (dirty()) { event.preventDefault(); event.returnValue = ""; } };
  window.addEventListener("beforeunload", beforeUnload);
  onCleanup(() => { generation++; window.removeEventListener("beforeunload", beforeUnload); });
  function query(name?: string, file?: string) {
    const params = new URLSearchParams({ space_id: props.store.state.snapshot?.active_space_id ?? "" });
    if (name) params.set("name", name);
    if (file) params.set("path", file);
    return params.toString();
  }
  async function catalog() {
    const current = ++generation;
    setBusy(true); setError(""); setApps([]); setSelected(undefined); setSource(undefined); setFiles([]); setPath(""); setPreview(false);
    try {
      const result = await props.store.api.request<{ apps: AppEntry[] }>(`/v1/apps?${query()}`);
      if (current === generation) setApps(result.apps);
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  createEffect(() => { const id = props.store.state.snapshot?.active_space_id; if (id) void catalog(); });
  async function select(app: AppEntry) {
    if (!discard()) return;
    const current = ++generation;
    setSelected(app); setSource(undefined); setFiles([]); setPath(""); setPreview(false); setBusy(true); setError("");
    try {
      const result = await props.store.api.request<{ files: string[] }>(`/v1/apps/files?${query(app.name)}`);
      if (current === generation) setFiles(result.files);
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  async function load(file: string) {
    if (!discard() || !selected()) return;
    const current = ++generation;
    setPath(file); setSource(undefined); setBusy(true); setError("");
    try {
      const result = await props.store.api.request<Source>(`/v1/apps/source?${query(selected()!.name, file)}`);
      if (current === generation) { setSource(result); setText(result.content); }
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  async function save() {
    if (!source() || !selected()) return;
    const current = generation;
    setBusy(true); setError("");
    try {
      const result = await props.store.api.request<Source>(`/v1/apps/source?${query(selected()!.name, path())}`, { method: "PUT", body: JSON.stringify({ content: text(), hash: source()!.hash }) });
      if (current === generation) { setSource(result); setText(result.content); setRevision((value) => value + 1); }
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  async function remove() {
    const app = selected();
    if (!app || !window.confirm(`Delete app “${app.name}” and all its files?`)) return;
    setBusy(true); setError("");
    try { await props.store.api.request(`/v1/apps?${query(app.name)}`, { method: "DELETE" }); setSelected(undefined); await catalog(); }
    catch (error) { setError(String(error)); } finally { setBusy(false); }
  }
  async function register() {
    const app = selected(); if (!app) return;
    setBusy(true); setError("");
    try { await props.store.api.request(`/v1/apps?${query(app.name)}`, { method: "POST" }); await catalog(); }
    catch (error) { setError(String(error)); } finally { setBusy(false); }
  }
  async function build() {
    const app = selected(); const space = props.store.state.snapshot?.active_space_id;
    if (!app || !space || !discard()) return;
    if (dirty()) setText(source()!.content);
    setBusy(true); setError(""); setBuildOutput("Building…");
    const current = generation;
    try {
      const output = await props.store.api.runTool(space, "app", { action: "build", app: app.name });
      if (current === generation) { setBuildOutput(output); setRevision((value) => value + 1); }
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  const appUrl = () => selected()?.url ? new URL(selected()!.url!, props.store.api.baseUrl).href : "";
  return <section class="workspace-page apps-page"><h1>Apps</h1><p class="muted">Apps in {props.store.state.snapshot?.active_space_name}</p>
    <button disabled={busy()} onClick={() => { if (discard()) void catalog(); }}>Refresh apps</button>
    <Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show>
    <Show when={busy()}><p role="status">Loading…</p></Show>
    <div class="app-catalog"><For each={apps()} fallback={<Show when={!busy()}><p class="muted">No apps in this space. Ask Nexus to build one in Chat.</p></Show>}>{(app) => <button classList={{ selected: selected()?.name === app.name }} disabled={busy()} onClick={() => void select(app)}>{app.name}</button>}</For></div>
    <Show when={selected()}>{(app) => <div class="app-workbench"><h2>{app().name}</h2>
      <Show when={app().url} fallback={<><p class="muted">App server unavailable or app not registered.</p><button disabled={busy()} onClick={() => void register()}>Register app</button></>}>
        <a class="app-launch" href={appUrl()} target="_blank" rel="noopener noreferrer">Launch app ↗</a>
        <button onClick={() => setPreview(!preview())}>{preview() ? "Close preview" : "Preview app"}</button>
      </Show>
      <Show when={app().served_from}><p class="muted">Preview serves {app().served_from}/. Source changes need a rebuild before they appear.</p></Show>
      <button disabled={busy()} onClick={() => void build()}>Build app</button>
      <Show when={buildOutput()}><pre class="app-build-output" role="status">{buildOutput()}</pre></Show>
      <Show when={preview()}><iframe title={`${app().name} preview`} src={`${appUrl()}?revision=${revision()}`} sandbox="allow-scripts allow-forms allow-downloads allow-popups" referrerpolicy="no-referrer" /></Show>
      <label>Source file<select aria-label="App source file" value={path()} disabled={busy()} onChange={(event) => void load(event.currentTarget.value)}><option value="" disabled>Select a file</option><For each={files()}>{(file) => <option value={file}>{file}</option>}</For></select></label>
      <Show when={source()}><textarea class="app-source" aria-label="App source editor" spellcheck={false} value={text()} disabled={busy()} onInput={(event) => setText(event.currentTarget.value)} /><div class="app-editor-actions"><button disabled={busy() || !dirty()} onClick={() => void save()}>Save file</button><button disabled={busy()} onClick={() => void load(path())}>Reload file</button><span role="status">{dirty() ? "Unsaved changes" : "Saved"}</span></div></Show>
      <button class="danger" disabled={busy() || dirty()} onClick={() => void remove()}>Delete app</button>
    </div>}</Show>
  </section>;
}
