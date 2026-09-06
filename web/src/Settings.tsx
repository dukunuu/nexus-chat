import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { NexusStore } from "./state";

type Field = { key: string; label: string; options?: string[]; type?: string; hint?: string };
const groups: { name: string; fields: Field[] }[] = [
  { name: "Display", fields: [
    { key: "show_stats", label: "Show message statistics", type: "checkbox" },
    { key: "show_reasoning", label: "Expand reasoning", type: "checkbox" },
    { key: "hide_hints", label: "Hide composer hints", type: "checkbox" },
    { key: "verbosity", label: "Answer length", options: ["normal", "concise", "caveman"] },
    { key: "usage_range", label: "Default usage range", options: ["day", "week", "month", "all"] },
  ] },
  { name: "Generation", fields: [
    { key: "temperature", label: "Temperature", type: "number", hint: "0–2; blank uses the provider default" },
    { key: "top_p", label: "Top P", type: "number", hint: "0–1; blank uses the provider default" },
    { key: "max_tokens", label: "Maximum output tokens", type: "number", hint: "Blank uses the provider default" },
    { key: "compact_threshold", label: "Auto-compact at context %", type: "number", hint: "0 disables automatic compaction" },
  ] },
  { name: "Models and media", fields: [
    { key: "memory_model", label: "Memory model" }, { key: "transcriber_model", label: "Image understanding model" },
    { key: "ocr_model", label: "OCR model" }, { key: "ocr_engine", label: "OCR engine", options: ["auto", "tesseract", "vlm", "local"] },
    { key: "local_ocr_model", label: "Local OCR model" }, { key: "embedding_model", label: "Embedding model" },
    { key: "image_gen_model", label: "Image generation model" }, { key: "video_gen_model", label: "Video generation model" },
  ] },
  { name: "Search", fields: [
    { key: "search_provider", label: "Search provider", options: ["auto", "langsearch", "searxng", "duckduckgo"] },
    { key: "searxng_url", label: "SearXNG URL", hint: "Existing URL credentials are hidden. Enter a complete URL to replace it." },
    { key: "langsearch_key", label: "LangSearch key", type: "password", hint: "Write-only. A blank unchanged field preserves the key." },
    { key: "blocked_domains", label: "Blocked domains", type: "textarea", hint: "One domain per line, for this space" },
  ] },
];

