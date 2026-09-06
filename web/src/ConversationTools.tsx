import { Show, createMemo, createSignal } from "solid-js";
import type { Message } from "./types";
import type { NexusStore } from "./state";

/** Copy/export/context controls kept beside the transcript so they work on mobile too. */
export function ConversationTools(props: { messages: Message[]; onExport?: () => void; store?: NexusStore; sessionId?: string | null }) {
  const [open, setOpen] = createSignal(false);
  const [error, setError] = createSignal("");
  const [context, setContext] = createSignal<{ system_tokens: number; memory_tokens: number; skills_tokens: number; conversation_tokens: number; limit: number | null; compacted: boolean }>();
  const text = createMemo(() => props.messages.filter((message) => message.role === "user" || message.role === "assistant").map((message) => `${message.role}: ${message.content}`).join("\n\n"));
  const estimate = createMemo(() => props.messages.reduce((total, message) => total + message.content.length, 0));
  async function copy() { try { await navigator.clipboard.writeText(text()); setError("Copied conversation."); } catch { setError("Clipboard unavailable. Use Export conversation to download it."); } }
  function exportConversation() {
    if (props.onExport) { props.onExport(); return; }
    const blob = new Blob([text()], { type: "text/markdown;charset=utf-8" });
    const url = URL.createObjectURL(blob); const link = document.createElement("a");
    link.href = url; link.download = `conversation-${props.sessionId ?? "draft"}.md`; link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  async function toggle() { const next = !open(); setOpen(next); if (next && props.store && props.sessionId) { try { setContext(await props.store.api.context(props.sessionId)); } catch { setContext(undefined); } } }
  return <div class="conversation-tools"><button aria-expanded={open()} onClick={() => void toggle()}>Tools</button><Show when={open()}><div class="tools-popover"><button onClick={() => void copy()}>Copy conversation</button><button onClick={exportConversation}>Export conversation</button><Show when={error()}><p role="status">{error()}</p></Show><Show when={context()} fallback={<p class="muted">{estimate().toLocaleString()} characters in transcript</p>}>{(value) => <p class="muted">{value().conversation_tokens.toLocaleString()} conversation tokens · {value().system_tokens.toLocaleString()} system · {value().memory_tokens.toLocaleString()} memory · {value().skills_tokens.toLocaleString()} skills{formatLimit(value().limit)}</p>}</Show></div></Show></div>;
}

function formatLimit(limit: number | null) { return limit === null ? "" : ` / ${limit.toLocaleString()}`; }
