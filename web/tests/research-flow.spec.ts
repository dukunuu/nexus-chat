import { expect, test, type Page } from "@playwright/test";

type RequestLog = { method: string; path: string; body: Record<string, unknown> };

async function connectResearch(page: Page, options: { selected: boolean; running: boolean } = { selected: true, running: true }) {
  const requests: RequestLog[] = [];
  let selected = options.selected;
  let running = options.running;
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "research", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const body = request.postDataJSON?.() ?? {};
    requests.push({ method: request.method(), path: url.pathname, body });
    if (url.pathname === "/v1/snapshot") {
      const session = { id: "research-1", title: "Research", model: "test", kind: "research", web_mode: false, created_at: "now" };
      return route.fulfill({ json: { active_session_id: selected ? session.id : null, active_space_id: "space", active_space_name: "Default", sessions: selected ? [session] : [], models: [], settings: {}, tasks: running && selected ? [{ id: 7, session_id: session.id, session_title: session.title, status: "streaming", buffer_chars: 4, buffer: "work" }] : [], research: {} } });
    }
    if (url.pathname === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\n\n" });
    if (url.pathname === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "space", name: "Default" }] } });
    if (url.pathname === "/v1/command") return route.fulfill({ json: { ok: true } });
    if (url.pathname.endsWith("/citations")) return route.fulfill({ json: { citations: [{ url: "https://example.com/source", title: "Source", report_file: "report.md" }] } });
    if (url.pathname.endsWith("/context")) return route.fulfill({ json: { system_tokens: 10, memory_tokens: 2, skills_tokens: 3, conversation_tokens: 20, limit: 100, compacted: false } });
    if (url.pathname === "/v1/research/start") { selected = true; running = true; return route.fulfill({ json: { ok: true, session_id: "research-1" } }); }
    if (url.pathname === "/v1/research/research-1/stop") { running = false; return route.fulfill({ json: { ok: true } }); }
    if (/\/v1\/research\/research-1$/.test(url.pathname)) return route.fulfill({ json: { session_id: "research-1", topic: "Test research", stages: [{ label: "survey", detail: "waiting" }], gate: running ? { session_id: "research-1", phase: { Clarify: { round: 1 } }, questions: ["Which scope?"] } : null, steers: [], report: "# Final report\n\nEvidence [1]", citations: [], running } });
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto("/");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  return requests;
}

async function openWorkspace(page: Page, name: string) {
  const sidebar = page.getByRole("button", { name: "Open sidebar" });
  if (await sidebar.isVisible()) await sidebar.click();
  await page.getByRole("navigation", { name: "Workspace", exact: true }).getByRole("button", { name, exact: true }).click();
}

test("research flow targets the selected session and exposes gate, steer, context and export", async ({ page }) => {
  const requests = await connectResearch(page);
  await openWorkspace(page, "Research activity");
  await expect(page.getByText("Which scope?", { exact: true })).toBeVisible();
  await page.getByLabel("Steer this research").fill("Check primary sources");
  await page.getByRole("button", { name: /Queue steer/i }).click();
  await page.getByLabel("Clarify response").fill("Global scope");
  await page.getByRole("button", { name: "Send response", exact: true }).click();
  await page.getByRole("button", { name: /Stop research|Stop/i }).click();
  const gate = requests.find((request) => request.path === "/v1/research/research-1/answer");
  const steer = requests.find((request) => request.path === "/v1/research/research-1/steer");
  const stop = requests.find((request) => request.path === "/v1/research/research-1/stop");
  expect(gate?.method).toBe("POST");
  expect(gate?.body).toEqual({ text: "Global scope" });
  expect(steer?.method).toBe("POST");
  expect(steer?.body).toEqual({ text: "Check primary sources" });
  expect(stop?.method).toBe("POST");
  const exportButton = page.getByRole("button", { name: /Export markdown|Export report/i });
  if (await exportButton.count()) {
    const download = page.waitForEvent("download");
    await exportButton.click();
    await expect((await download).suggestedFilename()).toMatch(/\.md$/);
  }
  await openWorkspace(page, "Chat");
  await page.getByRole("button", { name: "Tools", exact: true }).click();
  await expect(page.getByText(/20 conversation tokens/)).toBeVisible();
});

test("research starts without a selected session and can stop a running job", async ({ page }) => {
  const requests = await connectResearch(page, { selected: false, running: false });
  await openWorkspace(page, "Research activity");
  await page.getByLabel("Start research").fill("Fresh topic");
  await page.getByRole("button", { name: "Start", exact: true }).click();
  const start = requests.find((request) => request.path === "/v1/research/start");
  expect(start?.method).toBe("POST");
  expect(start?.body).toMatchObject({ topic: "Fresh topic", gated: true, space_id: "space" });
});
