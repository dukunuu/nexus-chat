import { expect, test } from "@playwright/test";

test("media workspace uses authenticated blobs and media tools", async ({ page, isMobile }) => {
  const calls: { path: string; body?: unknown }[] = [];
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "media", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.pathname === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\r\n\r\n" });
    if (url.pathname === "/v1/snapshot") return route.fulfill({ json: { active_session_id: "chat", active_space_id: "default", active_space_name: "default", sessions: [{ id: "chat", title: "Test", model: "test", kind: "chat" }], models: [], settings: {}, tasks: [] } });
    if (url.pathname === "/v1/media" && request.method() === "GET") return route.fulfill({ json: { space_id: "default", files: [{ name: "note.txt", size: 4, modified: "now", mime: "text/plain", kind: "file" }], images: [{ name: "photo.png", size: 4, modified: "now", mime: "image/png", kind: "image" }], videos: [], scripts: [] } });
    if (url.pathname === "/v1/media/blob") return route.fulfill({ contentType: "image/png", body: Buffer.from("png") });
    if (url.pathname === "/v1/tools/run") { calls.push({ path: url.pathname, body: request.postDataJSON() }); return route.fulfill({ json: { result: { result: "generated" } } }); }
    return route.fulfill({ status: 404, json: { error: { message: "unmocked" } } });
  });
  await page.goto("/");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByRole("navigation", { name: "Workspace" }).getByRole("button", { name: "Images/scripts" }).click();
  await page.getByRole("tab", { name: "Images" }).click();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByRole("img", { name: "photo.png" })).toBeVisible();
  await page.getByLabel("Prompt").fill("a red kite");
  await page.getByRole("button", { name: "Generate image" }).click();
  expect(calls[0].body).toMatchObject({ params: { name: "media", arguments: { action: "generate_image", prompt: "a red kite" } } });
});
