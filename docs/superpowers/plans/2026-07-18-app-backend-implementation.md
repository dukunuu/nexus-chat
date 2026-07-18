# App Backend: KV Store, File Uploads, UUID URLs — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the appserver so web apps get persistent KV storage, file upload capability, and access to user-uploaded images/files — all reachable via UUID-based URLs.

**Architecture:** Extend the existing hand-rolled TCP appserver with REST API endpoints under `/_api/`. Per-app SQLite for KV. Manual multipart parsing (no new deps). Tools bridge conversation images and space files into apps. UUIDs replace space names in URLs.

**Tech Stack:** Rust, tokio, rusqlite (already in deps), no new crate deps.

---

### Task 1: App Registry (`_apps.json`)

**Files:**
- Modify: `src/appserver.rs`
- Test: inline (new test functions in appserver.rs)

Add an app registry mapping UUID → `(space, name)`:

```rust
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    pub space: String,
    pub name: String,
}

/// The app registry: maps UUIDs to (space, name). Thread-safe via RwLock.
#[derive(Debug, Clone)]
pub struct AppRegistry {
    inner: Arc<RwLock<HashMap<String, AppEntry>>>,
    path: std::path::PathBuf,
}
```

- [ ] **Step 1: Add `AppRegistry` struct with load/save/lookup methods**

```rust
impl AppRegistry {
    /// Load from `_apps.json` at `spaces_root`, creating an empty one if missing.
    pub fn load(spaces_root: &std::path::Path) -> Self {
        let path = spaces_root.join("_apps.json");
        let map = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        AppRegistry { inner: Arc::new(RwLock::new(map)), path }
    }

    /// Assign a new UUID for (space, name). Returns the UUID.
    pub fn assign(&self, space: &str, name: &str) -> String {
        let uuid = uuid::Uuid::new_v4().to_string();
        {
            let mut map = self.inner.write().unwrap();
            map.insert(uuid.clone(), AppEntry { space: space.to_string(), name: name.to_string() });
        }
        let _ = self.save();
        uuid
    }

    /// Lookup UUID → AppEntry.
    pub fn lookup(&self, uuid: &str) -> Option<AppEntry> {
        self.inner.read().unwrap().get(uuid).cloned()
    }

    /// Resolve an app by name within a space → UUID.
    pub fn resolve(&self, space: &str, name: &str) -> Option<String> {
        self.inner.read().unwrap().iter()
            .find(|(_, e)| e.space == space && e.name == name)
            .map(|(u, _)| u.clone())
    }

    /// Save to disk (write temp, rename for atomicity).
    fn save(&self) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(&*self.inner.read().unwrap())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
```

Also add a method on `AppServer` to return an app URL by UUID:

```rust
pub fn app_url(&self, uuid: &str) -> String {
    format!("http://127.0.0.1:{}/{}/", self.port(), uuid)
}
```

And modify `AppServer` to hold a `AppRegistry`:

```rust
pub struct AppServer {
    port: u16,
    registry: AppRegistry,
    spaces_root: PathBuf,
}
```

- [ ] **Step 2: Replace `start()` signature** — pass `spaces_root`, load registry, store in struct:

```rust
pub async fn start(spaces_root: PathBuf) -> Option<AppServer> {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(("127.0.0.1", 0)).await.ok()?,
    };
    let port = listener.local_addr().ok()?.port();
    let registry = AppRegistry::load(&spaces_root);
    let srv = AppServer { port, registry: registry.clone(), spaces_root: spaces_root.clone() };
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let root = spaces_root.clone();
            let reg = registry.clone();
            tokio::spawn(async move { let _ = handle(stream, &root, &reg).await; });
        }
    });
    Some(srv)
}
```

Change `handle()` signature to accept `&AppRegistry`. Expose `registry()` getter on `AppServer`.

- [ ] **Step 3: Remove old `space_url()` method, add `registry()` and `spaces_root()` getters**:

```rust
pub fn registry(&self) -> &AppRegistry { &self.registry }
pub fn spaces_root(&self) -> &Path { &self.spaces_root }
```

- [ ] **Step 4: Update the `resolve()` function** to check UUIDs first, fall back to `(space, app)` path:

```rust
fn resolve(spaces_root: &Path, registry: &AppRegistry, raw_path: &str) -> Option<PathBuf> {
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let mut segs = decoded.split('/').filter(|s| !s.is_empty());
    let first = segs.next()?; // could be UUID or space name
    let second = segs.next().unwrap_or("");
    let rest: Vec<&str> = segs.collect();

    // API route: /<uuid>/_api/...
    if second == "_api" {
        return resolve_api_path(spaces_root, registry, &first, &rest[1..]);
    }

    // Try UUID lookup first
    let (space, app) = if let Some(entry) = registry.lookup(first) {
        (entry.space, entry.app)
    } else {
        // Fall back to old space/app format
        if second.is_empty() { return None; }
        (first.to_string(), second.to_string())
    };

    let mut file = spaces_root.join(&space).join("apps").join(&app);
    for seg in &rest {
        for s in [&space, &app].iter().chain(rest.iter()) {
            if *s == ".." || *s == "." || s.contains('\\') { return None; }
        }
        file.push(seg);
    }
    if file.is_dir() || second.is_empty() {
        file.push("index.html");
    }
    let canon = file.canonicalize().ok()?;
    let root = spaces_root.canonicalize().ok()?;
    canon.starts_with(&root).then_some(canon)
}
```

Wait — the traversal guard should check all segments including the UUID. Let me fix the logic:

```rust
fn resolve(spaces_root: &Path, registry: &AppRegistry, raw_path: &str) -> Option<PathBuf> {
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let mut segs: Vec<&str> = decoded.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() { return None; }
    for seg in &segs {
        if *seg == ".." || *seg == "." || seg.contains('\\') { return None; }
    }
    // API route: /<uuid>/_api/...
    if segs.len() >= 2 && segs[1] == "_api" {
        return resolve_api(spaces_root, registry, &segs);
    }
    let (space, app) = match registry.lookup(&segs[0]) {
        Some(entry) => (entry.space, entry.app),
        None => {
            if segs.len() < 2 { return None; }
            (segs[0].to_string(), segs[1].to_string())
        }
    };
    let mut file = spaces_root.join(&space).join("apps").join(&app);
    for seg in &segs[2..] {
        file.push(seg);
    }
    if file.is_dir() || segs.len() < 3 {
        file.push("index.html");
    }
    let canon = file.canonicalize().ok()?;
    let root = spaces_root.canonicalize().ok()?;
    canon.starts_with(&root).then_some(canon)
}
```

