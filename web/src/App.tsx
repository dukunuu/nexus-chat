import { AppsPage } from "./Apps";
import { ResearchPage } from "./Research";
import { MediaPage } from "./Media";
import { ManagementPage } from "./Management";
import { SettingsPage } from "./Settings";
import { SyncPage } from "./Sync";
import { ConversationTools } from "./ConversationTools";
import { SessionsPage } from "./Sessions";
import { MarkdownContent, MediaContext } from "./MarkdownContent";
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, useContext } from "solid-js";
import { FilesPage, SpacePicker, workspacePages, type WorkspacePage } from "./Workspace";
import { NexusApi } from "./api";
import { knownDevices, rememberDevice } from "./devices";
import type { KnownDevice } from "./devices";
import { markdown } from "./markdown";
import { createNexusStore } from "./state";
import type { Gate, Message, StreamDraft } from "./types";

const configuredUrl = import.meta.env.VITE_NEXUS_URL as string | undefined;
const configuredToken = import.meta.env.VITE_NEXUS_TOKEN as string | undefined;

function savedUrl() {
  return configuredUrl ?? sessionStorage.getItem("nexus.url") ?? window.location.origin;
}

function savedToken() {
  return configuredToken ?? sessionStorage.getItem("nexus.token") ?? "";
}

export function App() {
  const [url, setUrl] = createSignal(savedUrl());
  const [token, setToken] = createSignal(savedToken());
  const [connected, setConnected] = createSignal(false);
  const [connecting, setConnecting] = createSignal(false);
  const [connectError, setConnectError] = createSignal("");
  const [discovered, setDiscovered] = createSignal(false);
  const [rememberAccess, setRememberAccess] = createSignal(false);
  const [known, setKnown] = createSignal<KnownDevice[]>(knownDevices());
  const [composer, setComposer] = createSignal("");
  const [sidebar, setSidebar] = createSignal(false);
  const [showDetails, setShowDetails] = createSignal(false);
  const [sessionFilter, setSessionFilter] = createSignal("");
  const [store, setStore] = createSignal<ReturnType<typeof createNexusStore>>();

  async function connect(candidateUrl = url(), candidateToken = token(), remember = rememberAccess()) {
    const hostUrl = candidateUrl.trim();
    const hostToken = candidateToken.trim();
    if (!hostUrl || !hostToken) return false;
    setConnecting(true);
    setConnectError("");
    try {
      const api = new NexusApi(hostUrl, hostToken);
      const [discovery] = await Promise.all([api.discovery(), api.snapshot()]);
      sessionStorage.setItem("nexus.url", hostUrl);
      sessionStorage.setItem("nexus.token", hostToken);
      if (remember) {
        setKnown(rememberDevice({ hostId: discovery.host_id, name: discovery.product, url: hostUrl, token: hostToken, version: discovery.version, lastSeen: new Date().toISOString() }));
      } else {
        setKnown(rememberDevice({ hostId: discovery.host_id, name: discovery.product, url: hostUrl, version: discovery.version, lastSeen: new Date().toISOString() }));
      }
      const next = createNexusStore(api);
      setStore(next);
      next.start();
      setConnected(true);
      return true;
    } catch (error) {
      setConnectError(error instanceof Error ? error.message : String(error));
      return false;
    } finally {
      setConnecting(false);
    }
  }

  async function reconnectKnownDevices(devices: KnownDevice[]) {
    for (const device of devices) {
      if (!device.token) continue;
      if (await connect(device.url, device.token, true)) return;
    }
  }

  onMount(() => {
    // Public discovery needs no token; the form already points at this origin.
    void new NexusApi(savedUrl(), "").discovery().then((host) => setDiscovered(host.product === "nexus-chat")).catch(() => undefined);
    const currentToken = savedToken();
    if (currentToken) void connect(savedUrl(), currentToken, false);
    else void reconnectKnownDevices(known());
  });
  onCleanup(() => store()?.stop());

  return (
    <Show when={connected() && store()} fallback={
      <ConnectScreen discovered={discovered()} url={url()} token={token()} setUrl={setUrl} setToken={setToken} onConnect={() => void connect()} connecting={connecting()} error={connectError()} remember={rememberAccess()} setRemember={setRememberAccess} devices={known()} onReconnect={(device) => { setUrl(device.url); if (device.token) setToken(device.token); void connect(device.url, device.token ?? "", Boolean(device.token)); }} />
    }>
      {(activeStore) => <ChatShell store={activeStore()} composer={composer()} setComposer={setComposer}
        sidebar={sidebar()} setSidebar={setSidebar} showDetails={showDetails()} setShowDetails={setShowDetails}
        sessionFilter={sessionFilter()} setSessionFilter={setSessionFilter} onDisconnect={() => {
          activeStore().stop(); setConnected(false); setStore(undefined);
        }} />}
    </Show>
  );
}

