import { expect, test } from "@playwright/test";

test("management controls persist scoped swarm, watches, and skills", async ({ page, isMobile }) => {
  let space = "default";
  let roster = [{ name: "Ada", model: "test", blurb: "careful" }];
  let watches: { id: string; topic: string; interval_hours: number }[] = [];
  let armed = "";
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "management", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.pathname === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\r\n\r\n" });
    if (url.pathname === "/v1/snapshot") return route.fulfill({ json: { active_session_id: "chat", active_space_id: space, active_space_name: space, sessions: [{ id: "chat", title: "Test chat", model: "test", kind: "chat" }], models: [], settings: {}, tasks: [] } });
    if (url.pathname === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "default", name: "default" }, { id: "other", name: "other" }] } });
    if (url.pathname === "/v1/command") { const body = request.postDataJSON(); if (body.SwitchSpace) space = body.SwitchSpace.name; return route.fulfill({ json: { ok: true } }); }
    if (url.pathname === "/v1/swarm" && request.method() === "GET") return route.fulfill({ json: { session_id: "chat", enabled: false, running: false, personas: roster } });
    if (url.pathname === "/v1/swarm/roster") { roster = request.postDataJSON().personas; return route.fulfill({ json: { personas: roster, enabled: false, running: false } }); }
    if (url.pathname === "/v1/swarm/mode") return route.fulfill({ json: { enabled: request.postDataJSON().enabled, running: false, personas: roster } });
    if (url.pathname === "/v1/swarm/start") return route.fulfill({ json: { enabled: false, running: false, personas: roster } });
    if (url.pathname === "/v1/swarm/stop") return route.fulfill({ json: { enabled: false, running: false, personas: roster } });
    if (url.pathname === "/v1/skills") return route.fulfill({ json: { skills: [{ name: "demo", description: "Demo skill", managed: true }] } });
    if (url.pathname === "/v1/skills/arm") { armed = request.postDataJSON().name; return route.fulfill({ json: { ok: true } }); }
    if (url.pathname === "/v1/skills/install") return route.fulfill({ json: { ok: true } });
    if (url.pathname === "/v1/watches" && request.method() === "GET") return route.fulfill({ json: { watches } });
    if (url.pathname === "/v1/watches" && request.method() === "POST") { const body = request.postDataJSON(); watches = [{ id: "w1", topic: body.topic, interval_hours: body.interval_hours }]; return route.fulfill({ json: watches[0] }); }
    if (url.pathname === "/v1/watches/w1" && request.method() === "DELETE") { watches = []; return route.fulfill({ json: { deleted: true } }); }
    if (url.pathname === "/v1/watches/w1/run") return route.fulfill({ json: { started: true } });
    return route.fulfill({ status: 404, json: { error: { message: "unmocked" } } });
  });
  await page.goto("/");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByRole("navigation", { name: "Workspace" }).getByRole("button", { name: "Management" }).click();
  await expect(page.getByRole("heading", { name: "Swarm", exact: true })).toBeVisible();
  await page.getByLabel("Persona name").fill("Grace");
  await page.getByRole("button", { name: "Save roster" }).click();
  expect(roster[0].name).toBe("Grace");
  await page.getByRole("button", { name: "Start" }).click();
  await expect(page.getByRole("alert")).toContainText("could not start");
  await page.getByRole("tab", { name: "Skills" }).click();
  await page.getByRole("button", { name: "Arm" }).click();
  expect(armed).toBe("demo");
  await page.getByRole("tab", { name: "Watches" }).click();
  await page.getByLabel("Watch topic").fill("Rust releases");
  await page.getByRole("button", { name: "Create" }).click();
  await expect(page.getByText("Rust releases")).toBeVisible();
  await page.getByRole("button", { name: "Delete" }).click();
  expect(watches).toEqual([]);
  if (isMobile) expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