- [ ] **Step 5: Add `resolve_api()` for API routes**: Separate function that resolves the app dir and dispatches to API handlers.

```rust
fn resolve_api(spaces_root: &Path, registry: &AppRegistry, segs: &[&str]) -> Option<PathBuf> {
    // segs = [uuid, "_api", ...]
    let entry = registry.lookup(segs[0])?;
    let app_dir = spaces_root.join(&entry.space).join("apps").join(&entry.name);
    app_dir.is_dir().then_some(app_dir)
}
```

- [ ] **Step 6: Update tests** to work with the new registry-aware resolve().

```rust
#[test]
fn resolve_uuid() {
    let root = &setup();
    let reg = AppRegistry::load(root);
    let uuid = reg.assign("default", "deck");
    let f = resolve(root, &reg, &format!("/{uuid}/style.css")).unwrap();
    assert!(f.ends_with("default/apps/deck/style.css"));
}
```

- [ ] **Step 7: Run tests to verify**

Run: `cargo test -p nexus-chat -- appserver::tests --nocapture`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/appserver.rs
git commit -m "feat: add app registry with UUID routing"
```

---

### Task 2: Appserver API Infrastructure — CORS, Method Dispatch, Body Reading

**Files:**
- Modify: `src/appserver.rs`

- [ ] **Step 1: Add `Options` variant to method parsing and expand allowed methods**

Change the method check from:

```rust
let head = method == "HEAD";
if method != "GET" && !head {
    return respond(&mut stream, 405, "text/plain", b"method not allowed", head).await;
}
```

To:

```rust
let is_api = raw_path.contains("/_api/");
if method != "GET" && method != "HEAD" && method != "POST" && method != "PUT" && method != "DELETE" && method != "OPTIONS" {
    return respond(&mut stream, 405, "text/plain", b"method not allowed", false).await;
}
if method == "OPTIONS" {
    return respond_cors(&mut stream).await;
}
if !is_api && method != "GET" && method != "HEAD" {
    return respond(&mut stream, 405, "text/plain", b"only GET allowed outside /_api/", false).await;
}
```

- [ ] **Step 2: Add `respond_cors()` function**:

```rust
async fn respond_cors(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    let header = "HTTP/1.1 204 No Content\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Methods: GET, HEAD, POST, PUT, DELETE, OPTIONS\r\n\
        Access-Control-Allow-Headers: Content-Type\r\n\
        Access-Control-Max-Age: 86400\r\n\
        Content-Length: 0\r\n\r\n";
    stream.write_all(header.as_bytes()).await?;
    stream.shutdown().await
}
```

- [ ] **Step 3: Read request body for POST/PUT/DELETE**

Currently `handle()` reads up to 8 KiB of raw request (header only). Need to parse Content-Length and read the body for API calls:

```rust
async fn handle(mut stream: tokio::net::TcpStream, spaces_root: &Path, registry: &AppRegistry) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        let n = stream.read(&mut chunk).await?;
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
    }
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
    let header_bytes = &buf[..header_end];
    let header_text = std::str::from_utf8(header_bytes).unwrap_or("").to_string();
    let mut lines = header_text.lines();
    let request_line = lines.next().unwrap_or("").to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");

    // Parse Content-Length
    let content_length: usize = lines
        .filter_map(|l| l.strip_prefix("Content-Length: ").or_else(|| l.strip_prefix("content-length: ")))
        .filter_map(|v| v.trim().parse().ok())
        .next()
        .unwrap_or(0);

    let max_body = 10 * 1024 * 1024; // 10 MB
    if content_length > max_body {
        return respond(&mut stream, 413, "text/plain", b"body too large", false).await;
    }

    // Read remaining body (might already be partially in buf after headers)
    let body_start = header_end + 4; // skip \r\n\r\n
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 { break; }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(std::cmp::min(body.len(), content_length));

    // Content-Type for multipart parsing
    let content_type: &str = lines
        .filter_map(|l| l.strip_prefix("Content-Type: ").or_else(|| l.strip_prefix("content-type: ")))
        .next()
        .unwrap_or("");

    // Dispatch
    let is_api = raw_path.contains("/_api/");
    if method == "OPTIONS" {
        return respond_cors(&mut stream).await;
    }
    if !is_api && method != "GET" && method != "HEAD" {
        return respond(&mut stream, 405, "text/plain", b"only GET allowed outside /_api/", false).await;
    }
    if is_api && (method == "POST" || method == "PUT" || method == "DELETE" || method == "GET") {
        return handle_api(&mut stream, spaces_root, registry, method, raw_path, &body, content_type, head).await;
    }
    // Fall through to static file serving for GET/HEAD
    // ... rest of existing handle() logic
}
```

Note: `head` is now unused for API routes. API routes always return full body.

- [ ] **Step 4: Add `handle_api()` dispatcher**:

```rust
async fn handle_api(
    stream: &mut tokio::net::TcpStream,
    spaces_root: &Path,
    registry: &AppRegistry,
    method: &str,
    raw_path: &str,
    body: &[u8],
    content_type: &str,
) -> std::io::Result<()> {
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let segs: Vec<&str> = decoded.split('/').filter(|s| !s.is_empty()).collect();
    // segs = [uuid, "_api", ...]
    if segs.len() < 3 {
        return respond(stream, 404, "text/plain", b"not found", false).await;
    }
    let Some(entry) = registry.lookup(segs[0]) else {
        return respond(stream, 404, "text/plain", b"unknown app", false).await;
    };
    let app_dir = spaces_root.join(&entry.space).join("apps").join(&entry.name);
    if !app_dir.is_dir() {
        return respond(stream, 404, "text/plain", b"app not found on disk", false).await;
    }

    let sub = segs[2]; // "kv" or "upload"
    match sub {
        "kv" => handle_kv(stream, &app_dir, method, &segs[3..], body).await,
        "upload" if method == "POST" => handle_upload(stream, &app_dir, body, content_type).await,
        _ => respond(stream, 404, "text/plain", b"unknown api endpoint", false).await,
    }
}
```

- [ ] **Step 5: Update respond() to accept CORS headers for API responses**:

Add CORS headers to all API responses. Modify `respond()` to accept a `cors: bool` parameter:

```rust
async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    head: bool,
) -> std::io::Result<()> {
    let reason = match status { 200 => "OK", 201 => "Created", 204 => "No Content", 400 => "Bad Request", 404 => "Not Found", 405 => "Method Not Allowed", 413 => "Request Entity Too Large", _ => "Not Found" };
    let mut header = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    // CORS for API responses
    if mime == "application/json" || status != 200 {
        header.push_str("Access-Control-Allow-Origin: *\r\n");
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;
    if !head && !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p nexus-chat -- appserver::tests --nocapture`
Expected: existing tests still pass

- [ ] **Step 7: Commit**

```bash
git add src/appserver.rs
git commit -m "feat: add CORS, method dispatch, and body reading to appserver"
```

---

### Task 3: API — KV Store Endpoints

**Files:**
- Modify: `src/appserver.rs`

- [ ] **Step 1: Add `handle_kv()` function**

Per-app SQLite at `<app_dir>/_store.db`:

```rust
async fn handle_kv(
    stream: &mut tokio::net::TcpStream,
    app_dir: &std::path::Path,
    method: &str,
    segs: &[&str],
    body: &[u8],
) -> std::io::Result<()> {
    let db_path = app_dir.join("_store.db");
    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => { let _ = c.execute_batch("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)"); c }
        Err(e) => return respond(stream, 500, "text/plain", format!("db error: {e}").as_bytes(), false).await,
    };

    match method {
        "GET" if segs.is_empty() => {
            // List all keys
            let mut stmt = match conn.prepare("SELECT key FROM kv ORDER BY key") {
                Ok(s) => s,
                Err(e) => return respond(stream, 500, "text/plain", format!("query error: {e}").as_bytes(), false).await,
            };
            let keys: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap_or_else(|_| Box::new(std::iter::empty()))
                .filter_map(|r| r.ok())
                .collect();
            let json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string());
            respond(stream, 200, "application/json", json.as_bytes(), false).await
        }
        "GET" => {
            let key = percent_decode(segs.join("/").as_str());
            match conn.query_row("SELECT value FROM kv WHERE key = ?1", [&key], |r| r.get::<_, String>(0)) {
                Ok(value) => respond(stream, 200, "text/plain", value.as_bytes(), false).await,
                Err(rusqlite::Error::QueryReturnedNoRows) => respond(stream, 404, "text/plain", b"not found", false).await,
                Err(e) => respond(stream, 500, "text/plain", format!("read error: {e}").as_bytes(), false).await,
            }
        }
        "PUT" => {
            let key = percent_decode(segs.join("/").as_str());
            let value = std::str::from_utf8(body).unwrap_or("");
            match conn.execute("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)", rusqlite::params![key, value]) {
                Ok(_) => respond(stream, 200, "text/plain", b"ok", false).await,
                Err(e) => respond(stream, 500, "text/plain", format!("write error: {e}").as_bytes(), false).await,
            }
        }
        "DELETE" => {
            let key = percent_decode(segs.join("/").as_str());
            match conn.execute("DELETE FROM kv WHERE key = ?1", [key]) {
                Ok(0) => respond(stream, 404, "text/plain", b"not found", false).await,
                Ok(_) => respond(stream, 200, "text/plain", b"deleted", false).await,
                Err(e) => respond(stream, 500, "text/plain", format!("delete error: {e}").as_bytes(), false).await,
            }
        }
        _ => respond(stream, 405, "text/plain", b"method not allowed", false).await,
    }
}
```

- [ ] **Step 2: Write KV store tests in appserver.rs**

```rust
#[tokio::test]
async fn kv_roundtrip() {
    let (srv, root) = setup_with_registry().await;
    let uuid = srv.registry().assign("default", "kvtest");
    let c = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", srv.port());

    // PUT
    let r = c.put(format!("{base}/{uuid}/_api/kv/hello"))
        .body("world")
        .send().await.unwrap();
    assert_eq!(r.status(), 200);

    // GET
    let r = c.get(format!("{base}/{uuid}/_api/kv/hello"))
        .send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "world");

    // DELETE
    let r = c.delete(format!("{base}/{uuid}/_api/kv/hello"))
        .send().await.unwrap();
    assert_eq!(r.status(), 200);

    // GET after delete = 404
    let r = c.get(format!("{base}/{uuid}/_api/kv/hello"))
        .send().await.unwrap();
    assert_eq!(r.status(), 404);
}
```

```rust
#[tokio::test]
async fn kv_list_keys() {
    let (srv, root) = setup_with_registry().await;
    let uuid = srv.registry().assign("default", "kvlist");
    let c = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", srv.port());

    c.put(format!("{base}/{uuid}/_api/kv/a")).body("1").send().await.unwrap();
    c.put(format!("{base}/{uuid}/_api/kv/b")).body("2").send().await.unwrap();
    c.put(format!("{base}/{uuid}/_api/kv/c")).body("3").send().await.unwrap();

    let r = c.get(format!("{base}/{uuid}/_api/kv")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let keys: Vec<String> = r.json().await.unwrap();
    assert_eq!(keys, vec!["a", "b", "c"]);
}
```

Need to update `setup()` to create and return a usable AppServer:

```rust
async fn setup_with_registry() -> (AppServer, PathBuf) {
    let tmp = std::env::temp_dir().join(format!("nexus-appserver-{}", uuid::Uuid::new_v4()));
    let app = tmp.join("default").join("apps").join("deck");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("index.html"), "<h1>slides</h1>").unwrap();
    let srv = AppServer::start(tmp.clone()).await.unwrap();
    (srv, tmp)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p nexus-chat -- appserver::tests --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/appserver.rs
git commit -m "feat: add KV store API (GET/PUT/DELETE per-app SQLite)"
```

---

### Task 4: API — File Upload Endpoint

**Files:**
- Modify: `src/appserver.rs`

- [ ] **Step 1: Add `handle_upload()` with multipart parsing**

Parse multipart/form-data from raw bytes. No new crate. Use boundary from Content-Type header, find form-data parts, extract filename and body.

```rust
async fn handle_upload(
    stream: &mut tokio::net::TcpStream,
    app_dir: &std::path::Path,
    body: &[u8],
    content_type: &str,
) -> std::io::Result<()> {
    // Extract boundary from Content-Type: multipart/form-data; boundary=----...
    let boundary = match content_type.split(';').filter_map(|p| p.trim().strip_prefix("boundary=")).next() {
        Some(b) => b.trim_matches('"').to_string(),
        None => return respond(stream, 400, "text/plain", b"missing boundary in Content-Type", false).await,
    };

    // Find the first file part between --boundary\r\n and \r\n--boundary--
    let delimiter = format!("\r\n--{}", boundary);
    let start_marker = format!("--{}\r\n", boundary);

    let body_str = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return respond(stream, 400, "text/plain", b"upload body is not valid UTF-8", false).await,
    };

    let Some(part_start) = body_str.find(&start_marker) else {
        return respond(stream, 400, "text/plain", b"no multipart part found", false).await;
    };
    let header_start = part_start + start_marker.len();
    let Some(header_end) = body_str[header_start..].find("\r\n\r\n") else {
        return respond(stream, 400, "text/plain", b"malformed multipart part", false).await;
    };
    let part_headers = &body_str[header_start..header_start + header_end];

    // Extract filename from Content-Disposition
    let filename = part_headers.split(';')
        .filter_map(|p| p.trim().strip_prefix("filename="))
        .next()
        .map(|f| f.trim_matches('"').trim_matches('\'').to_string())
        .unwrap_or_else(|| "upload.bin".to_string());

    // Find body content (after \r\n\r\n) and end (before next --boundary)
    let body_start_idx = header_start + header_end + 4;
    let part_body_end = body_str[body_start_idx..].find(&delimiter)
        .map(|d| body_start_idx + d)
        .unwrap_or(body.len());

    let file_body = &body[body_start_idx..part_body_end];
    // Strip trailing \r\n before --boundary
    let file_body = if file_body.ends_with(b"\r\n") { &file_body[..file_body.len() - 2] } else { file_body };

    // Determine extension
    let ext = std::path::Path::new(&filename).extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_string());

    let uploads_dir = app_dir.join("_uploads");
    std::fs::create_dir_all(&uploads_dir).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("cannot create uploads dir: {e}"))
    })?;

    let file_id = uuid::Uuid::new_v4().to_string();
    let save_path = uploads_dir.join(format!("{file_id}.{ext}"));
    std::fs::write(&save_path, file_body).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("cannot save upload: {e}"))
    })?;

    let url = format!("/{}/_uploads/{file_id}.{ext}", /* uuid from caller segs[0] — need to pass it */);
    // Actually, we need the UUID. Let's pass it from handle_api.
    // We'll restructure handle_api to pass the UUID:
    // handle_upload(stream, &app_dir, segs[0], body, content_type)
}
```

Actually, I realize I need to pass the UUID to `handle_upload` so I can construct the upload URL. Let me adjust the signature:

```rust
async fn handle_upload(
    stream: &mut tokio::net::TcpStream,
    app_dir: &std::path::Path,
    app_uuid: &str,
    body: &[u8],
    content_type: &str,
) -> std::io::Result<()> {
    // ... same parsing as above ...
    let url = format!("/{app_uuid}/_uploads/{file_id}.{ext}");
    let json = serde_json::json!({"name": filename, "url": url});
    let body = serde_json::to_string(&json).unwrap_or_default();
    respond(stream, 200, "application/json", body.as_bytes(), false).await
}
```

And update `handle_api` to pass `segs[0]` as the UUID.

Also, the `_uploads/` directory needs to be served via static GET. Update the `resolve()` function: if the path matches `/_uploads/` (after SSO segments), serve from `app_dir/_uploads/<name>`. Actually, since UUID routing resolves to the app dir, and `_uploads/` is inside it, the normal GET should work — BUT the current resolve() appends extra segments. Let me check:

If URL is `GET /<uuid>/_uploads/file.png`:
- `resolve()` sees path: `/<uuid>/_uploads/file.png`
- segs = [uuid, "_uploads", "file.png"]
- UUID lookup → entry(space="default", app="deck")
- Then `file = spaces_root/default/apps/deck`
- Append segs[2..]: "_uploads", "file.png"
- Result: `spaces_root/default/apps/deck/_uploads/file.png` ✓
- Not a dir, no index.html appended ✓

But there's an issue: `_uploads` could be interpreted as part of the app path. Let me check... the code appends `segs[2..]` which includes `_uploads` and `file.png`. Then it checks if the file is a dir → it's not → no index.html appended. So it should work. But I need to make sure the `_uploads` check in the segment filter doesn't reject it. Since `_uploads` doesn't contain `..`, `.`, or `\`, it's fine.

Actually wait, there's a subtlety. In my new `resolve()`, I have:

```rust
if segs.len() >= 2 && segs[1] == "_api" {
    return resolve_api(spaces_root, registry, &segs);
}
```

This catches `/_api/` routes before UUID resolution. I should also do the same for `_uploads` to make sure it resolves correctly through the normal path. Actually, `_uploads` doesn't need special handling — it's just a directory inside the app. The normal UUID resolve path handles it fine because it appends all segments after the UUID as path components.

Let me verify the resolve flow for `GET /abc123/_uploads/file.png`:
1. segs = ["abc123", "_uploads", "file.png"]
2. Not an API route (segs[1] is "_uploads", not "_api")
3. UUID lookup for "abc123" → Some(AppEntry { space: "default", name: "deck" })
4. `file = spaces_root/default/apps/deck`
5. Append segs[2..]: "_uploads/file.png"
6. `file = spaces_root/default/apps/deck/_uploads/file.png`
7. Not a dir → no index.html appended
8. Canonicalize check ✓

Great, that works.

But wait, there's another issue with the new resolve. The current code has:

```rust
if file.is_dir() || rest.is_empty() {
    file.push("index.html");
}
```

In my new code:
```rust
if file.is_dir() || segs.len() < 3 {
    file.push("index.html");
}
```

For `GET /abc123/` (UUID only: segs = ["abc123"], len < 3): ✓ serves index.html
For `GET /abc123/style.css` (segs = ["abc123", "style.css"]): OK
For `GET /abc123/_uploads/file.png` (segs = ["abc123", "_uploads", "file.png"]): not a dir, segs.len() >= 3, so no index.html ✓

Good.

- [ ] **Step 2: Write upload tests in appserver.rs**

```rust
#[tokio::test]
async fn file_upload_and_serve() {
    let (srv, _root) = setup_with_registry().await;
    let uuid = srv.registry().assign("default", "uploadtest");
    let c = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", srv.port());

    // Fake multipart body
    let boundary = "----TestBoundary123";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\n\r\nHello World!\r\n--{boundary}--\r\n"
    );
    let r = c.post(format!("{base}/{uuid}/_api/upload"))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .send().await.unwrap();
    assert_eq!(r.status(), 200);
    let resp: serde_json::Value = r.json().await.unwrap();
    let url = resp["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("/{uuid}/_uploads/")));
    assert!(url.ends_with(".txt"));

    // Serve the uploaded file
    let r = c.get(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "Hello World!");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p nexus-chat -- appserver::tests --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/appserver.rs
git commit -m "feat: add file upload endpoint with multipart parsing"
```

---

### Task 5: Wire AppRegistry into AppsCtx + Tool Changes

**Files:**
- Modify: `src/tools.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Add `registry`, `space_db_path`, `images_dir`, `space` fields to `AppsCtx`**

```rust
pub struct AppsCtx {
    pub dir: PathBuf,
    pub space_url: String,
    pub registry: crate::appserver::AppRegistry,
    pub space_name: String,
    pub space_db_path: PathBuf,
    pub images_dir: PathBuf,
}
```

- [ ] **Step 2: Update `AppServer` to expose needed info**

Add `spaces_root()` accessor:

```rust
impl AppServer {
    pub fn spaces_root(&self) -> &Path { &self.spaces_root }
    pub fn registry(&self) -> &AppRegistry { &self.registry }
}
```

- [ ] **Step 3: Wire `AppsCtx` in `app/mod.rs`**

```rust
self.app_server.as_ref().map(|s| {
    let space = &self.active_space.name;
    let uuid = s.registry().resolve(space, /* will get from db */).unwrap_or_else(|| s.registry().assign(space, ""));
    crate::tools::AppsCtx {
        dir: self.space.apps_dir(space),
        space_url: s.app_url(&uuid), // will be updated when the app is created
        registry: s.registry().clone(),
        space_name: space.clone(),
        space_db_path: self.space.db_path(),
        images_dir: self.space.images_dir(space),
    }
})
```

Wait, this has a problem. `space_url` is used already for the `live at <url>` link in `write_file`. But with UUID URLs, the URL depends on the app's UUID, not the space. And the app UUID doesn't exist until `write_file` creates an app.

So the flow needs to change: `AppsCtx` doesn't have a fixed `space_url` anymore. Instead, tools construct URLs from the app UUID returned by the registry. The `app_link()` method can look up the UUID and construct the URL.

Let me refactor: Remove `space_url` from `AppsCtx`, add `server_port: u16` and `registry`. Tools use `registry` + `server_port` to construct URLs.

```rust
pub struct AppsCtx {
    pub dir: PathBuf,
    pub server_port: u16,
    pub registry: crate::appserver::AppRegistry,
    pub space_name: String,
    pub space_db_path: PathBuf,
    pub images_dir: PathBuf,
}
```

Then `app_link()` becomes:

```rust
fn app_link(&self, name: &str) -> String {
    let uuid = match &self.apps {
        Some(ctx) => ctx.registry.resolve(&ctx.space_name, name).unwrap_or_default(),
        None => return String::new(),
    };
    // Also accept app_uuid directly: if name looks like a UUID, use it as-is
    let uuid = if uuid.len() == 36 && uuid.chars().filter(|c| *c == '-').count() == 4 {
        uuid
    } else {
        uuid
    };
    if uuid.is_empty() {
        String::new()
    } else {
        format!("live at http://127.0.0.1:{}/{uuid}/", self.server_port, uuid)
    }
}
```

Wait, actually the port needs to come from somewhere. `ToolBox` currently gets it implicitly through `space_url` which was pre-computed. Let me add `server_port: u16` to `AppsCtx`.

- [ ] **Step 4: Update `app/mod.rs` wiring**

```rust
self.app_server.as_ref().map(|s| crate::tools::AppsCtx {
    dir: self.space.apps_dir(&self.active_space.name),
    server_port: s.port(),
    registry: s.registry().clone(),
    space_name: self.active_space.name.clone(),
    space_db_path: self.space.db_path(),
    images_dir: self.space.images_dir(&self.active_space.name),
})
```

- [ ] **Step 5: Update `app_link()` in `tools.rs`** as shown above.

- [ ] **Step 6: Wire `AppServer` in `main.rs` to expose port + registry**

The `AppServer.start()` already returns `Option<AppServer>`. After startup, `main.rs` stores it in `app.app_server`. The `app_server` field already exists in `App` — it's the `Option<AppServer>`. So `s.port()` and `s.registry()` just need to be added as getters (they are already `pub` fields or need accessors).

Check: `AppServer` in `appserver.rs` line 14-17:
```rust
pub struct AppServer {
    port: u16,
    registry: AppRegistry,
    spaces_root: PathBuf,
}
```

The `port()` method already exists. I need to add `pub fn registry()` and `pub fn spaces_root()`.

- [ ] **Step 7: Compile and fix any issues**

Run: `cargo check`
Expected: No errors

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs src/app/mod.rs src/appserver.rs
git commit -m "refactor: wire AppRegistry into AppsCtx, remove space_url in favor of UUID resolution"
```

---

### Task 6: Tools — UUID Return from write_file + app_uuid Support

**Files:**
- Modify: `src/tools.rs`

- [ ] **Step 1: Update `write_file` tool to register app when first write happens**

In `write_file` handler (`tools.rs` lines ~992-1021), after resolving the app path:

```rust
"write_file" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let field = |k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
    };
    let (app, path, content) = (field("app"), field("path"), field("content"));
    let status = format!("Writing {app}/{path}…");

    // Resolve UUID: if app_uuid is provided, use it; otherwise resolve by name
    let result = match self.resolve_app(&app) {
        Err(e) => e,
        Ok((uuid, _)) => {
            let path = format!("{uuid}"); // use UUID-based URL
            let app_url = self.app_link(&uuid);
            // ... write the file ...
            match std::fs::write(/* ... */) {
                Ok(()) => format!("wrote {app}/{path} ({} bytes) — {app_url}", content.len()),
                Err(e) => format!("write failed: {e}"),
            }
        }
    };
    (result, status)
}
```

Add a `resolve_app()` method:

```rust
fn resolve_app(&self, name_or_uuid: &str) -> Result<(String, PathBuf), String> {
    let ctx = self.apps.as_ref().ok_or("apps not available")?;
    let is_uuid = name_or_uuid.len() == 36 && name_or_uuid.chars().filter(|c| *c == '-').count() == 4;
    let (uuid, app_name) = if is_uuid {
        let entry = ctx.registry.lookup(name_or_uuid).ok_or_else(|| format!("unknown app uuid: {name_or_uuid}"))?;
        (name_or_uuid.to_string(), entry.name)
    } else {
        let uuid = match ctx.registry.resolve(&ctx.space_name, name_or_uuid) {
            Some(u) => u,
            None => ctx.registry.assign(&ctx.space_name, name_or_uuid),
        };
        (uuid, name_or_uuid.to_string())
    };
    let app_dir = ctx.dir.join(&app_name);
    Ok((uuid, app_dir))
}
```

- [ ] **Step 2: Update `app_link()`** to accept and use UUID:

```rust
fn app_link(&self, uuid: &str) -> String {
    match &self.apps {
        Some(ctx) => format!("live at http://127.0.0.1:{}/{uuid}/", ctx.server_port),
        None => String::new(),
    }
}
```

- [ ] **Step 3: Update `write_file` handler** completely:

Replace the old handler (lines ~992-1021):

```rust
"write_file" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let (app_or_uuid, path, content) = (field("app"), field("path"), field("content"));
    let status = format!("Writing {app_or_uuid}/{path}…");
    let result = match self.resolve_app(&app_or_uuid) {
        Err(e) => e,
        Ok((uuid, app_dir)) => {
            let file = app_dir.join(&path);
            let write = file.parent()
                .map(std::fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|()| std::fs::write(&file, &content));
            match write {
                Ok(()) => format!(
                    "wrote {app_or_uuid}/{path} ({} bytes) — {}",
                    content.len(),
                    self.app_link(&uuid),
                ),
                Err(e) => format!("write failed: {e}"),
            }
        }
    };
    (result, status)
}
```

- [ ] **Step 4: Update `read_app_file` handler** (line ~1096) to use `resolve_app()`:

Replace `self.app_path(&app, &path)` with `self.resolve_app(&app_or_uuid)` approach:

```rust
"read_app_file" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let app_or_uuid = field("app");
    let path = field("path");
    let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
    let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
    let status = format!("Reading {app_or_uuid}/{path}…");
    let result = match self.resolve_app(&app_or_uuid) {
        Err(e) => e,
        Ok((_, app_dir)) => {
            let file = app_dir.join(&path);
            // ... confinement check ...
            let file = resolve_confined_dir(&app_dir, &path)?;
            match std::fs::read_to_string(&file) {
                // ... same as before ...
            }
        }
    };
    (result, status)
}
```

Add `resolve_confined_dir()` helper (like `resolve_confined` but relative to a resolved dir):

```rust
fn resolve_confined_dir(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.starts_with('/') { return Err("path must be relative".to_string()); }
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
            return Err(format!("invalid path segment in {rel:?}"));
        }
    }
    let mut p = root.to_path_buf();
    for seg in rel.split('/') { p.push(seg); }
    Ok(p)
}
```

- [ ] **Step 5: Update remaining app tools** — `edit_file`, `grep_app`, `install_packages` — all replace `self.app_path(&app, ...)` with `self.resolve_app(&app_or_uuid)`.

Add a helper on `ToolBox`:

```rust
fn resolve_app(&self, uuid_or_name: &str) -> Result<(String, PathBuf), String> {
    let ctx = self.apps.as_ref().ok_or("apps not available")?;
    let (uuid, app_name) = if looks_like_uuid(uuid_or_name) {
        let entry = ctx.registry.lookup(uuid_or_name).ok_or_else(|| format!("unknown app: {uuid_or_name}"))?;
        (uuid_or_name.to_string(), entry.name)
    } else {
        let uuid = match ctx.registry.resolve(&ctx.space_name, uuid_or_name) {
            Some(u) => u,
            None => ctx.registry.assign(&ctx.space_name, uuid_or_name),
        };
        (uuid, uuid_or_name.to_string())
    };
    let app_dir = ctx.dir.join(&app_name);
    if !app_dir.is_dir() {
        std::fs::create_dir_all(&app_dir).map_err(|e| format!("cannot create app dir: {e}"))?;
    }
    Ok((uuid, app_dir))
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}
```

**Important**: `app_path()` was used for confinement. With `resolve_app()`, the confinement is on the app name level (registry lookup validates it) + individual file path via `resolve_confined_dir()`.

- [ ] **Step 6: Update `install_packages`** — when app is part of the call, find app dir:

Instead of `let dir = pkg_json.parent().unwrap().to_path_buf();` coming from `self.app_path()`, use:

```rust
Ok(()) if !app.is_empty() => {
    let (_, app_dir) = match self.resolve_app(&app) {
        Err(e) => return (e, status),
        Ok(t) => t,
    };
    let dir = app_dir;
    // ... rest of install_packages logic using dir ...
}
```

- [ ] **Step 7: Run tests**

Run: `cargo check`
Expected: Compiles

Run: `cargo test -p nexus-chat -- tools::tests --nocapture`
Expected: PASS (may need to update test toolboxes)

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add UUID support to app tools, register apps on write_file"
```