function ConnectScreen(props: { discovered: boolean; url: string; token: string; setUrl: (value: string) => void; setToken: (value: string) => void; onConnect: () => void; connecting: boolean; error: string; remember: boolean; setRemember: (value: boolean) => void; devices: KnownDevice[]; onReconnect: (device: KnownDevice) => void }) {
  return <main class="connect-screen">
    <section class="connect-card">
      <div class="brand-mark">N</div>
      <p class="eyebrow">LOCAL-FIRST INTELLIGENCE</p>
      <h1>Nexus Chat</h1>
      <p class="muted">Connect to your local host. Your token stays in this browser session unless you choose to remember access.</p>
      <Show when={props.devices.length > 0}><div class="known-devices"><p class="eyebrow">PREVIOUS HOSTS</p><For each={props.devices}>{(device) => <button class="known-device" onClick={() => props.onReconnect(device)}><span class="device-status">{device.token ? "●" : "○"}</span><span><strong>{device.name}</strong><small>{device.url}</small></span><span>↗</span></button>}</For></div></Show>
      <Show when={props.discovered}><p class="muted">Nexus host discovered. Enter your token to connect.</p></Show>
      <label>Host URL<input value={props.url} onInput={(event) => props.setUrl(event.currentTarget.value)} placeholder="http://127.0.0.1:8643" /></label>
      <label>Host token<input type="password" value={props.token} onInput={(event) => props.setToken(event.currentTarget.value)} onKeyDown={(event) => event.key === "Enter" && props.onConnect()} placeholder="Bearer token" /></label>
      <label class="remember-access"><input type="checkbox" checked={props.remember} onChange={(event) => props.setRemember(event.currentTarget.checked)} /> Remember access on this browser</label>
      <Show when={props.error}><p class="connect-error">{props.error}</p></Show>
      <button class="primary-button" onClick={props.onConnect} disabled={props.connecting}>{props.connecting ? "Connecting…" : "Connect"} <span>↗</span></button>
      <p class="hint">Run <code>nexus host</code> to start the local daemon.</p>
    </section>
  </main>;
}

