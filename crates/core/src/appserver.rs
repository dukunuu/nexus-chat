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
    /// Subdirectory the app's files are served from (e.g. "dist" after a
    /// framework build). None = served from the app root (classic apps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_from: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppRegistry {
    inner: Arc<RwLock<HashMap<String, AppEntry>>>,
    path: std::path::PathBuf,
}

impl AppRegistry {
    pub fn load(spaces_root: &Path) -> Self {
        let path = spaces_root.join("_apps.json");
        let mut map: HashMap<String, AppEntry> = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        // Scan for orphan apps (app dirs not yet in the registry) and assign
        // UUIDs so old space-name URLs keep working.
        if let Ok(rd) = std::fs::read_dir(spaces_root) {
            for entry in rd.filter_map(std::result::Result::ok) {
                let space_path = entry.path();
                if !space_path.is_dir() {
                    continue;
                }
                let space_name = match space_path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let apps_dir = space_path.join("apps");
                if !apps_dir.is_dir() {
                    continue;
                }
                if let Ok(ad) = std::fs::read_dir(&apps_dir) {
                    for app_entry in ad.filter_map(std::result::Result::ok) {
                        if !app_entry.path().is_dir() {
                            continue;
                        }
                        let Ok(app_name) = app_entry.file_name().into_string() else {
                            continue;
                        };
                        let already = map
                            .values()
                            .any(|e| e.space == space_name && e.name == app_name);
                        if !already {
                            let uuid = uuid::Uuid::new_v4().to_string();
                            map.insert(
                                uuid,
                                AppEntry {
                                    space: space_name.clone(),
                                    name: app_name,
                                    served_from: None,
                                },
                            );
                        }
                    }
                }
            }
        }
        let registry = Self {
            inner: Arc::new(RwLock::new(map)),
            path,
        };
        let _ = registry.save();
        registry
    }

    pub fn assign(&self, space: &str, name: &str) -> String {
        let uuid = uuid::Uuid::new_v4().to_string();
        {
            let mut map = self.inner.write().unwrap();
            map.insert(
                uuid.clone(),
                AppEntry {
                    space: space.to_string(),
                    name: name.to_string(),
                    served_from: None,
                },
            );
        }
        let _ = self.save();
        uuid
    }

    pub fn lookup(&self, uuid: &str) -> Option<AppEntry> {
        self.inner.read().unwrap().get(uuid).cloned()
    }

    pub fn resolve(&self, space: &str, name: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap()
            .iter()
            .find(|(_, e)| e.space == space && e.name == name)
            .map(|(u, _)| u.clone())
    }

    /// Record that an app's served files live under `subdir` (e.g. "dist")
    /// after a successful framework build.
    pub fn set_served_from(&self, uuid: &str, subdir: &str) {
        {
            let mut map = self.inner.write().unwrap();
            if let Some(entry) = map.get_mut(uuid) {
                entry.served_from = Some(subdir.to_string());
            }
        }
        let _ = self.save();
    }

    pub fn rename_space(&self, old: &str, new: &str) {
        let mut map = self.inner.write().unwrap();
        for entry in map.values_mut() {
            if entry.space == old {
                entry.space = new.to_string();
            }
        }
        drop(map);
        let _ = self.save();
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
}

impl AppServer {
    /// Bind (preferring 8642, falling back to an ephemeral port) and start
    /// serving `spaces_root` in a background task. None if even `:0` fails.
    pub async fn start(spaces_root: PathBuf) -> Option<Self> {
        let listener = match TcpListener::bind(("127.0.0.1", PORT)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind(("127.0.0.1", 0)).await.ok()?,
        };
        let port = listener.local_addr().ok()?.port();
        let registry = AppRegistry::load(&spaces_root);
        let srv = Self {
            port,
            registry: registry.clone(),
        };
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

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn app_url(&self, uuid: &str) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port(), uuid)
    }