---

### Task 7: Tools — `list_images` and `copy_images_to_app`

**Files:**
- Modify: `src/tools.rs`
- Modify: `src/db.rs`

- [ ] **Step 1: Add `message_images_for_session` query to `db.rs`**

```rust
/// All images attached to messages in a session, ordered by message then created_at.
pub fn message_images_for_session(conn: &Connection, session_id: &str) -> Result<Vec<MessageImage>> {
    let mut stmt = conn.prepare(
        "SELECT mi.id, mi.path, mi.description FROM message_images mi
         JOIN messages m ON m.id = mi.message_id
         WHERE m.session_id = ?1
         ORDER BY m.created_at ASC, mi.created_at ASC"
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(MessageImage {
            id: r.get(0)?,
            path: r.get(1)?,
            description: r.get(2)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}
```

- [ ] **Step 2: Add `list_images` tool definition**

In `defs()`, after the existing app tools, add:

```rust
if self.apps.is_some() {
    defs.push(ToolDef {
        name: "list_images".to_string(),
        description: "List images the user has pasted in this conversation: [{id, description}]. Each image can be copied into an app with copy_images_to_app.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
        }),
    });
}
```

- [ ] **Step 3: Add `list_images` handler**

Inside `run()`, before the `"write_file"` match arm:

```rust
"list_images" => {
    let status = "Listing images…".to_string();
    let result = match &self.apps {
        None => "no app context available".to_string(),
        Some(ctx) => {
            match rusqlite::Connection::open(&ctx.space_db_path) {
                Err(e) => format!("db error: {e}"),
                Ok(conn) => {
                    let session_id = ""; // need session id — pass it via AppsCtx
                    // For now, use the current session from the ToolBox
                    match crate::db::message_images_for_session(&conn, session_id) {
                        Ok(images) => {
                            let list: Vec<serde_json::Value> = images.iter().map(|img| {
                                serde_json::json!({"id": img.id, "description": img.description})
                            }).collect();
                            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
                        }
                        Err(e) => format!("query error: {e}"),
                    }
                }
            }
        }
    };
    (result, status)
}
```

Need session_id — add to `AppsCtx` or pass through `ToolBox`. The simplest: add `session_id: String` to `AppsCtx`:

```rust
pub struct AppsCtx {
    pub dir: PathBuf,
    pub server_port: u16,
    pub registry: crate::appserver::AppRegistry,
    pub space_name: String,
    pub space_db_path: PathBuf,
    pub images_dir: PathBuf,
    pub session_id: String,
}
```

- [ ] **Step 4: Add `copy_images_to_app` tool definition**