function ChatShell(props: {
  store: ReturnType<typeof createNexusStore>;
  composer: string;
  setComposer: (value: string) => void;
  sidebar: boolean;
  setSidebar: (value: boolean) => void;
  showDetails: boolean;
  setShowDetails: (value: boolean) => void;
  sessionFilter: string;
  setSessionFilter: (value: string) => void;
  onDisconnect: () => void;
}) {
  const [page, changePage] = createSignal<WorkspacePage>("Chat");
  function setPage(next: WorkspacePage) {
    if (next !== page() && !props.store.canNavigate()) return false;
    changePage(next);
    return true;
  }
  const state = props.store.state;
  const [uploading, setUploading] = createSignal(false);
  const [attachmentError, setAttachmentError] = createSignal("");
  async function attachImage(file: File) {
    const space = state.snapshot?.active_space_id;
    const session = state.activeSessionId;
    if (!space || uploading()) return;
    setUploading(true); setAttachmentError("");
    try {
      if (file.size > 10 * 1024 * 1024) throw new Error("Chat images must be 10 MiB or smaller.");
      const result = await props.store.api.request<{ markdown: string }>(`/v1/attachments?space_id=${encodeURIComponent(space)}`, {
        method: "POST", body: file, headers: { "Content-Type": "application/octet-stream" },
      });
      if (space === state.snapshot?.active_space_id && session === state.activeSessionId) {
        props.setComposer(`${props.composer}${props.composer ? "\n" : ""}${result.markdown}`);
      }
    } catch (error) { setAttachmentError(String(error)); }
    finally { setUploading(false); }
  }
  createEffect(() => {
    const update = state.composerUpdate;
    if (update) props.setComposer(update.text);
  });
  createEffect(() => { if (state.loginRequested) setPage("Settings"); });
  const activeSession = createMemo(() => state.snapshot?.sessions.find((session) => session.id === state.activeSessionId));
  const messages = createMemo(() => state.activeSessionId ? state.messages[state.activeSessionId] ?? [] : []);
  const draft = createMemo(() => state.activeSessionId ? state.drafts[state.activeSessionId] : undefined);
  const sessions = createMemo(() => (state.snapshot?.sessions ?? []).filter((session) =>
    `${session.title} ${session.slug ?? ""}`.toLowerCase().includes(props.sessionFilter.toLowerCase()),
  ));
  const running = createMemo(() => state.snapshot?.tasks.length ?? 0);
  let messageScroll!: HTMLDivElement;
  let pinnedToBottom = true;
  const keepPinned = () => {
    if (!messageScroll) return;
    if (pinnedToBottom) messageScroll.scrollTop = messageScroll.scrollHeight;
  };
  createEffect(() => {
    messages();
    draft();
    queueMicrotask(keepPinned);
  });

  async function send() {
    const text = props.composer.trim();
    if (!text) return;
    props.setComposer("");
    if (text.startsWith("/")) {
      await runSlashCommand(text, props.store);
      return;
    }
    if (!await props.store.command({ Send: { text } })) props.setComposer(text);
  }

  return <MediaContext.Provider value={{ api: props.store.api, space: () => state.snapshot?.active_space_id, showReasoning: () => state.snapshot?.settings.show_reasoning ?? false }}><div class="app-shell" classList={{ "hide-hints": state.snapshot?.settings.hide_hints, "hide-stats": state.snapshot?.settings.show_stats === false }}>
    <Show when={props.sidebar}><button class="scrim" aria-label="Close sidebar" onClick={() => props.setSidebar(false)} /></Show>
    <aside classList={{ sidebar: true, open: props.sidebar }}>
      <div class="sidebar-top"><div class="wordmark"><span class="brand-mark small">N</span><strong>NEXUS</strong></div><button class="icon-button mobile-only" onClick={() => props.setSidebar(false)}>×</button></div>
      <SpacePicker store={props.store} />
      <nav class="workspace-nav" aria-label="Workspace"><For each={workspacePages}>{(item) => <button classList={{ selected: page() === item }} aria-current={page() === item ? "page" : undefined} onClick={() => { setPage(item); props.setSidebar(false); if (item === "Usage") void props.store.loadUsage(); }}>{item}</button>}</For></nav>
      <button class="new-chat" onClick={() => { if (!setPage("Chat")) return; void props.store.command("NewSession"); props.setSidebar(false); }}><span>＋</span> New conversation <kbd>⌘ K</kbd></button>
      <label class="session-search"><span>⌕</span><input value={props.sessionFilter} onInput={(event) => props.setSessionFilter(event.currentTarget.value)} placeholder="Search conversations" /></label>
      <div class="session-heading"><span>CONVERSATIONS</span><span>{sessions().length}</span></div>
      <nav class="session-list" aria-label="Conversations">
        <For each={sessions()} fallback={<p class="empty-small">No conversations yet.</p>}>{(session) =>
          <button classList={{ "session-row": true, selected: session.id === state.activeSessionId }} onClick={() => { if (!setPage("Chat")) return; void props.store.selectSession(session.id); props.setSidebar(false); }}>
            <span class="session-dot" /> <span class="session-name">{session.title || "Untitled conversation"}</span><Show when={state.snapshot?.tasks.some((task) => task.session_id === session.id)}><span class="pulse" /></Show>
          </button>
        }</For>
      </nav>
      <div class="sidebar-footer"><Show when={running() > 0}><div class="running-task">◌ {running()} task{running() === 1 ? "" : "s"} running</div></Show><button class="footer-button" onClick={() => props.setShowDetails(!props.showDetails)}>⚙ Workspace</button><button class="footer-button danger" onClick={() => { if (props.store.canNavigate()) props.onDisconnect(); }}>⇥ Disconnect</button></div>
    </aside>
    <main class="conversation">
      <header class="topbar"><button class="icon-button mobile-only" onClick={() => props.setSidebar(true)} aria-label="Open sidebar">☰</button><div class="title-block"><span class="eyebrow">{page() === "Chat" ? "CONVERSATION" : "WORKSPACE"}</span><h2>{page() === "Chat" ? activeSession()?.title || "Nexus Chat" : page()}</h2></div><div class="top-actions"><ConnectionDot status={state.connection} /><Show when={running() > 0}><span class="task-count">{running()} active</span></Show><button class="icon-button" onClick={() => props.setShowDetails(!props.showDetails)} aria-label="Toggle details">⋯</button></div></header>
      <Show when={page() !== "Chat"}><Show when={page() === "Files"}><FilesPage store={props.store} /></Show>
        <Show when={page() === "Research activity"}><ResearchPage store={props.store} sessionId={state.activeSessionId} /></Show>
        <Show when={page() === "Apps"}><AppsPage store={props.store} /></Show>
        <Show when={page() === "Images/scripts"}><MediaPage store={props.store} onAttach={(markdown) => { if (setPage("Chat")) props.setComposer(`${props.composer}${props.composer ? "\n" : ""}${markdown}`); }} /></Show>
        <Show when={page() === "Management"}><ManagementPage store={props.store} /></Show>
        <Show when={page() === "Settings"}><SettingsPage store={props.store} /></Show>
        <Show when={page() === "Sync"}><SyncPage store={props.store} /></Show>
        <Show when={page() === "Conversations"}><SessionsPage store={props.store} /></Show>
        <Show when={page() === "Usage"}><section class="workspace-page"><h1>Usage</h1><Show when={state.error}><p role="alert">{state.error}</p></Show><button onClick={() => void props.store.loadUsage()}>View usage analytics</button></section></Show>
      </Show>
      <Show when={page() === "Chat"}>
      <ConversationTools store={props.store} messages={messages()} sessionId={state.activeSessionId} />
      <div class="message-scroll" ref={messageScroll} onScroll={() => { pinnedToBottom = messageScroll.scrollHeight - messageScroll.scrollTop - messageScroll.clientHeight < 80; }}>
        <Show when={state.error}><div class="error-banner">{state.error}</div></Show>
        <Show when={state.status}><div class="status-banner">{state.status}</div></Show>
        <Show when={messages().length > 0 || draft()} fallback={<EmptyState onSuggestion={(text) => props.setComposer(text)} />}>
          <div class="message-column"><MessageList messages={messages()} /><Show when={draft()}>{(value) => <DraftView draft={value()} showDetails={props.showDetails} />}</Show></div>
        </Show>
      </div>
      <Show when={state.gate && state.gate.session_id === state.activeSessionId ? state.gate : undefined}>{(gate) => <GateBar gate={gate()} onAnswer={async (answer) => { await props.store.command({ AnswerGate: { text: answer } }); props.setComposer(""); }} />}</Show>
      <div onPaste={(event) => { const file = Array.from(event.clipboardData?.files ?? []).find((item) => item.type.startsWith("image/")); if (file) { event.preventDefault(); void attachImage(file); } }}>
        <label class="chat-attachment">{uploading() ? "Uploading image…" : "Attach image"}<input aria-label="Attach image" type="file" accept="image/png,image/jpeg,image/webp,image/gif" disabled={uploading()} onChange={(event) => { const file = event.currentTarget.files?.[0]; if (file) void attachImage(file); event.currentTarget.value = ""; }} /></label>
        <Show when={attachmentError()}><p class="error-banner" role="alert">{attachmentError()}</p></Show>
        <Composer value={props.composer} setValue={props.setComposer} onSend={send} running={Boolean(activeSession() && state.snapshot?.tasks.some((task) => task.session_id === activeSession()?.id))} onStop={() => void props.store.command({ Cancel: { task: null } })} />
      </div>
    </Show>
    </main>
    <Show when={state.usage && page() === "Usage" ? state.usage : undefined}>{(usage) => <UsageModal usage={usage()} loading={state.usageLoading} onClose={() => props.store.closeUsage()} onRange={(range) => void props.store.loadUsage(range)} />}</Show>
    <Show when={props.showDetails}><DetailsPanel store={props.store} activeSession={activeSession()} research={state.research?.sessionId === state.activeSessionId ? state.research.update : null} onClose={() => props.setShowDetails(false)} /></Show>
  </div></MediaContext.Provider>;
}

