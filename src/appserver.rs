//! Tiny always-on static file server for model-created apps. Serves
//! `GET /<space>/<app>/<path…>` from `spaces/<space>/apps/<app>/<path…>`,
//! localhost only, GET/HEAD only. Hand-rolled on tokio — no framework dep
//! for ~150 lines of static serving.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Preferred fixed port so links stay stable across restarts.
const PORT: u16 = 8642;

#[derive(Clone)]
pub struct AppServer {
    port: u16,
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
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let root = spaces_root.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, &root).await;
                });
            }
        });
        Some(AppServer { port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// URL prefix for a space's apps: `http://127.0.0.1:port/<space>/`.
    pub fn space_url(&self, space: &str) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port(), encode(space))
    }
}

async fn handle(mut stream: tokio::net::TcpStream, spaces_root: &Path) -> std::io::Result<()> {
    // Read until end of headers; requests are tiny GETs, cap at 8 KiB.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let request_line = std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.lines().next())
        .unwrap_or("")
        .to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let head = method == "HEAD";
    if method != "GET" && !head {
        return respond(&mut stream, 405, "text/plain", b"method not allowed", head).await;
    }
    match resolve(spaces_root, raw_path) {
        Some(file) => {
            let mime = mime_for(&file);
            match tokio::fs::read(&file).await {
                Ok(body) => respond(&mut stream, 200, mime, &body, head).await,
                Err(_) => respond(&mut stream, 404, "text/plain", b"not found", head).await,
            }
        }
        None => respond(&mut stream, 404, "text/plain", b"not found", head).await,
    }
}

/// Map a request path `/<space>/<app>/<rest…>` to a file under
/// `spaces_root/<space>/apps/<app>/<rest…>`, or None if malformed/escaping.
fn resolve(spaces_root: &Path, raw_path: &str) -> Option<PathBuf> {
    let path = raw_path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path);
    let mut segs = decoded.split('/').filter(|s| !s.is_empty());
    let space = segs.next()?;
    let app = segs.next()?;
    let rest: Vec<&str> = segs.collect();
    for seg in [space, app].iter().chain(rest.iter()) {
        if *seg == ".." || *seg == "." || seg.contains('\\') {
            return None;
        }
    }
    let mut file = spaces_root.join(space).join("apps").join(app);
    for seg in &rest {
        file.push(seg);
    }
    if file.is_dir() || rest.is_empty() {
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
        405 => "Method Not Allowed",
        _ => "Not Found",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(header.as_bytes()).await?;
    if !head {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
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
        let f = resolve(root, "/default/deck/style.css").unwrap();
        assert!(f.ends_with("default/apps/deck/style.css"));
        // dir root → index.html
        let f = resolve(root, "/default/deck/").unwrap();
        assert!(f.ends_with("deck/index.html"));
        let f = resolve(root, "/default/deck").unwrap();
        assert!(f.ends_with("deck/index.html"));
        // traversal + malformed rejected
        assert!(resolve(root, "/default/deck/../../secret").is_none());
        assert!(resolve(root, "/default/%2e%2e/apps/deck/index.html").is_none());
        assert!(resolve(root, "/default").is_none());
        assert!(resolve(root, "/default/deck/missing.js").is_none());
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
    fn space_url_encodes_segment() {
        let s = AppServer { port: 9999 };
        assert_eq!(s.space_url("my space"), "http://127.0.0.1:9999/my%20space/");
    }
}
