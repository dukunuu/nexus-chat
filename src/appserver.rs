//! Tiny always-on static file server for model-created apps. Serves
//! `GET /<space>/<app>/<path…>` from `spaces/<space>/apps/<app>/<path…>`,
//! localhost only, GET/HEAD only. Hand-rolled on tokio — no framework dep
//! for ~150 lines of static serving.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Preferred fixed port so links stay stable across restarts.
const PORT: u16 = 8642;

// ---------------------------------------------------------------------------
// App registry — maps UUID ↔ (space, name), persisted to spaces_root/_apps.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    pub space: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AppRegistry {
    inner: Arc<RwLock<HashMap<String, AppEntry>>>,
    path: std::path::PathBuf,
}

impl AppRegistry {
    pub fn load(spaces_root: &Path) -> Self {
        let path = spaces_root.join("_apps.json");
        let map = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        AppRegistry { inner: Arc::new(RwLock::new(map)), path }
    }

    pub fn assign(&self, space: &str, name: &str) -> String {
        let uuid = uuid::Uuid::new_v4().to_string();
        {
            let mut map = self.inner.write().unwrap();
            map.insert(uuid.clone(), AppEntry { space: space.to_string(), name: name.to_string() });
        }
        let _ = self.save();
        uuid
    }

    pub fn lookup(&self, uuid: &str) -> Option<AppEntry> {
        self.inner.read().unwrap().get(uuid).cloned()
    }

    pub fn resolve(&self, space: &str, name: &str) -> Option<String> {
        self.inner.read().unwrap().iter()
            .find(|(_, e)| e.space == space && e.name == name)
            .map(|(u, _)| u.clone())
    }

    fn save(&self) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(&*self.inner.read().unwrap())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// App server
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppServer {
    port: u16,
    registry: AppRegistry,
    spaces_root: PathBuf,
}

impl AppServer {
    /// Bind (preferring 8642, falling back to an ephemeral port) and start
    /// serving `spaces_root` in a background task. None if even `:0` fails.
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
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let root = spaces_root.clone();
                let reg = registry.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, &root, &reg).await;
                });
            }
        });
        Some(srv)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn app_url(&self, uuid: &str) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port(), uuid)
    }

    pub fn registry(&self) -> &AppRegistry {
        &self.registry
    }

    pub fn spaces_root(&self) -> &Path {
        &self.spaces_root
    }
}

async fn handle(
    mut stream: tokio::net::TcpStream,
    spaces_root: &Path,
    registry: &AppRegistry,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let header_end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => pos,
        None => return respond(&mut stream, 400, "text/plain", b"bad request", false).await,
    };
    let header_str = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(_) => return respond(&mut stream, 400, "text/plain", b"bad request", false).await,
    };

    let request_line = header_str.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let head = method == "HEAD";

    if method == "OPTIONS" {
        return respond(&mut stream, 204, "text/plain", b"", false).await;
    }

    let content_length = parse_header_value(header_str, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > 10 * 1024 * 1024 {
        return respond(&mut stream, 413, "text/plain", b"request entity too large", false).await;
    }

    let content_type = parse_header_value(header_str, "content-type").unwrap_or("").to_string();

    let body: Vec<u8> = if content_length > 0 {
        let body_start = header_end + 4;
        let in_buf = &buf[body_start..];
        let already = in_buf.len().min(content_length);
        let mut body = in_buf[..already].to_vec();
        if already < content_length {
            body.resize(content_length, 0);
            stream.read_exact(&mut body[already..]).await?;
        }
        body
    } else {
        Vec::new()
    };

    if raw_path.contains("/_api/") {
        return handle_api(&mut stream, spaces_root, registry, method, raw_path, &body, &content_type).await;
    }

    if method != "GET" && !head {
        return respond(&mut stream, 405, "text/plain", b"method not allowed", head).await;
    }

    match resolve(spaces_root, registry, raw_path) {
        Some(file) => {
            let mime = mime_for(&file);
            match tokio::fs::read(&file).await {
                Ok(b) => respond(&mut stream, 200, mime, &b, head).await,
                Err(_) => respond(&mut stream, 404, "text/plain", b"not found", head).await,
            }
        }
        None => respond(&mut stream, 404, "text/plain", b"not found", head).await,
    }
}