```rust
if self.apps.is_some() {
    defs.push(ToolDef {
        name: "copy_images_to_app".to_string(),
        description: "Copy one or more conversation images into an app's _images/ directory so the app can display them. image_ids come from list_images. Returns [{id, url}] with URLs the app can use in <img> tags.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "image_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "one or more image IDs from list_images"
                },
                "app": {
                    "type": "string",
                    "description": "app UUID or name to copy into"
                },
            },
            "required": ["image_ids", "app"],
        }),
    });
}
```

- [ ] **Step 5: Add `copy_images_to_app` handler**

```rust
"copy_images_to_app" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let image_ids: Vec<String> = v.get("image_ids").and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let app = v.get("app").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let status = format!("Copying {} images to {app}…", image_ids.len());
    let result = match self.apps.as_ref() {
        None => "apps not available".to_string(),
        Some(ctx) => {
            let (uuid, app_dir) = match self.resolve_app(&app) {
                Err(e) => e,
                Ok(t) => t,
            };
            let images_dir = app_dir.join("_images");
            std::fs::create_dir_all(&images_dir);
            let mut out = Vec::new();
            for img_id in &image_ids {
                let src = ctx.images_dir.join(format!("{img_id}.png"));
                // Try to find by querying the db for the path
                let src_path = match rusqlite::Connection::open(&ctx.space_db_path) {
                    Ok(conn) => {
                        conn.query_row(
                            "SELECT path FROM message_images WHERE id = ?1",
                            [img_id],
                            |r| r.get::<_, String>(0),
                        ).ok()
                    }
                    Err(_) => None,
                };
                let Some(src_path) = src_path else {
                    out.push(serde_json::json!({"id": img_id, "error": "not found in db"}));
                    continue;
                };
                let src = std::path::Path::new(&src_path);
                let filename = src.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                let dst = images_dir.join(filename);
                match std::fs::copy(src, &dst) {
                    Ok(_) => {
                        out.push(serde_json::json!({
                            "id": img_id,
                            "url": format!("/{uuid}/_images/{filename}"),
                        }));
                    }
                    Err(e) => {
                        out.push(serde_json::json!({"id": img_id, "error": format!("{e}")}));
                    }
                }
            }
            serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
        }
    };
    (result, status)
}
```