export function SettingsPage(props: { store: NexusStore }) {
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [baseline, setBaseline] = createSignal<Record<string, string>>({});
  const [loaded, setLoaded] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [notice, setNotice] = createSignal("");
  const [kind, setKind] = createSignal("instructions");
  const [document, setDocument] = createSignal<{ content: string; hash: string }>();
  const [draft, setDraft] = createSignal("");
  let generation = 0;
  const dirty = () => Object.keys(values()).some((key) => values()[key] !== baseline()[key]) || Boolean(document() && draft() !== document()!.content);
  const discard = () => !dirty() || window.confirm("Discard unsaved settings changes?");
  onCleanup(props.store.guardNavigation(discard));
  onCleanup(() => { generation++; });
  async function load() {
    const current = ++generation;
    setLoaded(false); setDocument(undefined); setError("");
    try {
      const result = await props.store.api.request<Record<string, unknown>>("/v1/settings");
      if (current !== generation) return;
      const next: Record<string, string> = {};
      for (const field of groups.flatMap((group) => group.fields)) {
        const value = result[field.key];
        next[field.key] = field.type === "password" ? "" : Array.isArray(value) ? value.join("\n") : typeof value === "boolean" ? value ? "1" : "0" : String(value ?? "");
      }
      setValues(next); setBaseline({ ...next }); setLoaded(true);
    } catch (error) { if (current === generation) setError(String(error)); }
  }
  createEffect(() => { props.store.state.snapshot?.active_space_id; void load(); });
  async function save() {
    const current = generation;
    const spaceId = props.store.state.snapshot?.active_space_id;
    const entries = Object.entries(values()).filter(([key, value]) => value !== baseline()[key]);
    setBusy(true); setError(""); setNotice("");
    try {
      for (const [key, value] of entries) {
        await props.store.api.request("/v1/settings", { method: "PUT", body: JSON.stringify({ key, value, space_id: spaceId }) });
        if (current !== generation) return;
        setBaseline((previous) => ({ ...previous, [key]: value }));
      }
      setNotice("Settings saved."); await props.store.refresh();
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  const documentPath = () => `/v1/settings/document?${new URLSearchParams({ kind: kind(), space_id: props.store.state.snapshot?.active_space_id ?? "" })}`;
  async function loadDocument(next = kind()) {
    if (document() && draft() !== document()!.content && !window.confirm("Discard unsaved document changes?")) return;
    setKind(next); setDocument(undefined); setBusy(true); setError("");
    const current = generation;
    try { const result = await props.store.api.request<{ content: string; hash: string }>(documentPath()); if (current === generation) { setDocument(result); setDraft(result.content); } }
    catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  async function saveDocument() {
    const current = generation;
    setBusy(true); setError("");
    try {
      const result = await props.store.api.request<{ content: string; hash: string }>(documentPath(), { method: "PUT", body: JSON.stringify({ content: draft(), hash: document()?.hash }) });
      if (current === generation) { setDocument(result); setNotice("Document saved."); }
    } catch (error) { if (current === generation) setError(String(error)); }
    finally { if (current === generation) setBusy(false); }
  }
  return <section class="workspace-page settings-page"><h1>Settings</h1>
    <Show when={error()}><p role="alert" class="error-banner">{error()}</p></Show><Show when={notice()}><p role="status">{notice()}</p></Show>
    <Show when={loaded()} fallback={<p>Loading settings…</p>}>
      <datalist id="settings-models"><For each={props.store.state.snapshot?.models ?? []}>{(model) => <option value={model.id}>{model.name}</option>}</For></datalist>
      <For each={groups}>{(group) => <fieldset disabled={busy()}><legend>{group.name}</legend><div class="settings-grid"><For each={group.fields}>{(field) => <label>{field.label}
        <Show when={field.type === "checkbox"} fallback={<Show when={field.options} fallback={<Show when={field.type === "textarea"} fallback={<input type={field.type ?? "text"} step="any" list={field.key.endsWith("_model") ? "settings-models" : undefined} autocomplete={field.type === "password" ? "new-password" : "off"} value={values()[field.key] ?? ""} onInput={(event) => setValues((previous) => ({ ...previous, [field.key]: event.currentTarget.value }))} />}><textarea value={values()[field.key] ?? ""} onInput={(event) => setValues((previous) => ({ ...previous, [field.key]: event.currentTarget.value }))} /></Show>}><select value={values()[field.key]} onChange={(event) => setValues((previous) => ({ ...previous, [field.key]: event.currentTarget.value }))}><For each={field.options}>{(option) => <option>{option}</option>}</For></select></Show>}><input type="checkbox" checked={values()[field.key] === "1"} onChange={(event) => setValues((previous) => ({ ...previous, [field.key]: event.currentTarget.checked ? "1" : "0" }))} /></Show>
        <Show when={field.hint}><small class="muted">{field.hint}</small></Show>
      </label>}</For></div></fieldset>}</For>
      <button disabled={busy() || !dirty()} onClick={() => void save()}>Save settings</button>
      <h2>Instructions and memory</h2><p class="muted">Space instructions and memory affect this workspace. The base system prompt affects every space.</p>
      <select aria-label="Configuration document" value={kind()} disabled={busy()} onChange={(event) => void loadDocument(event.currentTarget.value)}><option value="instructions">Space instructions</option><option value="memory">Space memory</option><option value="system">Base system prompt</option></select><button disabled={busy()} onClick={() => void loadDocument()}>Load document</button>
      <Show when={document()}><textarea aria-label="Configuration document editor" class="app-source" value={draft()} disabled={busy()} onInput={(event) => setDraft(event.currentTarget.value)} /><button disabled={busy() || draft() === document()?.content} onClick={() => void saveDocument()}>Save document</button></Show>
    </Show>
    <LoginPanel store={props.store} />
  </section>;
}

interface LoginState { pending: boolean; user_code: string | null; verification_url: string; message: string; configured: string[] }
export function LoginPanel(props: { store: NexusStore }) {
  const [backend, setBackend] = createSignal("openrouter");
  const [key, setKey] = createSignal("");
  const [login, setLogin] = createSignal<LoginState>();
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let stopped = false;
  async function refresh() {
    try { const result = await props.store.api.request<LoginState>("/v1/login"); if (!stopped) setLogin(result); }
    catch (error) { if (!stopped) setError(String(error)); }
  }
  onMount(() => { void refresh(); const timer = window.setInterval(() => void refresh(), 2000); onCleanup(() => { stopped = true; window.clearInterval(timer); }); });
  async function submit(cancel = false) {
    setBusy(true); setError("");
    try {
      await props.store.api.request("/v1/login", { method: "POST", body: JSON.stringify({ backend: backend(), key: key(), cancel }) });
      setKey(""); await refresh(); await props.store.refresh();
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  return <section class="login-panel"><h2>Provider login</h2><p class="muted">Configured: {login()?.configured.join(", ") || "none"}</p><Show when={error()}><p role="alert">{error()}</p></Show>
    <label>Provider<select value={backend()} disabled={busy() || login()?.pending} onChange={(event) => setBackend(event.currentTarget.value)}><option value="openrouter">OpenRouter</option><option value="openai">OpenAI</option><option value="opencode">OpenCode Go</option><option value="codex">OpenAI Codex</option></select></label>
    <Show when={backend() !== "codex"}><label>API key<input type="password" autocomplete="new-password" value={key()} onInput={(event) => setKey(event.currentTarget.value)} /></label></Show>
    <button disabled={busy() || login()?.pending || (backend() !== "codex" && !key().trim())} onClick={() => void submit()}>{backend() === "codex" ? "Start device login" : "Save provider key"}</button>
    <Show when={login()?.pending}><button disabled={busy()} onClick={() => { setBackend("codex"); void submit(true); }}>Cancel device login</button></Show>
    <Show when={login()?.user_code}><div class="device-code"><p>Your device code: <strong>{login()?.user_code}</strong></p><a href="https://auth.openai.com/codex/device" target="_blank" rel="noopener noreferrer">Open verification page ↗</a></div></Show>
    <p role="status">{login()?.message}</p>
  </section>;
}
