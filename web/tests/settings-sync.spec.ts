import { expect, test, type Page } from "@playwright/test";

const settings = {
  show_stats: true,
  show_reasoning: false,
  hide_hints: false,
  verbosity: "normal",
  usage_range: "week",
  temperature: 0.7,
  top_p: 1,
  max_tokens: 4000,
  compact_threshold: 80,
  memory_model: "memory-model",
  transcriber_model: "vision-model",
  ocr_model: "ocr-model",
  ocr_engine: "auto",
  local_ocr_model: "",
  embedding_model: "embedding-model",
  image_gen_model: "image-model",
  video_gen_model: "video-model",
  search_provider: "langsearch",
  searxng_url: "https://search.example.test",
  langsearch_key: "server-side-secret",
  blocked_domains: ["ads.example", "tracking.example"],
};

async function connect(page: Page) {
  await page.goto("/");
  await page.getByLabel("Host token").fill("host-token");
  await page.getByRole("button", { name: "Connect", exact: false }).click();
  await expect(page.getByRole("heading", { name: "Nexus Chat" })).toBeVisible();
}

async function openWorkspacePage(page: Page, name: string, isMobile: boolean) {
  if (isMobile) await page.getByRole("button", { name: "Open sidebar" }).click();
  await page.getByRole("navigation", { name: "Workspace", exact: true }).getByRole("button", { name, exact: true }).click();
}

function snapshot() {
  return {
    active_session_id: "chat",
    active_space_id: "default",
    active_space_name: "default",
    sessions: [{ id: "chat", title: "Test chat", model: "test", kind: "chat" }],
    models: [],
    settings: {},
    tasks: [],
  };
}

test("settings save only sends changed values and login keeps provider keys write-only", async ({ page, isMobile }) => {
  const puts: { key: string; value: string; space_id?: string }[] = [];
  let login = { pending: false, user_code: null, verification_url: "", message: "", configured: ["openrouter"] };
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "settings", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\r\n\r\n" });
    if (path === "/v1/snapshot") return route.fulfill({ json: snapshot() });
    if (path === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "default", name: "default" }] } });
    if (path === "/v1/sessions/chat/messages") return route.fulfill({ json: { messages: [] } });
    if (path === "/v1/settings" && request.method() === "GET") return route.fulfill({ json: settings });
    if (path === "/v1/settings" && request.method() === "PUT") {
      puts.push(request.postDataJSON());
      return route.fulfill({ json: { ok: true } });
    }
    if (path === "/v1/login" && request.method() === "GET") return route.fulfill({ json: login });
    if (path === "/v1/login" && request.method() === "POST") {
      const body = request.postDataJSON();
      expect(body.key).toBe("new-secret");
      expect(body.cancel).toBe(false);
      login = { ...login, configured: ["openrouter", body.backend] };
      return route.fulfill({ json: { ok: true } });
    }
    return route.fulfill({ status: 404, json: { error: { message: `unmocked ${path}` } } });
  });

  await connect(page);
  await openWorkspacePage(page, "Settings", isMobile);
  await expect(page.locator("h1").filter({ hasText: "Settings" })).toBeVisible();
  await expect(page.getByLabel("LangSearch key")).toHaveValue("");
  await page.getByLabel("Answer length").selectOption("concise");
  await page.getByRole("button", { name: "Save settings" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Settings saved." })).toBeVisible();
  expect(puts).toEqual([{ key: "verbosity", value: "concise", space_id: "default" }]);

  await page.getByLabel("API key").fill("new-secret");
  await page.getByRole("button", { name: "Save provider key" }).click();
  await expect(page.getByLabel("API key")).toHaveValue("");
  await expect(page.locator(".login-panel").getByText("openrouter, openrouter")).toBeVisible();
});

