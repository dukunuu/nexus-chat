import { expect, test } from "@playwright/test";
import { NexusApi } from "../src/api";

test("SSE decodes split UTF-8, CRLF, heartbeats and multiple data lines", async () => {
  const originalFetch = globalThis.fetch;
  const bytes = new TextEncoder().encode(': heartbeat\r\n\r\ndata: {"type":"status",\r\ndata: "payload":"你好"}\r\n\r\ndata: {"type":"stream","payload":[1,{"Token":"answer"}]}\n\n');
  globalThis.fetch = async (_input, init) => {
    expect(new Headers(init?.headers).get("Authorization")).toBe("Bearer test-token");
    return new Response(new ReadableStream({ start(controller) { for (const byte of bytes) controller.enqueue(new Uint8Array([byte])); controller.close(); } }), { headers: { "Content-Type": "text/event-stream" } });
  };
  try {
    const events: unknown[] = [];
    await new NexusApi("http://host.invalid", "test-token").events(new AbortController().signal, (event) => events.push(event));
    expect(events).toEqual([{ type: "status", payload: "你好" }, { type: "stream", payload: [1, { Token: "answer" }] }]);
  } finally { globalThis.fetch = originalFetch; }
});
