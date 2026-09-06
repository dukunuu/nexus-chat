import type { ContextSnapshot, Message, Snapshot, UsageSnapshot, WireEvent } from "./types";

export interface Discovery {
  host_id: string;
  product: string;
  version: string;
  api: string;
  authentication: string;
}

export class NexusApi {
  readonly baseUrl: string;
  private readonly token: string;

  constructor(baseUrl: string, token: string) {
    this.baseUrl = baseUrl.replace(/\/$/, "");
    this.token = token;
  }

  private headers(): HeadersInit {
    return {
      Authorization: `Bearer ${this.token}`,
      "Content-Type": "application/json",
    };
  }

  async request<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      ...init,
      headers: { ...this.headers(), ...init.headers },
    });
    if (!response.ok) {
      let detail = `${response.status} ${response.statusText}`;
      try {
        const body = (await response.json()) as { error?: { message?: string } };
        detail = body.error?.message ?? detail;
      } catch {
        // Preserve the HTTP error when the body is not JSON.
      }
      throw new Error(detail);
    }
    return (await response.json()) as T;
  }

  async blob(spaceId: string, name: string): Promise<Blob> {
    const response = await fetch(`${this.baseUrl}/v1/sync/blob?space_id=${encodeURIComponent(spaceId)}&name=${encodeURIComponent(name)}`, { headers: { Authorization: `Bearer ${this.token}` } });
    if (!response.ok) throw new Error(`Download failed: ${response.status}`);
    return response.blob();
  }

  async binary(path: string, init: RequestInit = {}): Promise<Blob> {
    const response = await fetch(`${this.baseUrl}${path}`, { ...init, headers: { Authorization: `Bearer ${this.token}`, ...init.headers } });
    if (!response.ok) {
      const value = await response.json().catch(() => null);
      throw new Error(value?.error?.message ?? `Transfer failed: ${response.status}`);
    }
    return response.blob();
  }

  discovery(): Promise<Discovery> {
    return this.request<Discovery>("/.well-known/nexus");
  }

  snapshot(): Promise<Snapshot> {
    return this.request<Snapshot>("/v1/snapshot");
  }

  usage(range = "all"): Promise<UsageSnapshot> {
    return this.request<UsageSnapshot>(`/v1/usage?range=${encodeURIComponent(range)}`);
  }

  context(sessionId: string): Promise<ContextSnapshot> {
    return this.request<ContextSnapshot>(`/v1/research/${encodeURIComponent(sessionId)}/context`);
  }

  research(sessionId: string): Promise<import("./types").ResearchSnapshot> {
    return this.request<import("./types").ResearchSnapshot>(`/v1/research/${encodeURIComponent(sessionId)}`);
  }

  async messages(sessionId: string): Promise<Message[]> {
    const result = await this.request<{ messages: Message[] }>(
      `/v1/sessions/${encodeURIComponent(sessionId)}/messages`,
    );
    return result.messages;
  }

  command(command: unknown): Promise<{ ok: boolean }> {
    return this.request<{ ok: boolean }>("/v1/command", {
      method: "POST",
      body: JSON.stringify(command),
    });
  }

  async runTool(spaceId: string, name: string, args: unknown): Promise<string> {
    const response = await this.request<{ result?: { result: string }; error?: { message: string } }>(`/v1/tools/run?space_id=${encodeURIComponent(spaceId)}`, {
      method: "POST", body: JSON.stringify({ jsonrpc: "2.0", id: crypto.randomUUID(), method: "tools/run", params: { name, arguments: args } }),
    });
    if (response.error) throw new Error(response.error.message);
    if (!response.result) throw new Error("Tool returned no result");
    return response.result.result;
  }

  async events(
    signal: AbortSignal,
    onEvent: (event: WireEvent) => void,
    onOpen?: () => void,
  ): Promise<void> {
    const response = await fetch(`${this.baseUrl}/v1/events`, {
      headers: { Authorization: `Bearer ${this.token}`, Accept: "text/event-stream" },
      signal,
    });
    if (!response.ok) throw new Error(`event stream: ${response.status}`);
    if (!response.body) throw new Error("event stream has no body");
    onOpen?.();

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const frames = buffer.split(/\r?\n\r?\n/);
        buffer = frames.pop() ?? "";
        for (const frame of frames) {
          const data = frame
            .split(/\r?\n/)
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).trimStart())
            .join("\n");
          if (data) onEvent(JSON.parse(data) as WireEvent);
        }
      }
    } finally {
      reader.releaseLock();
    }
  }
}