/// Map a request path to a file under `spaces_root`. The first path segment
/// is either a UUID (looked up in the registry) or a space name (with the
/// second segment as the app name).
fn resolve(spaces_root: &Path, registry: &AppRegistry, raw_path: &str) -> Option<PathBuf> {
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let segs: Vec<&str> = decoded.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }

    for seg in &segs {
        if *seg == ".." || *seg == "." || seg.contains('\\') {
            return None;
        }
    }

    // API route: /<uuid>/_api/...
    if segs.len() >= 2 && segs[1] == "_api" {
        let entry = registry.lookup(segs[0])?;
        let app_dir = spaces_root.join(&entry.space).join("apps").join(&entry.name);
        return app_dir.is_dir().then_some(app_dir);
    }

    let (space, app, path_start) = match registry.lookup(segs[0]) {
        Some(entry) => (entry.space, entry.name, 1usize),
        None => {
            if segs.len() < 2 {
                return None;
            }
            (segs[0].to_string(), segs[1].to_string(), 2)
        }
    };
    let mut file = spaces_root.join(&space).join("apps").join(&app);
    for seg in &segs[path_start..] {
        file.push(seg);
    }
    if file.is_dir() || path_start >= segs.len() {
        file.push("index.html");
    }
    // Backstop against traversal tricks the segment filter missed: the
    // canonical path must stay under the canonical spaces root.
    let canon = file.canonicalize().ok()?;
    let root = spaces_root.canonicalize().ok()?;
    canon.starts_with(&root).then_some(canon)
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    head: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Request Entity Too Large",
        501 => "Not Implemented",
        _ => "Not Found",
    };
    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len(),
    );
    if mime.starts_with("application/json") || status != 200 {
        header.push_str("Access-Control-Allow-Origin: *\r\n");
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;
    if !head && !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}

fn parse_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    for line in headers.lines().skip(1) {
        if let Some(pos) = line.find(':') {
            let key = line[..pos].trim();
            if key.eq_ignore_ascii_case(name) {
                return Some(line[pos + 1..].trim());
            }
        }
    }
    None
}

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
    if segs.len() < 3 || segs[1] != "_api" {
        return respond(stream, 404, "text/plain", b"not found", false).await;
    }
    let Some(entry) = registry.lookup(segs[0]) else {
        return respond(stream, 404, "text/plain", b"unknown app", false).await;
    };
    let app_dir = spaces_root.join(&entry.space).join("apps").join(&entry.name);
    if !app_dir.is_dir() {
        return respond(stream, 404, "text/plain", b"app not found on disk", false).await;
    }

    match segs.get(2).copied() {
        Some("kv") => handle_kv(stream, &app_dir, method, &segs[3..], body).await,
        Some("upload") if method == "POST" => handle_upload(stream, &app_dir, &segs[0], body, content_type).await,
        _ => respond(stream, 404, "text/plain", b"unknown api endpoint", false).await,
    }
}

async fn handle_kv(
    stream: &mut tokio::net::TcpStream,
    app_dir: &Path,
    method: &str,
    segs: &[&str],
    body: &[u8],
) -> std::io::Result<()> {
    let db_path = app_dir.join("_store.db");
    let (status, mime, body_bytes) = kv_op(&db_path, method, segs, body);
    respond(stream, status, mime, &body_bytes, false).await
}

fn kv_op(db_path: &Path, method: &str, segs: &[&str], body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => {
            let _ = c.execute_batch("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)");
            c
        }
        Err(e) => return (500, "text/plain", format!("db error: {e}").into_bytes()),
    };

    match method {
        "GET" if segs.is_empty() => {
            let keys: Vec<String> = match conn.prepare("SELECT key FROM kv ORDER BY key") {
                Ok(mut stmt) => match stmt.query_map([], |r| r.get::<_, String>(0)) {
                    Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                    Err(e) => return (500, "text/plain", format!("query error: {e}").into_bytes()),
                },
                Err(e) => return (500, "text/plain", format!("query error: {e}").into_bytes()),
            };
            let json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string());
            (200, "application/json", json.into_bytes())
        }
        "GET" => {
            let key = percent_decode(segs.join("/").as_str());
            match conn.query_row("SELECT value FROM kv WHERE key = ?1", [&key], |r| r.get::<_, String>(0)) {
                Ok(value) => (200, "text/plain", value.into_bytes()),
                Err(rusqlite::Error::QueryReturnedNoRows) => (404, "text/plain", b"not found".to_vec()),
                Err(e) => (500, "text/plain", format!("read error: {e}").into_bytes()),
            }
        }
        "PUT" => {
            let key = percent_decode(segs.join("/").as_str());
            let value = std::str::from_utf8(body).unwrap_or("");
            match conn.execute("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)", rusqlite::params![key, value]) {
                Ok(_) => (200, "text/plain", b"ok".to_vec()),
                Err(e) => (500, "text/plain", format!("write error: {e}").into_bytes()),
            }
        }
        "DELETE" => {
            let key = percent_decode(segs.join("/").as_str());
            match conn.execute("DELETE FROM kv WHERE key = ?1", [key]) {
                Ok(0) => (404, "text/plain", b"not found".to_vec()),
                Ok(_) => (200, "text/plain", b"deleted".to_vec()),
                Err(e) => (500, "text/plain", format!("delete error: {e}").into_bytes()),
            }
        }
        _ => (405, "text/plain", b"method not allowed".to_vec()),
    }
}

