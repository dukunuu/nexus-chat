export interface Session {
  id: string;
  title: string;
  slug: string | null;
  model: string;
  kind: string;
  web_mode: boolean;
  created_at: string;
}

export interface Model {
  id: string;
  name: string;
  context_length: number | null;
  favorite: boolean;
}

export interface Settings {
  show_stats?: boolean;
  show_reasoning?: boolean;
  hide_hints?: boolean;
  model: string | null;
  verbosity: string;
  web_mode: boolean;
  incognito: boolean;
  search_provider: string;
  langsearch_configured: boolean;
  searxng_url: string;
  temperature: number | null;
  top_p: number | null;
  max_tokens: number | null;
  compact_threshold: number;
  memory_model: string;
  transcriber_model: string;
  ocr_model: string;
  ocr_engine: string;
  embedding_model: string;
  image_gen_model: string;
  video_gen_model: string;
  blocked_domains: string[];
}

export interface Task {
  id: number;
  session_id: string;
  session_title: string;
  model: string;
  backend: string;
  status: "tool" | "streaming" | string;
  buffer_chars: number;
  /** Partial answer retained by the host for SSE reconnect recovery. */
  buffer?: string;
}

export interface Snapshot {
  active_session_id: string | null;
  active_space_id: string;
  active_space_name: string;
  sessions: Session[];
  models: Model[];
  settings: Settings;
  tasks: Task[];
  /** Reconnectable research projection; older hosts omit this field. */
  research?: Record<string, ResearchSnapshot>;
}

export interface ResearchSnapshot {
  session_id: string;
  topic: string;
  stages: ResearchStage[];
  gate: Gate | null;
  steers: string[];
  report: string | null;
  citations: Citation[];
  running: boolean;
}

export interface ResearchStage {
  label: string;
  detail: string;
  updated_at?: string;
}

export interface Citation {
  report_file?: string;
  url: string;
  title?: string;
}

export interface ContextSnapshot {
  system_tokens: number;
  memory_tokens: number;
  skills_tokens: number;
  conversation_tokens: number;
  limit: number | null;
  compacted: boolean;
}

export interface UsageTotals {
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  rated_prompt_tokens: number;
  rated_cache_read_tokens: number;
  cost: number;
}

export interface UsageEntry {
  backend?: string;
  model?: string;
  created_at?: string;
  requests?: number;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens?: number;
  rated_prompt_tokens?: number;
  rated_cache_read_tokens?: number;
  prompt_convention?: string;
  cost: number | null;
}

export interface UsageSnapshot {
  range: string;
  totals: UsageTotals;
  by_backend: UsageEntry[];
  by_model: UsageEntry[];
  recent: UsageEntry[];
}

export interface Message {
  role: string;
  content: string;
  model: string | null;
  reasoning: string | null;
  tokens: number | null;
  secs: number | null;
  cost: number | null;
  phrase: string | null;
  persona: string | null;
  created_at: string | null;
}

export type StreamEvent =
  | { Token: string }
  | { Reasoning: string }
  | { Usage: Usage }
  | { Status: string }
  | { ToolCall: { name: string; arguments: string; result: string } }
  | "Done"
  | { Error: string };

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  cost: number | null;
}

export type WireEvent =
  | { type: "status"; payload: string }
  | { type: "composer_set"; payload: string }
  | { type: "composer_clear" }
  | { type: "viewport_reset" }
  | { type: "history_invalidated" }
  | { type: "open_login_popup" }
  | { type: "gate"; payload: Gate | null }
  | { type: "stream"; payload: [number, StreamEvent] | null }
  | { type: "title"; payload: [string, string, string] | null }
  | { type: "compact"; payload: [string, string, number, number] | null }
  | { type: "research"; payload: [string, string, string, ResearchEvent] | null }
  | { type: "models"; payload: unknown };

export interface Gate {
  session_id: string;
  phase: { Clarify: { round: number } } | { Approve: { rework: boolean } };
  questions?: string[];
}

export type ResearchEvent =
  | { Stage: { label: string; detail: string } }
  | { SurveyReady: { questions: string[]; round: number } }
  | { PlanReady: { questions: PlanQuestion[]; rework: boolean } }
  | { Done: string | { [key: string]: unknown } };

export interface PlanQuestion {
  question: string;
  why: string;
  angles: string[];
  sources: string[];
}

export interface StreamDraft {
  taskId: number;
  sessionId: string;
  answer: string;
  reasoning: string;
  status: string;
  tools: { name: string; arguments: string; result: string }[];
  usage: Usage | null;
  done: boolean;
  error: string | null;
}