interface ToolCallData {
  name: string;
  arguments: string;
  result: string;
}

function MessageList(props: { messages: Message[] }) {
  const toolCalls = createMemo(() => props.messages.filter((message) => message.role === "tool_call").map((message) => parseToolCall(message.content)));
  const compact = createMemo(() => toolCalls().length > 10);
  const firstToolIndex = createMemo(() => props.messages.findIndex((message) => message.role === "tool_call"));
  return <For each={props.messages}>{(message, index) => <Show when={message.role === "tool_call" && compact()} fallback={<MessageView message={message} />}><Show when={index() === firstToolIndex()}><ToolCallsSummary calls={toolCalls()} /></Show></Show>}</For>;
}

function ToolCallsSummary(props: { calls: ToolCallData[] }) {
  const [open, setOpen] = createSignal(false);
  return <><button class="tool-summary" onClick={() => setOpen(true)}><span class="tool-summary-icon">⚒</span><span><strong>{props.calls.length} tool calls</strong><small>Collapsed to keep the conversation readable</small></span><span class="tool-summary-open">Inspect ↗</span></button><Show when={open()}><div class="tool-modal-backdrop" role="presentation" onClick={(event) => event.target === event.currentTarget && setOpen(false)}><section class="tool-modal" role="dialog" aria-modal="true" aria-label="Tool calls"><header><div><span class="eyebrow">ACTIVITY</span><h2>{props.calls.length} tool calls</h2></div><button class="icon-button" onClick={() => setOpen(false)} aria-label="Close tool calls">×</button></header><div class="tool-modal-list"><For each={props.calls}>{(call) => <ToolCallView call={call} />}</For></div></section></div></Show></>;
}