- [ ] **Step 6: Wire `session_id` into `AppsCtx` in `app/mod.rs`**

In `refresh_toolbox`:

```rust
self.app_server.as_ref().map(|s| crate::tools::AppsCtx {
    dir: self.space.apps_dir(&self.active_space.name),
    server_port: s.port(),
    registry: s.registry().clone(),
    space_name: self.active_space.name.clone(),
    space_db_path: self.space.db_path(),
    images_dir: self.space.images_dir(&self.active_space.name),
    session_id: self.session.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
})
```

- [ ] **Step 7: Run tests**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs src/db.rs src/app/mod.rs
git commit -m "feat: add list_images and copy_images_to_app tools"
```

---

### Task 8: Tools — `copy_file_to_app`

**Files:**
- Modify: `src/tools.rs`

- [ ] **Step 1: Add `copy_file_to_app` tool definition**

```rust
if self.files.is_some() && self.apps.is_some() {
    defs.push(ToolDef {
        name: "copy_file_to_app".to_string(),
        description: "Copy an imported space file's text content into an app's KV store, accessible at /_api/kv/_file:<name>. The app's frontend reads it by GET /<app_uuid>/_api/kv/_file:<name>.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "file_name": { "type": "string", "description": "the file name as shown in the Files section" },
                "app": { "type": "string", "description": "app UUID or name" },
            },
            "required": ["file_name", "app"],
        }),
    });
}
```

- [ ] **Step 2: Add `copy_file_to_app` handler**

```rust
"copy_file_to_app" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let file_name = v.get("file_name").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let app = v.get("app").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let status = format!("Copying {file_name} to {app}…");
    let result = match (&self.apps, &self.files) {
        (None, _) => "apps not available".to_string(),
        (_, None) => "files not available".to_string(),
        (Some(ctx), Some(fc)) => {
            let (_, app_dir) = match self.resolve_app(&app) {
                Err(e) => e,
                Ok(t) => t,
            };
            // Read file text from db
            match rusqlite::Connection::open(&fc.db_path) {
                Err(e) => format!("db error: {e}"),
                Ok(conn) => match crate::db::file_text(&conn, &fc.space_id, &file_name) {
                    Ok(Some(text)) => {
                        let db_path = app_dir.join("_store.db");
                        match rusqlite::Connection::open(&db_path) {
                            Err(e) => format!("store error: {e}"),
                            Ok(store) => {
                                let _ = store.execute_batch("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)");
                                let key = format!("_file:{file_name}");
                                match store.execute("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)", rusqlite::params![key, text]) {
                                    Ok(_) => format!("copied {file_name} into {app}'s KV — read it at /_api/kv/_file:{file_name}"),
                                    Err(e) => format!("kv write error: {e}"),
                                }
                            }
                        }
                    }
                    Ok(None) => format!("unknown file: {file_name}"),
                    Err(e) => format!("file read error: {e}"),
                },
            }
        }
    };
    (result, status)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 4: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add copy_file_to_app tool"
