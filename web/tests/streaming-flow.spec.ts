import { expect, test } from "@playwright/test";

test("streamed Markdown updates, local images authenticate, and failed reload preserves the answer", async ({ page }) => {
  let failMessages = false;
  await page.addInitScript(() => {
    const original = window.fetch.bind(window);
    window.fetch = async (input, init) => {
      if (String(input).endsWith("/v1/events")) return new Response(new ReadableStream({
        start(controller) {
          Object.assign(window, { emitNexus: (event: unknown) => controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify(event)}\n\n`)) });
        },
      }), { headers: { "Content-Type": "text/event-stream" } });
      return original(input, init);
    };
  });
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "stream", product: "nexus-chat" } }));
  await page.route("**/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/v1/snapshot") return route.fulfill({ json: {
      active_session_id: "chat", active_space_id: "space", active_space_name: "default",
      sessions: [{ id: "chat", title: "Stream test", model: "test" }], models: [], settings: {},
      tasks: [{ id: 1, session_id: "chat", status: "streaming" }],
    } });
    if (path === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "space", name: "default" }] } });
    if (path.endsWith("/messages")) return route.fulfill(failMessages ? { status: 503, json: { error: { message: "Temporary reload failure" } } } : { json: { messages: [] } });
    if (path === "/v1/attachments") {
      expect(new URL(route.request().url()).searchParams.get("space_id")).toBe("space");
      expect(route.request().method()).toBe("POST");
      return route.fulfill({ json: { markdown: "![pasted image](uploaded.png)" } });
    }
    if (path === "/v1/media/blob") {
      expect(route.request().headers().authorization).toBe("Bearer test-token");
      return route.fulfill({ contentType: "image/png", body: Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j4uoAAAAASUVORK5CYII=", "base64") });
    }
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto("/");
  await page.getByLabel("Host token").fill("test-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  await expect(page.getByRole("heading", { name: "Stream test" })).toBeVisible();
  const emit = (event: unknown) => page.evaluate((value) => (window as unknown as { emitNexus: (event: unknown) => void }).emitNexus(value), event);
  await emit({ type: "stream", payload: [1, { Token: "Hello" }] });
  await expect(page.locator(".draft-message .markdown")).toHaveText("Hello");
  await emit({ type: "stream", payload: [1, { Token: " **world**\n\n![sample](sample.png)" }] });
  await expect(page.locator(".draft-message strong").filter({ hasText: "world" })).toBeVisible();
  await expect(page.getByRole("img", { name: "sample" })).toHaveAttribute("src", /^blob:/);
  await page.getByLabel("Attach image", { exact: true }).setInputFiles({ name: "sample.png", mimeType: "image/png", buffer: Buffer.from("test image") });
  await expect(page.getByPlaceholder("Ask Nexus anything…")).toHaveValue("![pasted image](uploaded.png)");
  await emit({ type: "composer_set", payload: "Restored draft" });
  await expect(page.getByPlaceholder("Ask Nexus anything…")).toHaveValue("Restored draft");
  failMessages = true;
  await emit({ type: "stream", payload: [1, "Done"] });
  await expect(page.getByText("Temporary reload failure")).toBeVisible();
  await expect(page.locator(".draft-message .markdown")).toContainText("Hello world");
});