function MessageView(props: { message: Message }) {
  const preferences = useContext(MediaContext);
  if (props.message.role === "tool_call") {
    return <ToolCallView call={parseToolCall(props.message.content)} />;
  }
  const assistant = props.message.role !== "user";
  return <article classList={{ message: true, user: !assistant, assistant }}>
    <div class="message-avatar">{assistant ? "✦" : "You"}</div>
    <div class="message-body"><Show when={assistant}><div class="message-meta"><strong>{props.message.persona || "Nexus"}</strong><Show when={props.message.model}><span>{props.message.model}</span></Show></div></Show>
      <Show when={props.message.reasoning}><details class="reasoning" open={preferences?.showReasoning()}><summary>Reasoning</summary><div class="reasoning-copy">{props.message.reasoning}</div></details></Show>
      <MessageContent content={props.message.content} />
      <Show when={props.message.tokens || props.message.cost}><div class="message-stats"><Show when={props.message.tokens}>{props.message.tokens} tokens</Show><Show when={props.message.secs}>{props.message.secs!.toFixed(1)}s</Show><Show when={props.message.cost}>${props.message.cost!.toFixed(4)}</Show></div></Show>
    </div>
  </article>;
}

function MessageContent(props: { content: string }) {
  const sources = createMemo(() => splitSources(props.content));
  return <><Show when={sources().body}><MarkdownContent content={sources().body} /></Show><Show when={sources().items}><section class="sources"><h3>Sources</h3><div class="source-list markdown" innerHTML={markdown(sources().items)} /></section></Show></>;
}

function splitSources(content: string): { body: string; items: string } {
  const match = content.match(/(?:^|\n)(Sources|References):\s*\n([\s\S]*)$/i);
  if (!match || match.index === undefined) return { body: content, items: "" };
  return { body: content.slice(0, match.index).trimEnd(), items: match[2].trim() };
}

function ToolCallView(props: { call: ToolCallData }) {
  return <article class="tool-message"><div class="tool-avatar">⚒</div><div class="tool-content"><details><summary><strong>{toolSummary(props.call)}</strong><span>tool call</span></summary><div class="tool-detail"><label>ARGUMENTS</label><pre>{prettyJson(props.call.arguments)}</pre><label>RESULT</label><pre>{props.call.result || "(no result)"}</pre></div></details></div></article>;
}

function parseToolCall(content: string): ToolCallData {
  try {
    const value = JSON.parse(content) as Partial<ToolCallData>;
    return { name: value.name || "unknown", arguments: value.arguments || "{}", result: value.result || "" };
  } catch {
    return { name: "tool call", arguments: content, result: "" };
  }
}

function prettyJson(value: string): string {
  try { return JSON.stringify(JSON.parse(value), null, 2); } catch { return value; }
}

