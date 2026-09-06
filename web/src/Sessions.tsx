import { For, Show, createSignal } from "solid-js";
import type { NexusStore } from "./state";

/** Session list actions for thin clients; the shell decides where to mount it. */
export function SessionsPage(props: { store: NexusStore }) {
  const [editing, setEditing] = createSignal<string>();
  const [title, setTitle] = createSignal("");
  const [error, setError] = createSignal("");
  const space = () => props.store.state.snapshot?.active_space_id;
  async function rename(id: string) {
    if (!space() || !title().trim()) return;
    try { await props.store.api.request(`/v1/sessions/${encodeURIComponent(id)}`, { method: "PATCH", body: JSON.stringify({ space_id: space(), title: title().trim() }) }); setEditing(undefined); await props.store.refresh(); }
    catch (value) { setError(String(value)); }
  }
  async function remove(id: string) {
    if (!space() || !window.confirm("Delete this conversation and its messages?")) return;
    try { await props.store.api.request(`/v1/sessions/${encodeURIComponent(id)}`, { method: "DELETE", body: JSON.stringify({ space_id: space() }) }); await props.store.refresh(); }
    catch (value) { setError(String(value)); }
  }
  return <section class="workspace-page"><h1>Conversations</h1><For each={props.store.state.snapshot?.sessions ?? []}>{(session) => <article class="file-row"><Show when={editing() === session.id} fallback={<strong>{session.title || "Untitled conversation"}</strong>}>{<input aria-label="Conversation title" value={title()} onInput={(event) => setTitle(event.currentTarget.value)} onKeyDown={(event) => { if (event.key === "Enter") void rename(session.id); }} />}</Show><div><Show when={editing() === session.id} fallback={<button onClick={() => { setEditing(session.id); setTitle(session.title); }}>Rename</button>}><button onClick={() => void rename(session.id)}>Save</button></Show><button disabled={props.store.state.activeSessionId === session.id} onClick={() => void remove(session.id)}>Delete</button></div></article>}</For><Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show></section>;
}
