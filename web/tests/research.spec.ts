import { expect, test } from "@playwright/test";
import { mergeResearch } from "../src/Research";

test("research projection keeps stage updates and parked gates on reconnect", () => {
  const snapshot = {
    session_id: "s1", topic: "climate", stages: [{ label: "survey", detail: "done" }],
    gate: { session_id: "s1", phase: { Clarify: { round: 2 } }, questions: ["Which region?"] },
    steers: [], report: null, citations: [], running: true,
  };
  const view = mergeResearch(snapshot, undefined, "s1");
  expect(view.questions).toEqual(["Which region?"]);
  const next = mergeResearch(view, { Stage: { label: "search", detail: "3 sources" } }, "s1");
  expect(next.stages.map((stage) => stage.label)).toEqual(["survey", "search"]);
  expect(next.stages[1].detail).toBe("3 sources");
});

test("research activity starts a project and exposes the steering controls", async ({ page }) => {
  let started = false;
  let running = false;
  const session = { id: "research-1", title: "Research", model: "test", kind: "research", web_mode: false, created_at: "now" };
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "research", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/v1/snapshot") return route.fulfill({ json: { active_session_id: started ? session.id : null, active_space_id: "space", active_space_name: "Default", sessions: started ? [session] : [], models: [], settings: {}, tasks: running ? [{ id: 1, session_id: session.id, session_title: session.title, status: "streaming", buffer_chars: 0, buffer: "" }] : [], research: {} } });
    if (url.pathname === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\n\n" });
    if (url.pathname === "/v1/research/start") { started = true; running = true; return route.fulfill({ json: { ok: true, session_id: session.id } }); }
    if (url.pathname === `/v1/research/${session.id}`) return route.fulfill({ json: { session_id: session.id, topic: "browser reconnects", stages: [], gate: null, steers: [], report: null, citations: [], running: true } });
    if (url.pathname === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "space", name: "Default" }] } });
    if (url.pathname === "/v1/command") return route.fulfill({ json: { ok: true } });
    return route.fulfill({ status: 404, json: { error: { message: "not mocked" } } });
  });
  await page.goto("/");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  const nav = page.getByRole("navigation", { name: "Workspace", exact: true });
  if (await page.getByRole("button", { name: "Open sidebar" }).isVisible()) await page.getByRole("button", { name: "Open sidebar" }).click();
  await nav.getByRole("button", { name: "Research activity", exact: true }).click();
  await page.getByLabel("Start research").fill("browser reconnects");
  await page.getByRole("button", { name: "Start", exact: true }).click();
  expect(started).toBe(true);
  await expect(page.getByLabel("Steer this research")).toBeVisible();
});
