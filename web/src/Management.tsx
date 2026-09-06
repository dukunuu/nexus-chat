import { For, Show, createEffect, createSignal } from "solid-js";
import type { NexusStore } from "./state";

type Persona = { name: string; model: string; blurb: string };
type Skill = { name: string; description: string; managed: boolean; body?: string };
type Watch = { id: string; topic: string; interval_hours: number; session_id: string; last_run_at?: string };

/** Web management panel for the app's swarm, skills, and standing watches. */
export function ManagementPage(props: { store: NexusStore }) {
  const [tab, setTab] = createSignal<"swarm" | "skills" | "watches">("swarm");
  return <section class="workspace-page management-page"><div class="management-tabs" role="tablist">
    <For each={[["swarm", "Swarm"], ["skills", "Skills"], ["watches", "Watches"]] as const}>{(item) => <button role="tab" aria-selected={tab() === item[0]} onClick={() => setTab(item[0])}>{item[1]}</button>}</For>
  </div><Show when={tab() === "swarm"}><SwarmPanel store={props.store} /></Show><Show when={tab() === "skills"}><SkillsPanel store={props.store} /></Show><Show when={tab() === "watches"}><WatchesPanel store={props.store} /></Show></section>;
}

function SwarmPanel(props: { store: NexusStore }) {
  const [personas, setPersonas] = createSignal<Persona[]>([]);
  const [enabled, setEnabled] = createSignal(false);
  const [error, setError] = createSignal("");
  const session = () => props.store.state.activeSessionId;
  let refreshVersion = 0;
  async function refresh() { const id = session(); if (!id) { setPersonas([]); setEnabled(false); return; } const version = ++refreshVersion; setPersonas([]); setEnabled(false); try { const value = await props.store.api.request<{ personas: Persona[]; enabled: boolean }>(`/v1/swarm?session_id=${encodeURIComponent(id)}`); if (version === refreshVersion && session() === id) { setPersonas(value.personas); setEnabled(value.enabled); } } catch (value) { if (version === refreshVersion && session() === id) setError(String(value)); } }
  createEffect(() => { session(); void refresh(); });
  const space = () => props.store.state.snapshot?.active_space_id;
  async function save() { const id = session(); if (!id || !space()) return; try { await props.store.api.request("/v1/swarm/roster", { method: "PUT", body: JSON.stringify({ space_id: space(), session_id: id, personas: personas() }) }); } catch (value) { setError(String(value)); } }
  async function mode(value: boolean) { const id = session(); if (!id || !space()) return; try { await props.store.api.request("/v1/swarm/mode", { method: "POST", body: JSON.stringify({ space_id: space(), session_id: id, enabled: value }) }); setEnabled(value); } catch (error) { setError(String(error)); } }
  async function run(action: "start" | "stop") { const id = session(); if (!id || !space()) return; try { const result = await props.store.api.request<{ running: boolean }>(`/v1/swarm/${action}`, { method: "POST", body: JSON.stringify({ space_id: space(), session_id: id }) }); if (action === "start" && !result.running) setError("Swarm could not start; check the active model and roster."); } catch (value) { setError(String(value)); } }
  return <div><h1>Swarm</h1><p class="muted">Configure the panel for the active conversation.</p><label><input type="checkbox" checked={enabled()} onChange={(event) => void mode(event.currentTarget.checked)} /> Swarm mode</label> <button onClick={() => void run("start")}>Start</button> <button onClick={() => void run("stop")}>Stop</button><For each={personas()}>{(persona, index) => <div class="file-row"><input aria-label="Persona name" value={persona.name} onInput={(event) => { const rows = personas().slice(); rows[index()].name = event.currentTarget.value; setPersonas(rows); }} /><input aria-label="Persona model" value={persona.model} onInput={(event) => { const rows = personas().slice(); rows[index()].model = event.currentTarget.value; setPersonas(rows); }} /><input aria-label="Persona blurb" value={persona.blurb} onInput={(event) => { const rows = personas().slice(); rows[index()].blurb = event.currentTarget.value; setPersonas(rows); }} /><button onClick={() => setPersonas(personas().filter((_, row) => row !== index()))}>Remove</button></div>}</For><button onClick={() => setPersonas([...personas(), { name: "", model: "", blurb: "" }])}>Add persona</button> <button onClick={() => void save()}>Save roster</button><Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show></div>;
}