test("sync performs the peer handshake, transfers files, and refreshes state", async ({ page, isMobile }) => {
  const localPosts: unknown[] = [];
  const peerPosts: unknown[] = [];
  const transferred: string[] = [];
  const localFiles = new Map([["notes.txt", "local"]]);
  const peerFiles = new Map([["peer.txt", "peer"]]);
  await page.route("**/.well-known/nexus", (route) => route.fulfill({ json: { host_id: "local", product: "nexus-chat", version: "test", api: "/v1", authentication: "bearer" } }));
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const peer = url.pathname.startsWith("/peer/");
    const path = peer ? url.pathname.slice("/peer".length) : url.pathname;
    if (peer && request.method() === "OPTIONS") return route.fulfill({ status: 204, headers: { "Access-Control-Allow-Origin": "*", "Access-Control-Allow-Methods": "GET,POST,PUT,OPTIONS", "Access-Control-Allow-Headers": "authorization,content-type" } });
    if (path === "/v1/events") return route.fulfill({ contentType: "text/event-stream", body: ": heartbeat\r\n\r\n" });
    if (path === "/v1/snapshot") return route.fulfill({ json: snapshot() });
    if (path === "/v1/spaces") return route.fulfill({ json: { spaces: [{ id: "default", name: "default" }] } });
    if (path === "/v1/sessions/chat/messages") return route.fulfill({ json: { messages: [] } });
    if (path === "/v1/sync/state") return route.fulfill({ json: { device_id: peer ? "peer-device" : "local-device", device_name: peer ? "Peer" : "Local", peers: [] } });
    if (path === "/v1/sync" && request.method() === "GET") {
      const body = {
        device_id: peer ? "peer-device" : "local-device",
        files: peer
          ? [{ space_id: "default", name: "notes.txt", hash: "hash-a", size: 5 }, { space_id: "default", name: "peer.txt", hash: "hash-b", size: 4 }]
          : [{ space_id: "default", name: "notes.txt", hash: "hash-a", size: 5 }],
        rows: [{ table: "settings" }],
        tombstones: [],
      };
      return route.fulfill({ json: body });
    }
    if (path === "/v1/sync" && request.method() === "POST") {
      (peer ? peerPosts : localPosts).push(request.postDataJSON());
      return route.fulfill({ json: { device_id: peer ? "peer-device" : "local-device", files: peer ? [{ space_id: "default", name: "peer.txt", hash: "hash-b", size: 4 }] : [], rows: [], tombstones: [] } });
    }
    if (path === "/v1/sync/blob" && request.method() === "GET") {
      const name = url.searchParams.get("name") ?? "";
      const value = (peer ? peerFiles : localFiles).get(name);
      if (value === undefined) return route.fulfill({ status: 404, json: { error: { message: "blob missing" } } });
      return route.fulfill({ contentType: "application/octet-stream", body: value });
    }
    if (path === "/v1/sync/blob" && request.method() === "PUT") {
      const name = url.searchParams.get("name") ?? "";
      (peer ? peerFiles : localFiles).set(name, "transferred");
      transferred.push(`${peer ? "peer" : "local"}:${name}`);
      return route.fulfill({ json: { ok: true } });
    }
    return route.fulfill({ status: 404, json: { error: { message: `unmocked ${url.href}` } } });
  });

  await connect(page);
  await openWorkspacePage(page, "Sync", isMobile);
  await expect(page.getByRole("heading", { name: "Sync and devices", exact: true })).toBeVisible();
  await page.getByLabel("Peer host URL").fill("http://127.0.0.1:5173/peer");
  await page.getByLabel("Peer bearer token").fill("peer-token");
  await page.getByRole("button", { name: "Sync now" }).click();
  await expect(page.getByRole("status")).toContainText("Sync complete.");
  expect(localPosts).toHaveLength(1);
  expect(peerPosts).toHaveLength(2);
  expect(transferred).toEqual(["peer:notes.txt", "local:peer.txt"]);
  expect([...localFiles.keys()].sort()).toEqual(["notes.txt", "peer.txt"]);
  expect([...peerFiles.keys()].sort()).toEqual(["notes.txt", "peer.txt"]);
});
