import { createStore } from "solid-js/store";
import { NexusApi } from "./api";
import type {
  Gate,
  Message,
  ResearchEvent,
  Snapshot,
  StreamDraft,
  StreamEvent,
  Usage,
  UsageSnapshot,
  WireEvent,
} from "./types";

interface State {
  snapshot: Snapshot | null;
  messages: Record<string, Message[]>;
  loadingMessages: Record<string, boolean>;
  activeSessionId: string | null;
  drafts: Record<string, StreamDraft | undefined>;
  gate: Gate | null;
  research: { sessionId: string; update: ResearchEvent } | null;
  researchBySession: Record<string, { update: ResearchEvent; receivedAt: number }>;
  usage: UsageSnapshot | null;
  usageLoading: boolean;
  status: string;
  connection: "connecting" | "connected" | "offline";
  error: string | null;
  composerUpdate: { text: string; revision: number } | null;
  loginRequested: number;
}

const initialState: State = {
  snapshot: null,
  messages: {},
  loadingMessages: {},
  activeSessionId: null,
  drafts: {},
  gate: null,
  research: null,
  researchBySession: {},
  usage: null,
  usageLoading: false,
  status: "",
  connection: "connecting",
  error: null,
  composerUpdate: null,
  loginRequested: 0,
};

const wait = (milliseconds: number) =>
  new Promise<void>((resolve) => window.setTimeout(resolve, milliseconds));

function variant(value: unknown): [string, unknown] | null {
  if (typeof value === "string") return [value, null];
  if (!value || typeof value !== "object") return null;
  const entry = Object.entries(value)[0];
  return entry ?? null;
}

function isUsage(value: unknown): value is Usage {
  return Boolean(value && typeof value === "object" && "total_tokens" in value);
}

