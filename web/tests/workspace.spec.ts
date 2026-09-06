import { expect, test } from "@playwright/test";

test("same-origin pairing, workspace navigation, files and streamed answer", async ({ page, isMobile }) => {
  let space = "default";
  let uploaded = false;
  let appSource = "<h1>Demo</h1>";
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "test", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/v1/apps") {
      await route.fulfill({ json: { apps: space === "default" ? [{ name: "demo", url: "/apps/demo-id/", served_from: null }] : [] } });
    } else if (path === "/v1/apps/files") {
      await route.fulfill({ json: { files: ["index.html"] } });
    } else if (path === "/v1/apps/source") {
      if (route.request().method() === "PUT") appSource = route.request().postDataJSON().content;
      await route.fulfill({ json: { content: appSource, hash: "revision" } });
    } else if (path === "/v1/events") {
      await route.fulfill({ contentType: "text/event-stream", body: ': heartbeat\r\n\r\ndata: {"type":"stream","payload":[1,{"Token":"Hello from SSE"}]}\r\n\r\n' });
    } else if (path === "/v1/snapshot") {
      await route.fulfill({ json: { active_session_id: space === "default" ? "chat" : null, active_space_id: space, active_space_name: space, sessions: space === "default" ? [{ id: "chat", title: "Test chat", model: "test", kind: "chat" }] : [], models: [], settings: {}, tasks: [{ id: 1, session_id: "chat", session_title: "Test chat", status: "streaming" }] } });
    } else if (path === "/v1/spaces") {
      await route.fulfill({ json: { spaces: [{ id: "default", name: "default" }, { id: "other", name: "other" }] } });
    } else if (path === "/v1/files") {
      if (route.request().method() === "PUT") { uploaded = true; await route.fulfill({ json: { ok: true } }); }
      else await route.fulfill({ json: { files: uploaded && url.searchParams.get("space_id") === "default" ? [{ id: "file", name: "notes.txt", size: 5, status: "ok" }] : [] } });
    } else if (path === "/v1/command") {
      const body = route.request().postDataJSON();
      if (body.SwitchSpace) space = body.SwitchSpace.name;
      await route.fulfill({ json: { ok: true } });
    } else if (path.endsWith("/messages")) {
      await route.fulfill({ json: { messages: [] } });
    } else await route.fulfill({ status: 404, json: { error: { message: "not mocked" } } });
  });
  await page.route("**/apps/demo-id/**", (route) => route.fulfill({ contentType: "text/html", headers: { "Content-Security-Policy": "sandbox allow-scripts" }, body: "<h1>App preview content</h1>" }));
  await page.goto("/");
  await expect(page.getByLabel("Host URL")).toHaveValue("http://127.0.0.1:5173");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  await expect(page.getByText("Hello from SSE", { exact: true })).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByRole("navigation", { name: "Workspace", exact: true }).getByRole("button", { name: "Apps", exact: true }).click();
  await page.getByRole("button", { name: "demo", exact: true }).click();
  await expect(page.getByRole("link", { name: "Launch app" })).toHaveAttribute("href", "http://127.0.0.1:5173/apps/demo-id/");
  await page.getByRole("button", { name: "Preview app", exact: true }).click();
  await expect(page.frameLocator('iframe[title="demo preview"]').getByRole("heading", { name: "App preview content" })).toBeVisible();
  await page.getByLabel("App source file").selectOption("index.html");
  await expect(page.getByLabel("App source editor")).toHaveValue("<h1>Demo</h1>");
  await page.getByLabel("App source editor").fill("<h1>Updated</h1>");
  await page.getByRole("button", { name: "Save file", exact: true }).click();
  await expect(page.getByRole("button", { name: "Save file", exact: true })).toBeDisabled();
  expect(appSource).toBe("<h1>Updated</h1>");
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByRole("navigation", { name: "Workspace", exact: true }).getByRole("button", { name: "Files", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Files", exact: true, level: 1 })).toBeVisible();
  await page.getByLabel("Upload file").setInputFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") });
  await expect(page.getByText("notes.txt", { exact: true })).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByLabel("Space", { exact: true }).selectOption("other");
  if (isMobile) await page.getByRole("button", { name: "Close sidebar", exact: true }).click();
  await expect(page.getByText("No files in this space yet.")).toBeVisible();
  await expect(page.getByText("notes.txt", { exact: true })).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  expect(errors).toEqual([]);
});