    pub const fn registry(&self) -> &AppRegistry {
        &self.registry
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

    let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return respond(&mut stream, 400, "text/plain", b"bad request", false).await;
    };
    let Ok(header_str) = std::str::from_utf8(&buf[..header_end]) else {
        return respond(&mut stream, 400, "text/plain", b"bad request", false).await;
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
        return respond(
            &mut stream,
            413,
            "text/plain",
            b"request entity too large",
            false,
        )
        .await;
    }

    let content_type = parse_header_value(header_str, "content-type")
        .unwrap_or("")
        .to_string();

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
        return handle_api(
            &mut stream,
            spaces_root,
            registry,
            method,
            raw_path,
            &body,
            &content_type,
        )
        .await;
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
        let app_dir = spaces_root
            .join(&entry.space)
            .join("apps")
            .join(&entry.name);
        return app_dir.is_dir().then_some(app_dir);
    }

    let (space, app, path_start, served_from) = if let Some(entry) = registry.lookup(segs[0]) {
        (entry.space, entry.name, 1usize, entry.served_from)
    } else {
        if segs.len() < 2 {
            return None;
        }
        let served_from = registry
            .resolve(segs[0], segs[1])
            .and_then(|uuid| registry.lookup(&uuid))
            .and_then(|entry| entry.served_from);
        (
            segs[0].to_string(),
            segs[1].to_string(),
            2usize,
            served_from,
        )
    };
    let mut file = spaces_root.join(&space).join("apps").join(&app);
    if let Some(sub) = served_from {
        // User data written to the app root by copy_images (_images/) and
        // the upload API (_uploads/) stays there even when the app is
        // served from dist/ after a framework build.
        let root_data = segs
            .get(path_start)
            .is_some_and(|s| matches!(*s, "_images" | "_uploads"));
        if !root_data {
            file.push(sub);
        }
    }
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
    let app_dir = spaces_root
        .join(&entry.space)
        .join("apps")
        .join(&entry.name);
    if !app_dir.is_dir() {
        return respond(stream, 404, "text/plain", b"app not found on disk", false).await;
    }

    match segs.get(2).copied() {
        Some("kv") => handle_kv(stream, &app_dir, method, &segs[3..], body).await,
        Some("upload") if method == "POST" => {
            handle_upload(stream, &app_dir, segs[0], body, content_type).await
        }
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
            let _ =
                c.execute_batch("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)");
            c
        }
        Err(e) => return (500, "text/plain", format!("db error: {e}").into_bytes()),
    };

    match method {
        "GET" if segs.is_empty() => {
            let keys: Vec<String> = match conn.prepare("SELECT key FROM kv ORDER BY key") {
                Ok(mut stmt) => match stmt.query_map([], |r| r.get::<_, String>(0)) {
                    Ok(rows) => rows.filter_map(std::result::Result::ok).collect(),
                    Err(e) => return (500, "text/plain", format!("query error: {e}").into_bytes()),
                },
                Err(e) => return (500, "text/plain", format!("query error: {e}").into_bytes()),
            };
            let json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string());
            (200, "application/json", json.into_bytes())
        }
        "GET" => {
            let key = percent_decode(segs.join("/").as_str());
            match conn.query_row("SELECT value FROM kv WHERE key = ?1", [&key], |r| {
                r.get::<_, String>(0)
            }) {
                Ok(value) => (200, "text/plain", value.into_bytes()),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    (404, "text/plain", b"not found".to_vec())
                }
                Err(e) => (500, "text/plain", format!("read error: {e}").into_bytes()),
            }
        }
        "PUT" => {
            let key = percent_decode(segs.join("/").as_str());
            let value = std::str::from_utf8(body).unwrap_or("");
            match conn.execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value],
            ) {
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
    app_dir: &Path,
    app_uuid: &str,
    body: &[u8],
    content_type: &str,
) -> std::io::Result<()> {
    let boundary = match content_type
        .split(';')
        .find_map(|p| p.trim().strip_prefix("boundary="))
    {
        Some(b) => b.trim_matches('"').to_string(),
        None => {
            return respond(
                stream,
                400,
                "text/plain",
                b"missing boundary in Content-Type",
                false,
            )
            .await;
        }
    };
    if boundary.is_empty() {
        return respond(stream, 400, "text/plain", b"empty boundary", false).await;
    }

    let Ok(body_str) = std::str::from_utf8(body) else {
        return respond(
            stream,
            400,
            "text/plain",
            b"upload body is not valid UTF-8",
            false,
        )
        .await;
    };

    let part_header = format!("--{boundary}\r\n");
    let part_end = format!("\r\n--{boundary}");

    let Some(header_start) = body_str.find(&part_header) else {
        return respond(stream, 400, "text/plain", b"no multipart part found", false).await;
    };
    let after_header_marker = header_start + part_header.len();

    let Some(hdr_body_sep) = body_str[after_header_marker..].find("\r\n\r\n") else {
        return respond(
            stream,
            400,
            "text/plain",
            b"malformed multipart part: no header-body separator",
            false,
        )
        .await;
    };
    let hdr_end = after_header_marker + hdr_body_sep;
    let part_headers = &body_str[after_header_marker..hdr_end];

    let filename = part_headers
        .split(';')
        .find_map(|p| p.trim().strip_prefix("filename="))
        .map_or_else(
            || "upload.bin".to_string(),
            |f| f.trim_matches('"').to_string(),
        );

    let content_start = hdr_end + 4;
    let content_end = body_str[content_start..]
        .find(&part_end)
        .map_or(body_str.len(), |d| content_start + d);

    let mut file_body = &body[content_start..content_end];
    if file_body.ends_with(b"\r\n") {
        file_body = &file_body[..file_body.len() - 2];
    }

    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_string());

    let uploads_dir = app_dir.join("_uploads");
    if let Err(e) = std::fs::create_dir_all(&uploads_dir) {
        return respond(
            stream,
            500,
            "text/plain",
            format!("cannot create uploads dir: {e}").as_bytes(),
            false,
        )
        .await;
    }

    let file_id = uuid::Uuid::new_v4().to_string();
    let save_path = uploads_dir.join(format!("{file_id}.{ext}"));
    if let Err(e) = std::fs::write(&save_path, file_body) {
        return respond(
            stream,
            500,
            "text/plain",
            format!("cannot save upload: {e}").as_bytes(),
            false,
        )
        .await;
    }

    let url = format!("/{app_uuid}/_uploads/{file_id}.{ext}");
    let json = serde_json::json!({"name": filename, "url": url});
    let body_out = serde_json::to_string(&json).unwrap_or_default();
    respond(stream, 200, "application/json", body_out.as_bytes(), false).await
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
#[cfg(test)]
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
    fn resolve_uses_served_from_after_build() {
        let root = &setup();
        // Simulate a built app: the real files live in dist/.
        let dist = root.join("default/apps/deck/dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.html"), "<h1>built</h1>").unwrap();
        let reg = AppRegistry::load(root);
        // Use the entry the load-time orphan scan created — assign() would
        // add a second UUID for the same app (test-only artifact).
        let uuid = reg.resolve("default", "deck").unwrap();
        reg.set_served_from(&uuid, "dist");

        let f = resolve(root, &reg, &format!("/{uuid}/")).unwrap();
        assert!(f.ends_with("deck/dist/index.html"), "{f:?}");
        let f = resolve(root, &reg, "/default/deck/").unwrap();
        assert!(f.ends_with("deck/dist/index.html"), "{f:?}");
        // Source files outside dist are no longer served.
        assert!(resolve(root, &reg, "/default/deck/style.css").is_none());
        // The KV API still resolves to the app root.
        assert!(resolve(root, &reg, &format!("/{uuid}/_api/")).is_some());
        // copy_images (_images/) and upload (_uploads/) user data stay in
        // the app root and keep resolving in both URL forms.
        let images = root.join("default/apps/deck/_images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("pic.png"), "x").unwrap();
        let uploads = root.join("default/apps/deck/_uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        std::fs::write(uploads.join("f.txt"), "x").unwrap();
        assert!(resolve(root, &reg, &format!("/{uuid}/_images/pic.png")).is_some());
        assert!(resolve(root, &reg, "/default/deck/_images/pic.png").is_some());
        assert!(resolve(root, &reg, &format!("/{uuid}/_uploads/f.txt")).is_some());
    }

    #[test]
    fn served_from_persists_across_registry_reloads() {
        let root = &setup();
        let reg = AppRegistry::load(root);
        let uuid = reg.assign("default", "deck");
        reg.set_served_from(&uuid, "dist");
        let reloaded = AppRegistry::load(root);
        assert_eq!(
            reloaded.lookup(&uuid).unwrap().served_from.as_deref(),
            Some("dist")
        );
        // Classic apps round-trip with served_from unset.
        let classic = reg.assign("default", "plain");
        let reloaded = AppRegistry::load(root);
        assert_eq!(reloaded.lookup(&classic).unwrap().served_from, None);
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
        let s = AppServer {
            port: 9999,
            registry: reg,
        };
        assert_eq!(s.app_url("some-uuid"), "http://127.0.0.1:9999/some-uuid/");
    }

    #[tokio::test]
    async fn options_request_returns_204_with_cors() {
        let srv = AppServer::start(setup()).await.unwrap();
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c
            .request(reqwest::Method::OPTIONS, format!("{base}/default/deck/"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
        assert_eq!(r.headers()["access-control-allow-origin"], "*");
    }

    #[tokio::test]
    async fn put_outside_api_is_405() {
        let srv = AppServer::start(setup()).await.unwrap();
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c
            .put(format!("{base}/default/deck/index.html"))
            .body("new content")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 405);
    }

    #[tokio::test]
    async fn api_methods_routed_to_handle_api() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        let r = c
            .post(format!("{base}/{uuid}/_api/upload"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
    }

    #[tokio::test]
    async fn kv_roundtrip() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        // PUT
        let r = c
            .put(format!("{base}/{uuid}/_api/kv/hello"))
            .body("world")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // GET
        let r = c
            .get(format!("{base}/{uuid}/_api/kv/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.text().await.unwrap(), "world");

        // DELETE
        let r = c
            .delete(format!("{base}/{uuid}/_api/kv/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // GET after delete = 404
        let r = c
            .get(format!("{base}/{uuid}/_api/kv/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    }

    #[tokio::test]
    async fn kv_list_keys() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let base = format!("http://127.0.0.1:{}", srv.port());
        let c = reqwest::Client::new();

        c.put(format!("{base}/{uuid}/_api/kv/a"))
            .body("1")
            .send()
            .await
            .unwrap();
        c.put(format!("{base}/{uuid}/_api/kv/b"))
            .body("2")
            .send()
            .await
            .unwrap();
        c.put(format!("{base}/{uuid}/_api/kv/c"))
            .body("3")
            .send()
            .await
            .unwrap();

        let r = c
            .get(format!("{base}/{uuid}/_api/kv"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let keys: Vec<String> = r.json().await.unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn file_upload_and_serve() {
        let srv = AppServer::start(setup()).await.unwrap();
        let uuid = srv.registry().assign("default", "deck");
        let c = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", srv.port());

        let boundary = "----TestBoundary123";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\n\r\nHello World!\r\n--{boundary}--\r\n"
        );
        let r = c
            .post(format!("{base}/{uuid}/_api/upload"))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let resp: serde_json::Value = r.json().await.unwrap();
        let url = resp["url"].as_str().unwrap().to_string();
        assert!(url.starts_with(&format!("/{uuid}/_uploads/")), "url: {url}");
        assert!(
            std::path::Path::new(&url)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt")),
            "url: {url}"
        );

        let r = c.get(format!("{base}{url}")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.text().await.unwrap(), "Hello World!");
    }
}