export function createNexusStore(api: NexusApi) {
  const [state, setState] = createStore<State>(initialState);
  let stopped = false;
  let navigationGuard: (() => boolean) | undefined;
  let selectionInitialized = false;
  let eventsAbort: AbortController | undefined;
  let refreshVersion = 0;
  let streamVersion = 0;
  const messageVersions = new Map<string, number>();
  const messageErrors = new Map<string, string>();

  async function refresh() {
    const version = ++refreshVersion;
    const streams = streamVersion;
    try {
      const snapshot = await api.snapshot();
      if (version !== refreshVersion || stopped) return;
      if (state.snapshot && snapshot.active_space_id !== state.snapshot.active_space_id) {
        setState("activeSessionId", null);
        setState("gate", null);
        setState("research", null);
        setState("researchBySession", {});
        selectionInitialized = false;
      }
      setState("snapshot", snapshot);
      for (const task of snapshot.tasks) {
        if (task.buffer === undefined || !task.session_id || streams !== streamVersion) continue;
        const existing = state.drafts[task.session_id];
        if (existing?.taskId === task.id && existing.answer === task.buffer) continue;
        setState("drafts", task.session_id, existing?.taskId === task.id ? { ...existing, answer: task.buffer, status: task.status } : {
          taskId: task.id,
          sessionId: task.session_id,
          answer: task.buffer,
          reasoning: "",
          status: task.status,
          tools: [],
          usage: null,
          done: false,
          error: null,
        });
      }
      for (const [sessionId, draft] of Object.entries(state.drafts)) {
        if (draft && !snapshot.tasks.some((task) => task.id === draft.taskId)) {
          void loadMessages(sessionId).then((loaded) => { if (loaded && state.drafts[sessionId]?.taskId === draft.taskId) setState("drafts", sessionId, undefined); });
        }
      }
      const active = snapshot.active_session_id ?? (selectionInitialized ? state.activeSessionId : snapshot.sessions[0]?.id ?? null);
      const next = active && snapshot.sessions.some((session) => session.id === active) ? active : null;
      setState("activeSessionId", next);
      selectionInitialized = true;
      // A fresh host has no selected session. Select the first existing one
      // so subsequent Send commands mutate the conversation the user sees.
      if (!snapshot.active_session_id && next) void api.command({ ResolveSession: { id: next } });
      setState("error", messageErrors.get(next ?? "") ?? null);
      if (next && !state.messages[next]) void loadMessages(next);
    } catch (error) {
      setState("error", error instanceof Error ? error.message : String(error));
    }
  }

  async function loadMessages(sessionId: string) {
    const version = (messageVersions.get(sessionId) ?? 0) + 1;
    messageVersions.set(sessionId, version);
    setState("loadingMessages", sessionId, true);
    try {
      const messages = await api.messages(sessionId);
      if (stopped || messageVersions.get(sessionId) !== version) return false;
      setState("messages", sessionId, messages);
      messageErrors.delete(sessionId);
      setState("error", null);
      return true;
    } catch (error) {
      if (messageVersions.get(sessionId) === version) {
        const detail = error instanceof Error ? error.message : String(error);
        messageErrors.set(sessionId, detail);
        setState("error", detail);
      }
      return false;
    } finally {
      if (messageVersions.get(sessionId) === version) setState("loadingMessages", sessionId, false);
    }
  }

  function streamDraft(taskId: number, event: StreamEvent) {
    streamVersion++;
    const knownTask = state.snapshot?.tasks.find((task) => task.id === taskId);
    const sessionId = knownTask?.session_id ?? Object.values(state.drafts).find((draft) => draft?.taskId === taskId)?.sessionId;
    if (!sessionId) {
      void refresh();
      if (state.activeSessionId) void loadMessages(state.activeSessionId);
      return;
    }
    const existing = state.drafts[sessionId];
    const current = (existing?.taskId === taskId ? existing : undefined) ?? {
      taskId,
      sessionId,
      answer: "",
      reasoning: "",
      status: "",
      tools: [],
      usage: null,
      done: false,
      error: null,
    };
    const [kind, value] = variant(event) ?? [];
    const patch: Partial<StreamDraft> = { taskId, sessionId };
    if (kind === "Token") patch.answer = current.answer + String(value ?? "");
    if (kind === "Reasoning") patch.reasoning = current.reasoning + String(value ?? "");
    if (kind === "Status") patch.status = String(value ?? "");
    if (kind === "Usage" && isUsage(value)) patch.usage = value;
    if (kind === "ToolCall" && value && typeof value === "object") {
      patch.tools = [...current.tools, value as StreamDraft["tools"][number]];
    }
    if (kind === "Done") patch.done = true;
    if (kind === "Error") {
      patch.error = String(value ?? "stream failed");
      patch.done = true;
    }
    setState("drafts", sessionId, { ...current, ...patch });
    if (kind === "Done" || kind === "Error") {
      void loadMessages(sessionId).then((loaded) => { if (loaded && state.drafts[sessionId]?.taskId === taskId) setState("drafts", sessionId, undefined); });
      void refresh();
    }
  }

  function handleEvent(event: WireEvent) {
    switch (event.type) {
      case "status":
        setState("status", event.payload);
        break;
      case "composer_set":
        setState("composerUpdate", { text: event.payload, revision: (state.composerUpdate?.revision ?? 0) + 1 });
        setState("status", "draft restored");
        break;
      case "composer_clear":
        setState("composerUpdate", { text: "", revision: (state.composerUpdate?.revision ?? 0) + 1 });
        break;
      case "open_login_popup":
        setState("loginRequested", (value) => value + 1);
        break;
      case "gate":
        setState("gate", event.payload);
        break;
      case "stream":
        if (event.payload) streamDraft(event.payload[0], event.payload[1]);
        break;
      case "title":
      case "models":
      case "compact":
      case "history_invalidated":
        void refresh();
        if (event.type === "history_invalidated" && state.activeSessionId) {
          void loadMessages(state.activeSessionId);
        }
        break;
      case "research":
        if (event.payload) {
          const sessionId = event.payload[0];
          const update = event.payload[3];
          setState("research", { sessionId, update });
          setState("researchBySession", sessionId, { update, receivedAt: Date.now() });
        }
        break;
      default:
        // New server events must not break older clients.
        break;
    }
  }

  async function loadUsage(range = "all") {
    setState("usageLoading", true);
    try {
      setState("usage", await api.usage(range));
      setState("error", null);
    } catch (error) {
      setState("error", error instanceof Error ? error.message : String(error));
    } finally {
      setState("usageLoading", false);
    }
  }

  async function runEvents() {
    let delay = 500;
    while (!stopped) {
      eventsAbort = new AbortController();
      setState("connection", "connecting");
      try {
        await api.events(eventsAbort.signal, handleEvent, () => {
          setState("connection", "connected");
          // Reconcile missed stage/gate/task state immediately after every
          // reconnect; SSE is an update stream rather than durable storage.
          void refresh();
        });
        delay = 500;
      } catch (error) {
        if (stopped) break;
        setState("connection", "offline");
        setState("error", error instanceof Error ? error.message : String(error));
        await wait(delay);
        delay = Math.min(delay * 2, 10_000);
        continue;
      }
      if (!stopped) {
        setState("connection", "offline");
        await wait(delay);
      }
    }
  }

  return {
    canNavigate() { return navigationGuard?.() ?? true; },
    guardNavigation(guard: () => boolean) {
      navigationGuard = guard;
      return () => { if (navigationGuard === guard) navigationGuard = undefined; };
    },
    api,
    state,
    refresh,
    loadMessages,
    loadUsage,
    closeUsage() {
      setState("usage", null);
    },
    start() {
      stopped = false;
      void refresh();
      void runEvents();
    },
    stop() {
      stopped = true;
      eventsAbort?.abort();
    },
    async command(command: unknown) {
      if (command && typeof command === "object" && "SwitchSpace" in command && !(navigationGuard?.() ?? true)) return false;
      try {
        if (command === "NewSession") {
          setState("activeSessionId", null);
          selectionInitialized = true;
        }
        await api.command(command);
        setState("error", null);
        void refresh();
        return true;
      } catch (error) {
        setState("error", error instanceof Error ? error.message : String(error));
        return false;
      }
    },
    async selectSession(sessionId: string) {
      setState("activeSessionId", sessionId);
      selectionInitialized = true;
      if (!state.messages[sessionId]) void loadMessages(sessionId);
      try {
        await api.command({ ResolveSession: { id: sessionId } });
        void refresh();
      } catch (error) {
        setState("error", error instanceof Error ? error.message : String(error));
      }
    },
  };
}

export type NexusStore = ReturnType<typeof createNexusStore>;