function SkillsPanel(props: { store: NexusStore }) {
  const [skills, setSkills] = createSignal<Skill[]>([]); const [detail, setDetail] = createSignal<Skill>(); const [source, setSource] = createSignal(""); const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false); const [status, setStatus] = createSignal("");
  async function refresh() { try { setSkills((await props.store.api.request<{ skills: Skill[] }>("/v1/skills")).skills); } catch (value) { setError(String(value)); } }
  createEffect(() => { void refresh(); });
  async function show(skill: Skill) { try { setDetail(await props.store.api.request<Skill>(`/v1/skills/${encodeURIComponent(skill.name)}?detail=1`)); } catch (value) { setError(String(value)); } }
  const space = () => props.store.state.snapshot?.active_space_id;
  async function arm(name: string) { try { await props.store.api.request("/v1/skills/arm", { method: "POST", body: JSON.stringify({ space_id: space(), name }) }); } catch (value) { setError(String(value)); } }
  async function remove(name: string) { try { await props.store.api.request(`/v1/skills/${encodeURIComponent(name)}`, { method: "DELETE", body: JSON.stringify({ space_id: space() }) }); await refresh(); } catch (value) { setError(String(value)); } }
  async function install() { const value = source().trim(); if (!value || busy()) return; setBusy(true); setStatus("Installing skill…"); setError(""); try { await props.store.api.request("/v1/skills/install", { method: "POST", body: JSON.stringify({ space_id: space(), source: value }) }); setSource(""); await refresh(); setStatus("Skill installed."); } catch (value) { setError(String(value)); setStatus(""); } finally { setBusy(false); } }
  return <div><h1>Skills</h1><div class="file-row"><input aria-label="GitHub skill source" placeholder="owner/repo/path" value={source()} onInput={(event) => setSource(event.currentTarget.value)} /><button disabled={busy() || !source().trim()} onClick={() => void install()}>Install</button><button disabled={busy()} onClick={() => void refresh()}>Refresh</button></div><Show when={status()}><p role="status">{status()}</p></Show><For each={skills()}>{(skill) => <article class="file-row"><div><strong>{skill.name}</strong><small>{skill.description}</small></div><div><button onClick={() => void show(skill)}>Details</button><button onClick={() => void arm(skill.name)}>Arm</button><Show when={skill.managed}><button onClick={() => void remove(skill.name)}>Remove</button></Show></div></article>}</For><Show when={detail()}>{(value) => <article><h2>{value().name}</h2><pre>{value().body}</pre></article>}</Show><Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show></div>;
}

function WatchesPanel(props: { store: NexusStore }) {
  const [watches, setWatches] = createSignal<Watch[]>([]); const [topic, setTopic] = createSignal(""); const [interval, setInterval] = createSignal(24); const [error, setError] = createSignal("");
  async function refresh() { try { setWatches((await props.store.api.request<{ watches: Watch[] }>("/v1/watches")).watches); } catch (value) { setError(String(value)); } }
  createEffect(() => { props.store.state.snapshot?.active_space_id; void refresh(); });
  const space = () => props.store.state.snapshot?.active_space_id;
  async function create() { if (!space() || !topic().trim()) return; try { await props.store.api.request("/v1/watches", { method: "POST", body: JSON.stringify({ space_id: space(), topic: topic().trim(), interval_hours: interval() }) }); setTopic(""); await refresh(); } catch (value) { setError(String(value)); } }
  async function remove(id: string) { try { await props.store.api.request(`/v1/watches/${encodeURIComponent(id)}`, { method: "DELETE", body: JSON.stringify({ space_id: space() }) }); await refresh(); } catch (value) { setError(String(value)); } }
  async function run(id: string) { try { const result = await props.store.api.request<{ started: boolean }>(`/v1/watches/${encodeURIComponent(id)}/run`, { method: "POST", body: JSON.stringify({ space_id: space() }) }); if (!result.started) setError("Watch could not start; check the configured model."); } catch (value) { setError(String(value)); } }
  return <div><h1>Watches</h1><div class="file-row"><input aria-label="Watch topic" placeholder="Topic" value={topic()} onInput={(event) => setTopic(event.currentTarget.value)} /><input aria-label="Interval hours" type="number" min="1" value={interval()} onInput={(event) => setInterval(Number(event.currentTarget.value))} /><button onClick={() => void create()}>Create</button></div><For each={watches()}>{(watch) => <article class="file-row"><div><strong>{watch.topic}</strong><small>Every {watch.interval_hours}h · {watch.last_run_at ? `Last run ${watch.last_run_at}` : "Never run"}</small></div><div><button onClick={() => void run(watch.id)}>Run now</button><button onClick={() => void remove(watch.id)}>Delete</button></div></article>}</For><Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show></div>;
}