function toolSummary(call: ToolCallData): string {
  let args: Record<string, unknown> = {};
  try { args = JSON.parse(call.arguments) as Record<string, unknown>; } catch { /* keep raw arguments below */ }
  const text = (key: string) => typeof args[key] === "string" ? args[key] as string : "";
  if (call.name === "batch" && Array.isArray(args.calls)) return `batch · ${args.calls.length} operations`;
  if (call.name === "search") return `search · ${text("query") || "web query"}`;
  if (call.name === "fetch_url") return `fetch_url · ${text("url") || "URL"}`;
  const first = Object.values(args).find((value) => typeof value === "string") as string | undefined;
  return first ? `${call.name} · ${first.slice(0, 90)}` : call.name;
}

function DraftView(props: { draft: StreamDraft; showDetails: boolean }) {
  return <><Show when={props.draft.tools.length > 10} fallback={<For each={props.draft.tools}>{(tool) => <ToolCallView call={tool} />}</For>}><ToolCallsSummary calls={props.draft.tools} /></Show><article class="message assistant draft-message"><div class="message-avatar">✦</div><div class="message-body"><div class="message-meta"><strong>Nexus</strong><span class="live-label">LIVE</span></div><Show when={props.showDetails && props.draft.reasoning}><details class="reasoning" open><summary>Reasoning</summary><div class="reasoning-copy">{props.draft.reasoning}</div></details></Show><Show when={props.draft.status}><div class="stream-status">◌ {props.draft.status}</div></Show><MessageContent content={props.draft.answer} /><Show when={!props.draft.done && !props.draft.error}><span class="cursor" /></Show><Show when={props.draft.error}><div class="stream-error">{props.draft.error}</div></Show></div></article></>;
}

function Composer(props: { value: string; setValue: (value: string) => void; onSend: () => void; running: boolean; onStop: () => void }) {
  return <div class="composer-wrap"><div class="composer"><textarea value={props.value} onInput={(event) => props.setValue(event.currentTarget.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); props.onSend(); } }} placeholder="Ask Nexus anything…" rows="1" /><div class="composer-actions"><span class="composer-hint">Shift + Enter for newline</span><Show when={props.running} fallback={<button class="send-button" onClick={props.onSend} disabled={!props.value.trim()} aria-label="Send">↑</button>}><button class="stop-button" onClick={props.onStop} aria-label="Stop response">■</button></Show></div></div><p class="disclaimer">Nexus can make mistakes. Verify important information.</p></div>;
}

function GateBar(props: { gate: Gate; onAnswer: (answer: string) => Promise<void> }) {
  const [answer, setAnswer] = createSignal("");
  return <div class="gate-bar"><span>◈ Nexus is waiting for your input</span><input value={answer()} onInput={(event) => setAnswer(event.currentTarget.value)} placeholder="Answer the research question…" onKeyDown={(event) => { if (event.key === "Enter" && answer().trim()) void props.onAnswer(answer()); }} /><button onClick={() => { if (answer().trim()) void props.onAnswer(answer()); }}>Reply</button></div>;
}

function EmptyState(props: { onSuggestion: (text: string) => void }) {
  const suggestions = ["Research a complex topic", "Help me think through a decision", "Summarize my latest work"];
  return <div class="empty-state"><div class="empty-icon">✦</div><h1>Good to see you.</h1><p>Start a conversation with your local research partner.</p><div class="suggestions"><For each={suggestions}>{(suggestion) => <button onClick={() => props.onSuggestion(suggestion)}>{suggestion}<span>↗</span></button>}</For></div></div>;
}

