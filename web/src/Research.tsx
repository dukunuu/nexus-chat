import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { NexusStore } from "./state";
import type { Citation, Gate, PlanQuestion, ResearchEvent, ResearchSnapshot, ResearchStage } from "./types";
import { markdown } from "./markdown";

/** Full research workspace: reconnectable stages, gates, steering and report sources. */
export function ResearchPage(props: { store: NexusStore; sessionId: string | null }) {
  const state = props.store.state;
  const [steer, setSteer] = createSignal("");
  const [topic, setTopic] = createSignal("");
  const [citations, setCitations] = createSignal<Citation[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [loaded, setLoaded] = createSignal<ResearchSnapshot>();
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let generation = 0;
  const session = createMemo(() => loaded() ?? (props.sessionId ? state.snapshot?.research?.[props.sessionId] : undefined));
  const live = createMemo(() => props.sessionId ? state.researchBySession[props.sessionId] : undefined);
  const projection = createMemo(() => mergeResearch(session(), loaded() ? undefined : live()?.update, props.sessionId ?? ""));

  async function loadSources(id: string) {
    setLoading(true);
    try {
      const response = await props.store.api.request<{ citations: Citation[] }>(`/v1/research/${encodeURIComponent(id)}/citations`);
      setCitations(response.citations);
    } catch {
      // Older hosts expose sources through the snapshot only.
      setCitations(session()?.citations ?? []);
    } finally { setLoading(false); }
  }
  createEffect(() => {
    const id = props.sessionId;
    const current = ++generation;
    setLoaded(undefined); setCitations([]); setError("");
    if (!id) return;
    let pending = false;
    const refresh = async () => {
      if (pending) return;
      pending = true;
      try {
        const result = await props.store.api.research(id);
        if (current === generation) { setLoaded(result); setCitations(result.citations); }
      } catch (error) { if (current === generation) setError(String(error)); }
      finally { pending = false; }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 1000);
    onCleanup(() => { generation++; window.clearInterval(timer); });
  });

  async function action(name: string, payload: unknown) {
    if (!props.sessionId) return false;
    const id = props.sessionId;
    const current = generation;
    setBusy(true); setError("");
    try {
      await props.store.api.request(`/v1/research/${encodeURIComponent(id)}/${name}`, { method: "POST", body: JSON.stringify(payload) });
      if (current === generation) setLoaded(await props.store.api.research(id));
      await props.store.refresh();
      return true;
    } catch (error) { if (current === generation) setError(String(error)); return false; }
    finally { setBusy(false); }
  }

  async function sendSteer(event: SubmitEvent) {
    event.preventDefault();
    const text = steer().trim();
    if (!text) return;
    if (await action("steer", { text })) setSteer("");
  }
  async function start(event: SubmitEvent) {
    event.preventDefault();
    const value = topic().trim();
    if (!value) return;
    setBusy(true); setError("");
    try {
    if (props.sessionId) {
      await props.store.api.request(`/v1/research/${encodeURIComponent(props.sessionId)}/start`, { method: "POST", body: JSON.stringify({ topic: value, gated: true }) });
      await props.store.refresh();
    } else {
      await props.store.api.request("/v1/research/start", { method: "POST", body: JSON.stringify({ space_id: props.store.state.snapshot?.active_space_id, topic: value, gated: true }) });
      await props.store.refresh();
    }
    setTopic("");
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  async function answer(text: string) {
    return action("answer", { text });
  }
  const report = createMemo(() => projection().report);
  function exportReport() {
    const sources = projection().citations.map((citation, index) => `${index + 1}. ${citation.title} — ${citation.url}`).join("\n");
    const value = `${report() ?? ""}${sources ? `\n\n## Sources\n\n${sources}\n` : ""}`;
    const url = URL.createObjectURL(new Blob([value], { type: "text/markdown;charset=utf-8" }));
    const link = document.createElement("a"); link.href = url; link.download = `research-${props.sessionId ?? "report"}.md`; link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return <section class="workspace-page research-page">
    <header class="workspace-heading"><div><p class="eyebrow">RESEARCH ACTIVITY</p><h1>{projection().topic || "Research"}</h1></div><Show when={projection().running}><span class="pulse-label">● LIVE</span></Show></header>
    <Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show>
    <Show when={projection().running}><button disabled={busy()} onClick={() => void action("stop", {})}>Stop research</button></Show>
    <Show when={projection().stages.length > 0} fallback={<p class="muted">No research activity for this session yet.</p>}>
      <div class="research-stages" aria-live="polite"><For each={projection().stages}>{(stage) => <article class="research-stage"><span class="stage-marker">{stage.label === "done" ? "✓" : "•"}</span><div><strong>{stage.label}</strong><p>{stage.detail}</p></div></article>}</For></div>
    </Show>
    <Show when={!projection().running}><form class="steer-form" onSubmit={start}><label for="research-topic">Start research</label><div><input id="research-topic" value={topic()} onInput={(event) => setTopic(event.currentTarget.value)} placeholder="Research topic…" /><button disabled={busy() || !topic().trim()}>Start</button></div></form></Show>
    <Show when={projection().gate}>{(gate) => <GatePanel gate={gate()} questions={projection().questions} onAnswer={answer} />}</Show>
    <form class="steer-form" onSubmit={sendSteer}><label for="research-steer">Steer this research</label><div><input id="research-steer" value={steer()} onInput={(event) => setSteer(event.currentTarget.value)} placeholder="Look into another angle…" disabled={!projection().running} /><button disabled={busy() || !projection().running || !steer().trim()}>Queue steer</button></div></form>
    <Show when={projection().steers.length}><h2>Queued steering</h2><For each={projection().steers}>{(text) => <p>{text}</p>}</For></Show>
    <Show when={report()}>{(value) => <article class="research-report"><div class="report-actions"><h2>Final report</h2><button onClick={() => void copyText(value()).catch(() => setError("Clipboard unavailable. Export the report instead."))}>Copy report</button><button onClick={exportReport}>Export markdown</button></div><div class="markdown" innerHTML={markdown(value())} /></article>}</Show>
    <section class="research-citations"><div class="report-actions"><h2>Sources</h2><button onClick={() => props.sessionId && void loadSources(props.sessionId)} disabled={loading()}>{loading() ? "Loading…" : "Refresh"}</button></div><For each={citations().length ? citations() : projection().citations} fallback={<p class="muted">Sources appear as the report is verified.</p>}>{(citation, index) => <a class="citation-row" href={citation.url} target="_blank" rel="noreferrer"><span>[{index() + 1}]</span><span><strong>{citation.title || citation.url}</strong><small>{citation.url}</small></span>↗</a>}</For></section>
  </section>;
}

function GatePanel(props: { gate: Gate; questions: string[]; onAnswer: (text: string) => Promise<boolean> }) {
  const [answer, setAnswer] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const phase = Object.keys(props.gate.phase)[0] ?? "Clarify";
  const plan = "Approve" in props.gate.phase;
  return <article class="research-gate"><p class="eyebrow">{plan ? "PLAN APPROVAL" : "CLARIFYING QUESTIONS"}</p><Show when={props.questions.length}><For each={props.questions}>{(question) => <p>{question}</p>}</For></Show><textarea aria-label={`${phase} response`} value={answer()} onInput={(event) => setAnswer(event.currentTarget.value)} placeholder={plan ? "Approve, or describe changes…" : "Answer the questions…"} /><button disabled={busy()} onClick={async () => { setBusy(true); if (await props.onAnswer(answer())) setAnswer(""); setBusy(false); }}>Send response</button></article>;
}

export function mergeResearch(snapshot: ResearchSnapshot | undefined, event: ResearchEvent | undefined, sessionId: string): { topic: string; stages: ResearchStage[]; gate: Gate | null; steers: string[]; report: string | null; citations: Citation[]; running: boolean; questions: string[] } {
  const base = snapshot ?? { session_id: sessionId, topic: "", stages: [], gate: null, steers: [], report: null, citations: [], running: false };
  if (!event) return { ...base, questions: base.gate?.questions ?? [] };
  if ("Stage" in event) {
    const next = [...base.stages]; const value = event.Stage; const index = next.findIndex((stage) => stage.label === value.label);
    if (index >= 0) next[index] = { ...next[index], detail: value.detail }; else next.push(value);
    return { ...base, stages: next, running: true, questions: [] };
  }
  if ("SurveyReady" in event) return { ...base, gate: { session_id: sessionId, phase: { Clarify: { round: event.SurveyReady.round } } }, running: true, questions: event.SurveyReady.questions };
  if ("PlanReady" in event) return { ...base, gate: { session_id: sessionId, phase: { Approve: { rework: event.PlanReady.rework } } }, running: true, questions: event.PlanReady.questions.map((question: PlanQuestion) => question.question) };
  if ("Done" in event) return { ...base, gate: null, report: typeof event.Done === "string" ? event.Done : base.report, running: false, questions: [] };
  return { ...base, questions: base.gate?.questions ?? [] };
}

async function copyText(text: string) { await navigator.clipboard?.writeText(text); }