```

---

### Task 9: Update System Prompt

**Files:**
- Modify: `src/app/chat.rs`

- [ ] **Step 1: Update `apps_section()`** to document the new API

```rust
fn apps_section(&self) -> Option<String> {
    self.app_server.as_ref()?;
    let mut s = "## Apps\nYou can build apps with persistent storage (KV store), file upload, and access to user-uploaded images. Apps are served at UUID-based URLs.\n\n".to_string();

    s.push_str("### Tools\n");
    s.push_str("- `write_file(app, path, content)` — create/edit a file. First write to a new app name auto-generates a UUID. You can also use `app_uuid` instead of `app` name after creation.\n");
    s.push_str("- `read_app_file(app, path)` / `edit_file(app, path, edits)` — read and edit by hashline.\n");
    s.push_str("- `grep_app(app, pattern)` — search all files in an app.\n");
    s.push_str("- `install_packages(app=..., packages=[...])` — npm-install into an app.\n\n");

    s.push_str("### KV Store (per-app persistent key-value storage)\n");
    s.push_str("Each app has its own SQLite-backed KV store. Call these endpoints from frontend JavaScript:\n");
    s.push_str("- `PUT <app_url>/_api/kv/<key>` — upsert a value (body = raw text)\n");
    s.push_str("- `GET <app_url>/_api/kv/<key>` — read a value\n");
    s.push_str("- `DELETE <app_url>/_api/kv/<key>` — delete a value\n");
    s.push_str("- `GET <app_url>/_api/kv` — list all keys (returns JSON array)\n\n");

    s.push_str("### File Upload\n");
    s.push_str("- `POST <app_url>/_api/upload` with `multipart/form-data` — upload a file. Returns `{\"name\": \"...\", \"url\": \"/<uuid>/_uploads/...\"}`. Uploaded files persist and are served via GET.\n\n");

    s.push_str("### Using User Images\n");
    s.push_str("1. Call `list_images` to see pasted conversation images.\n");
    s.push_str("2. Call `copy_images_to_app(image_ids, app)` to copy images into the app's `_images/` directory.\n");
    s.push_str("3. Use the returned URLs in `<img src=\"...\">` tags.\n\n");

    s.push_str("### Using Space Files\n");
    s.push_str("- Call `copy_file_to_app(file_name, app)` to copy a file's text into the app's KV under key `_file:<name>`. Read it from the frontend as `GET <app_url>/_api/kv/_file:<name>`.\n\n");

    let apps = self.list_apps();
    if apps.is_empty() {
        s.push_str("No apps exist in this space yet.");
    } else {
        s.push_str("Existing apps:\n");
        for a in &apps {
            if let Some(uuid) = self.app_server.as_ref().and_then(|s| s.registry().resolve(&self.active_space.name, a)) {
                s.push_str(&format!("- {a} (uuid {uuid})\n"));
            } else {
                s.push_str(&format!("- {a}\n"));
            }
        }
    }
    Some(s.trim_end().to_string())
}
```

- [ ] **Step 2: Run tests**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 3: Commit**

```bash
git add src/app/chat.rs
git commit -m "feat: update system prompt with KV store, upload, and image/file bridge docs"
```

---

### Task 10: Migration — Startup Orphan Scan

**Files:**
- Modify: `src/appserver.rs`

- [ ] **Step 1: On startup, scan `spaces/*/apps/*/` and assign UUIDs to unregistered apps**

In `AppRegistry::load()`, after loading the registry JSON, scan for orphan apps:

```rust
pub fn load(spaces_root: &std::path::Path) -> Self {
    let path = spaces_root.join("_apps.json");
    let mut map: HashMap<String, AppEntry> = match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => HashMap::new(),
    };
    // Scan for orphan apps
    if let Ok(rd) = std::fs::read_dir(spaces_root) {
        for entry in rd.filter_map(|e| e.ok()) {
            let space_path = entry.path();
            if !space_path.is_dir() { continue; }
            let space_name = match space_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let apps_dir = space_path.join("apps");
            if !apps_dir.is_dir() { continue; }
            if let Ok(ad) = std::fs::read_dir(&apps_dir) {
                for app_entry in ad.filter_map(|e| e.ok()) {
                    if !app_entry.path().is_dir() { continue; }
                    let app_name = match app_entry.file_name().into_string() {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let already_registered = map.values().any(|e| e.space == space_name && e.name == app_name);
                    if !already_registered {
                        let uuid = uuid::Uuid::new_v4().to_string();
                        map.insert(uuid, AppEntry { space: space_name.clone(), name: app_name });
                    }
                }
            }
        }
    }
    // Save if we added anything
    let registry = AppRegistry { inner: Arc::new(RwLock::new(map)), path: path.clone() };
    let _ = registry.save();
    registry
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p nexus-chat -- appserver::tests --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/appserver.rs
git commit -m "feat: scan for orphan apps on startup and assign UUIDs"
```

---

## Self-Review

**Spec coverage:**
- [x] UUID-based URLs → Task 1 (resolve function), Task 2 (CORS/API infra)
- [x] App registry → Task 1
- [x] KV store (SQLite per app) → Task 3
- [x] File upload endpoint (multipart) → Task 4
- [x] User image access in apps (list_images + copy_images_to_app) → Task 7
- [x] Space file access in apps (copy_file_to_app) → Task 8
- [x] Tool UUID return → Task 6
- [x] System prompt update → Task 9
- [x] Migration (orphan scan) → Task 10
- [x] CORS and method dispatch → Task 2
- [x] `_uploads/` serving → normal UUID resolve covers it

**Placeholder check:** All code blocks contain complete implementation.

**Type consistency:** `AppRegistry` used consistently across appserver, tools, and app module. `resolve_app()` returns `(String, PathBuf)` consistently across all tool handlers.