function DetailsPanel(props: { store: ReturnType<typeof createNexusStore>; activeSession: { model: string; web_mode: boolean } | undefined; research: unknown; onClose: () => void }) {
  const state = props.store.state;
  return <aside class="details-panel"><div class="details-heading"><span>WORKSPACE</span><button class="icon-button" onClick={props.onClose} aria-label="Close workspace">×</button></div><label>MODEL<select value={state.snapshot?.settings.model ?? ""} onChange={(event) => void props.store.command({ SetModel: { id: event.currentTarget.value } })}><For each={state.snapshot?.models ?? []}>{(model) => <option value={model.id}>{model.name}</option>}</For></select></label><div class="toggle-row"><span>Web mode</span><button classList={{ toggle: true, on: state.snapshot?.settings.web_mode }} onClick={() => void props.store.command("ToggleWeb")}><span /></button></div><div class="toggle-row"><span>Incognito</span><button classList={{ toggle: true, on: state.snapshot?.settings.incognito }} onClick={() => void props.store.command({ Incognito: { on: !state.snapshot?.settings.incognito } })}><span /></button></div><Show when={props.research}><div class="research-card"><div class="eyebrow">RESEARCH</div><p>{researchLabel(props.research)}</p></div></Show><div class="details-divider" /><button class="utility-button" onClick={() => void props.store.command("Compact")}>⌁ Compact context</button><button class="utility-button" onClick={() => void props.store.loadUsage()}>◒ Usage analytics</button><Show when={state.usage}>{(usage) => <UsageModal usage={usage()} loading={state.usageLoading} onClose={() => props.store.closeUsage()} onRange={(range) => void props.store.loadUsage(range)} />}</Show></aside>;
}

function UsageModal(props: { usage: import("./types").UsageSnapshot; loading: boolean; onClose: () => void; onRange: (range: string) => void }) {
  const totals = props.usage.totals;
  return <div class="usage-modal-backdrop" role="presentation" onClick={(event) => event.target === event.currentTarget && props.onClose()}><section class="usage-modal" role="dialog" aria-modal="true" aria-label="Usage analytics"><header><div><span class="eyebrow">USAGE ANALYTICS</span><h2>{props.usage.range === "all" ? "All time" : `Last ${props.usage.range}`}</h2></div><button class="icon-button" onClick={props.onClose} aria-label="Close usage">×</button></header><nav class="usage-ranges" aria-label="Usage range"><For each={["day", "week", "month", "all"]}>{(range) => <button classList={{ selected: props.usage.range === range }} onClick={() => props.onRange(range)}>{range === "day" ? "24h" : range === "week" ? "7d" : range === "month" ? "30d" : "All"}</button>}</For></nav><div class="usage-content"><Show when={!props.loading} fallback={<p class="muted">Refreshing usage…</p>}><div class="usage-metrics"><div><strong>{totals.requests}</strong><span>requests</span></div><div><strong>{formatCount(totals.prompt_tokens + totals.completion_tokens)}</strong><span>tokens</span></div><div><strong>${totals.cost.toFixed(4)}</strong><span>cost</span></div><div><strong>{cacheRate(totals)}%</strong><span>cache hit rate</span></div></div><h3>By backend</h3><For each={props.usage.by_backend}>{(row) => <div class="usage-row"><span>{row.backend}</span><span>{row.requests ?? 0} requests · {formatCount(row.prompt_tokens + row.completion_tokens)} tokens · ${(row.cost ?? 0).toFixed(4)}</span></div>}</For><Show when={props.usage.by_backend.length === 0}><p class="muted">No usage recorded for this range.</p></Show></Show></div></section></div>;
}

function formatCount(value: number) { return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value); }
function cacheRate(totals: { rated_prompt_tokens: number; rated_cache_read_tokens: number }) { return totals.rated_prompt_tokens ? Math.round((totals.rated_cache_read_tokens / totals.rated_prompt_tokens) * 100) : 0; }
function ConnectionDot(props: { status: string }) { return <span class="connection"><i classList={{ connected: props.status === "connected" }} />{props.status}</span>; }
function researchLabel(value: unknown) { const entry = value && typeof value === "object" ? Object.entries(value)[0] : undefined; return entry ? `${entry[0]} — ${JSON.stringify(entry[1])}` : "Research in progress"; }

async function runSlashCommand(text: string, store: ReturnType<typeof createNexusStore>) {
  const [command, ...rest] = text.slice(1).trim().split(/\s+/);
  const value = rest.join(" ");
  const commands: Record<string, unknown> = { new: "NewSession", compact: "Compact", web: "ToggleWeb", cancel: { Cancel: { task: null } }, usage: "OpenUsage" };
  if (command === "research") commands.research = { RunResearch: { topic: value, gated: true } };
  if (command === "model" && value) commands.model = { SetModel: { id: value } };
  if (command === "incognito") commands.incognito = { Incognito: { on: value !== "off" } };
  if (commands[command]) await store.command(commands[command]);
  else await store.command({ Send: { text } });
}