async fn handle_upload(
    stream: &mut tokio::net::TcpStream,
    _app_dir: &Path,
    _uuid: &str,
    _body: &[u8],
    _content_type: &str,
) -> std::io::Result<()> {
    respond(stream, 501, "text/plain", b"upload not implemented", false).await
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Decode %XX escapes (space names may contain spaces). Invalid escapes pass
/// through literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode a path segment for a URL (space names may contain spaces).
pub(crate) fn encode(seg: &str) -> String {
    seg.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("nexus-appserver-{}", uuid::Uuid::new_v4()));
        let app = tmp.join("default").join("apps").join("deck");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("index.html"), "<h1>slides</h1>").unwrap();
        std::fs::write(app.join("style.css"), "h1{color:red}").unwrap();
        tmp
    }

    #[test]
    fn resolve_maps_and_guards() {
        let root = &setup();
        let reg = AppRegistry::load(root);
        let f = resolve(root, &reg, "/default/deck/style.css").unwrap();
        assert!(f.ends_with("default/apps/deck/style.css"));
        // dir root → index.html
        let f = resolve(root, &reg, "/default/deck/").unwrap();
        assert!(f.ends_with("deck/index.html"));
        let f = resolve(root, &reg, "/default/deck").unwrap();
        assert!(f.ends_with("deck/index.html"));
        // traversal + malformed rejected
        assert!(resolve(root, &reg, "/default/deck/../../secret").is_none());
        assert!(resolve(root, &reg, "/default/%2e%2e/apps/deck/index.html").is_none());
        assert!(resolve(root, &reg, "/default").is_none());
        assert!(resolve(root, &reg, "/default/deck/missing.js").is_none());
    }

    #[test]
    fn resolve_uuid() {
        let root = &setup();
        let reg = AppRegistry::load(root);
        let uuid = reg.assign("default", "deck");

        let f = resolve(root, &reg, &format!("/{uuid}/style.css")).unwrap();
        assert!(f.ends_with("default/apps/deck/style.css"));

        let f = resolve(root, &reg, &format!("/{uuid}/")).unwrap();
        assert!(f.ends_with("deck/index.html"));

        let f = resolve(root, &reg, &format!("/{uuid}")).unwrap();
        assert!(f.ends_with("deck/index.html"));
    }

    #[test]
    fn resolve_api() {
        let root = &setup();
        let reg = AppRegistry::load(root);
        let uuid = reg.assign("default", "deck");

        // API route returns the app directory path
        let f = resolve(root, &reg, &format!("/{uuid}/_api/"));
        assert!(f.is_some());
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode("my%20space/deck"), "my space/deck");
        assert_eq!(encode("my space"), "my%20space");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[tokio::test]
    async fn serves_files_with_mime_404_and_no_store() {
        let srv = AppServer::start(setup()).await.unwrap();
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c.get(format!("{base}/default/deck/")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.text().await.unwrap(), "<h1>slides</h1>");

        let r = c
            .get(format!("{base}/default/deck/style.css"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["content-type"], "text/css");

        let r = c
            .get(format!("{base}/default/deck/nope.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);

        let r = c
            .post(format!("{base}/default/deck/"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 405);
    }

    #[test]
    fn app_url_format() {
        let reg = AppRegistry::load(&PathBuf::from("/tmp"));
        let s = AppServer { port: 9999, registry: reg, spaces_root: PathBuf::from("/tmp") };
        assert_eq!(s.app_url("some-uuid"), "http://127.0.0.1:9999/some-uuid/");
    }

    #[tokio::test]
    async fn options_request_returns_204_with_cors() {
        let srv = AppServer::start(setup()).await.unwrap();
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c.request(reqwest::Method::OPTIONS, format!("{base}/default/deck/"))
            .send().await.unwrap();
        assert_eq!(r.status(), 204);
        assert_eq!(r.headers()["access-control-allow-origin"], "*");
    }

    #[tokio::test]
    async fn put_outside_api_is_405() {
        let srv = AppServer::start(setup()).await.unwrap();
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c.put(format!("{base}/default/deck/index.html"))
            .body("new content")
            .send().await.unwrap();
        assert_eq!(r.status(), 405);
    }

    #[tokio::test]
    async fn api_methods_routed_to_handle_api() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c.post(format!("{base}/{uuid}/_api/upload"))
            .send().await.unwrap();
        assert_eq!(r.status(), 501);
    }

    #[tokio::test]
    async fn kv_roundtrip() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

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

    #[tokio::test]
    async fn kv_list_keys() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        c.put(format!("{base}/{uuid}/_api/kv/a")).body("1").send().await.unwrap();
        c.put(format!("{base}/{uuid}/_api/kv/b")).body("2").send().await.unwrap();
        c.put(format!("{base}/{uuid}/_api/kv/c")).body("3").send().await.unwrap();

        let r = c.get(format!("{base}/{uuid}/_api/kv")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let keys: Vec<String> = r.json().await.unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }
}
