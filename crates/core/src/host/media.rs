//! Space-scoped media and script operations for thin host clients.
//!
//! The HTTP router owns authentication and parsing.  This module owns the
//! filesystem policy: every name is one safe path component, every mutation
//! resolves an explicit space id, and symlinks are rejected.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::app::App;

const MAX_MEDIA_BYTES: usize = 64 * 1024 * 1024;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

/// A catalog row returned to a host client.
#[derive(Debug, Clone, Serialize)]
pub struct MediaEntry {
    pub name: String,
    pub size: u64,
    pub modified: String,
    pub mime: String,
    pub kind: String,
    pub hash: String,
}

/// Bytes and a trusted MIME selected from the filename extension.
#[derive(Debug, Clone)]
pub struct MediaBlob {
    pub bytes: Vec<u8>,
    pub mime: String,
    #[allow(dead_code)]
    pub name: String,
}

/// A validated script job for the host to execute outside the app actor.
#[derive(Debug, Clone, Serialize)]
pub struct ScriptJob {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub space_id: String,
}

#[derive(Debug, Deserialize)]
struct ActionRequest {
    action: String,
    kind: Option<String>,
    name: Option<String>,
    from: Option<String>,
    to: Option<String>,
    content: Option<String>,
    hash: Option<String>,
    args: Option<Vec<String>>,
}

fn query_value(query: &str, key: &str) -> Option<String> {
    super::api::query_param(query, key)
}

/// Dispatch the JSON media surface. The host router supplies an already
/// authenticated request and passes the raw request body here.
#[allow(clippy::too_many_lines)]
pub fn dispatch(
    app: &mut App,
    method: &str,
    query: &str,
    body: &[u8],
) -> Result<serde_json::Value> {
    let space_id = query_value(query, "space_id").ok_or_else(|| anyhow!("space_id is required"))?;
    match method {
        "GET" => catalog(app, &space_id),
        "PUT" => {
            let kind = query_value(query, "kind").unwrap_or_else(|| "file".into());
            let name = query_value(query, "name").ok_or_else(|| anyhow!("name is required"))?;
            write(app, &space_id, &kind, &name, body)?;
            Ok(serde_json::json!({ "ok": true, "name": name }))
        }
        "DELETE" => {
            let kind = query_value(query, "kind").unwrap_or_else(|| "file".into());
            let name = query_value(query, "name").ok_or_else(|| anyhow!("name is required"))?;
            Ok(serde_json::json!({ "deleted": delete(app, &space_id, &kind, &name)? }))
        }
        "POST" => {
            let action: ActionRequest =
                serde_json::from_slice(body).context("invalid media action")?;
            let kind = action.kind.as_deref().unwrap_or("file");
            match action.action.as_str() {
                "attach" => {
                    let name = action
                        .name
                        .as_deref()
                        .ok_or_else(|| anyhow!("name is required"))?;
                    let (root, path) = target(app, &space_id, kind, name)?;
                    checked_file(&path, &root)?;
                    let bytes = std::fs::read(&path).context("reading attachment")?;
                    let data_url = format!(
                        "data:{};base64,{}",
                        mime_for_name(name),
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    Ok(serde_json::json!({
                        "name": name,
                        "mime": mime_for_name(name),
                        "markdown": format!("![{name}]({name})"),
                        "data_url": data_url,
                    }))
                }
                "rename" => {
                    rename(
                        app,
                        &space_id,
                        kind,
                        action
                            .from
                            .as_deref()
                            .ok_or_else(|| anyhow!("from is required"))?,
                        action
                            .to
                            .as_deref()
                            .ok_or_else(|| anyhow!("to is required"))?,
                    )?;
                    Ok(serde_json::json!({ "ok": true }))
                }
                "read_script" => {
                    let name = action
                        .name
                        .as_deref()
                        .ok_or_else(|| anyhow!("name is required"))?;
                    let content = read_script(app, &space_id, name)?;
                    Ok(
                        serde_json::json!({ "name": name, "content": content, "hash": file_hash(&target(app, &space_id, "script", name)?.1) }),
                    )
                }
                "write_script" => {
                    write_script(
                        app,
                        &space_id,
                        action
                            .name
                            .as_deref()
                            .ok_or_else(|| anyhow!("name is required"))?,
                        action
                            .content
                            .as_deref()
                            .ok_or_else(|| anyhow!("content is required"))?,
                        action.hash.as_deref(),
                    )?;
                    Ok(serde_json::json!({ "ok": true }))
                }
                "run_script" => Ok(serde_json::to_value(prepare_script(
                    app,
                    &space_id,
                    action
                        .name
                        .as_deref()
                        .ok_or_else(|| anyhow!("name is required"))?,
                    action.args.as_deref().unwrap_or(&[]),
                )?)?),
                "ocr" => {
                    queue_ocr(
                        app,
                        &space_id,
                        action
                            .name
                            .as_deref()
                            .ok_or_else(|| anyhow!("name is required"))?,
                    )?;
                    Ok(serde_json::json!({ "queued": true }))
                }
                "ocr_install" => {
                    app.ocr_local_install(action.name.as_deref().unwrap_or(""));
                    Ok(serde_json::json!({ "queued": true, "model": action.name }))
                }
                _ => bail!("invalid media action"),
            }
        }
        _ => bail!("unsupported media method"),
    }
}

/// Read the binary endpoint paired with [`dispatch`].
pub fn blob(app: &App, query: &str) -> Result<MediaBlob> {
    let space_id = query_value(query, "space_id").ok_or_else(|| anyhow!("space_id is required"))?;
    let name = query_value(query, "name").ok_or_else(|| anyhow!("name is required"))?;
    let kind = query_value(query, "kind").unwrap_or_else(|| "file".into());
    if space_id == app.active_space.id
        && component(&name)
        && matches!(kind.as_str(), "image" | "images")
        && let Some(root) = app.incognito_img_dir.as_ref()
        && root.join(&name).exists()
    {
        let path = root.join(&name);
        checked_file(&path, root)?;
        return Ok(MediaBlob {
            bytes: std::fs::read(path)?,
            mime: mime_for_name(&name).into(),
            name,
        });
    }
    read(app, &space_id, &kind, &name)
}

fn component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control)
}

fn space_name(app: &App, space_id: &str) -> Result<String> {
    app.db
        .list_spaces()?
        .into_iter()
        .find(|space| space.id == space_id)
        .map(|space| space.name)
        .filter(|name| component(name))
        .ok_or_else(|| anyhow!("unknown space"))
}

fn checked_file(path: &Path, root: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).context("reading media metadata")?;
    if !metadata.file_type().is_file() {
        bail!("media entry is not a regular file");
    }
    if metadata.file_type().is_symlink() {
        bail!("symlink media entries are not allowed");
    }
    let root = root.canonicalize().context("resolving media directory")?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid media path"))?
        .canonicalize()
        .context("resolving media parent")?;
    if !parent.starts_with(&root) {
        bail!("media path escapes space");
    }
    Ok(path.to_path_buf())
}

fn target(app: &App, space_id: &str, kind: &str, name: &str) -> Result<(PathBuf, PathBuf)> {
    if !component(name) {
        bail!("invalid media name");
    }
    let space = space_name(app, space_id)?;
    let root = match kind {
        "script" | "scripts" => app.space.scripts_dir(&space),
        "file" | "files" | "image" | "images" | "video" | "videos" => app.space.files_dir(&space),
        _ => bail!("invalid media kind"),
    };
    let path = root.join(name);
    if path.exists() {
        checked_file(&path, &root)?;
    }
    Ok((root, path))
}

/// Return a stable MIME for a filename. Unknown files use octet-stream.
#[must_use]
pub fn mime_for_name(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" | "md" | "rs" | "py" | "sh" | "toml" | "yaml" | "yml" | "js" | "ts" | "tsx"
        | "jsx" | "html" | "css" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn image_name(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" | "tif" | "tiff" | "svg"
    )
}

fn modified(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| {
            chrono::DateTime::from_timestamp(
                i64::try_from(time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?,
                0,
            )
        })
        .map_or_else(String::new, |date| date.to_rfc3339())
}

fn file_hash(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| {
            Sha256::digest(bytes)
                .iter()
                .fold(String::new(), |mut hash, byte| {
                    let _ = write!(hash, "{byte:02x}");
                    hash
                })
        })
        .unwrap_or_default()
}

/// List files, images, and scripts for an explicit space.
pub fn catalog(app: &App, space_id: &str) -> Result<serde_json::Value> {
    let space = space_name(app, space_id)?;
    let mut files = Vec::new();
    let mut images = Vec::new();
    let mut scripts = Vec::new();
    let mut videos = Vec::new();
    let file_dir = app.space.files_dir(&space);
    let script_dir = app.space.scripts_dir(&space);
    for (root, kind, out) in [
        (&file_dir, "file", &mut files),
        (&script_dir, "script", &mut scripts),
    ] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !component(&name) || !entry.file_type().is_ok_and(|value| value.is_file()) {
                continue;
            }
            let meta = entry.metadata()?;
            let item = MediaEntry {
                name: name.clone(),
                size: meta.len(),
                modified: modified(&entry.path()),
                mime: mime_for_name(&name).to_string(),
                kind: kind.to_string(),
                hash: file_hash(&entry.path()),
            };
            if kind == "file" && image_name(&name) {
                images.push(MediaEntry {
                    kind: "image".into(),
                    ..item.clone()
                });
            }
            if kind == "file"
                && Path::new(&name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "mp4" | "webm" | "mov" | "mkv"
                        )
                    })
            {
                videos.push(MediaEntry {
                    kind: "video".into(),
                    ..item.clone()
                });
            }
            out.push(item);
        }
    }
    files.sort_by(|left, right| left.name.cmp(&right.name));
    images.sort_by(|left, right| left.name.cmp(&right.name));
    scripts.sort_by(|left, right| left.name.cmp(&right.name));
    videos.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(
        serde_json::json!({ "space_id": space_id, "files": files, "images": images, "videos": videos, "scripts": scripts }),
    )
}

/// Read a file or image blob from the requested space.
pub fn read(app: &App, space_id: &str, kind: &str, name: &str) -> Result<MediaBlob> {
    let (root, path) = target(app, space_id, kind, name)?;
    let metadata = std::fs::metadata(&path).context("reading media metadata")?;
    let size = usize::try_from(metadata.len()).context("media size is too large")?;
    if size > MAX_MEDIA_BYTES {
        bail!("media files must be 64 MiB or smaller");
    }
    Ok(MediaBlob {
        bytes: std::fs::read(checked_file(&path, &root)?)?,
        mime: mime_for_name(name).to_string(),
        name: name.to_string(),
    })
}

/// Create a new file, refusing accidental overwrite.
pub fn write(app: &mut App, space_id: &str, kind: &str, name: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_MEDIA_BYTES {
        bail!("media files must be 64 MiB or smaller");
    }
    let (root, path) = target(app, space_id, kind, name)?;
    std::fs::create_dir_all(&root)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating {name}"))?;
    if let Err(error) = std::io::Write::write_all(&mut file, bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    if (kind == "file" || kind == "files" || kind == "image" || kind == "images")
        && space_id == app.active_space.id
    {
        app.rescan_files();
    }
    Ok(())
}

/// Delete a regular file or script. Missing entries are reported as false.
pub fn delete(app: &mut App, space_id: &str, kind: &str, name: &str) -> Result<bool> {
    let (root, path) = target(app, space_id, kind, name)?;
    if !path.exists() {
        return Ok(false);
    }
    checked_file(&path, &root)?;
    std::fs::remove_file(path)?;
    if space_id == app.active_space.id && kind != "script" && kind != "scripts" {
        app.rescan_files();
    }
    Ok(true)
}

/// Rename a regular file or script within its own directory.
pub fn rename(app: &mut App, space_id: &str, kind: &str, from: &str, to: &str) -> Result<()> {
    let (root, source) = target(app, space_id, kind, from)?;
    if !source.exists() {
        bail!("media entry not found");
    }
    checked_file(&source, &root)?;
    let (_, destination) = target(app, space_id, kind, to)?;
    if destination.exists() {
        bail!("{to} already exists");
    }
    std::fs::rename(source, destination)?;
    if space_id == app.active_space.id && kind != "script" && kind != "scripts" {
        app.rescan_files();
    }
    Ok(())
}

/// Read a UTF-8 script for an editor, bounded to the same limit as the TUI.
pub fn read_script(app: &App, space_id: &str, name: &str) -> Result<String> {
    let blob = read(app, space_id, "script", name)?;
    if blob.bytes.len() > MAX_SCRIPT_BYTES {
        bail!("scripts must be 1 MiB or smaller");
    }
    String::from_utf8(blob.bytes).map_err(|_| anyhow!("script is not UTF-8"))
}

/// Replace a script after an explicit save request.
pub fn write_script(
    app: &mut App,
    space_id: &str,
    name: &str,
    content: &str,
    expected_hash: Option<&str>,
) -> Result<()> {
    if content.len() > MAX_SCRIPT_BYTES || content.contains('\0') {
        bail!("scripts must be 1 MiB or smaller and cannot contain NUL bytes");
    }
    let (root, path) = target(app, space_id, "script", name)?;
    std::fs::create_dir_all(&root)?;
    if path.exists() {
        checked_file(&path, &root)?;
        if let Some(expected) = expected_hash
            && file_hash(&path) != expected
        {
            bail!("script changed on disk; reload before saving");
        }
    }
    std::fs::write(path, content).context("writing script")
}

/// Validate and prepare a selected script for execution outside the app actor.
/// The host must enforce its own timeout and output cap while running this
/// job; this helper deliberately never starts a child process.
pub fn prepare_script(app: &App, space_id: &str, name: &str, args: &[String]) -> Result<ScriptJob> {
    if args.len() > 64 || args.iter().any(|arg| arg.len() > 4096) {
        bail!("too many or oversized script arguments");
    }
    let (root, path) = target(app, space_id, "script", name)?;
    checked_file(&path, &root)?;
    let program = match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("py") => "python3",
        Some("rb") => "ruby",
        Some("js") => "node",
        _ => "sh",
    };
    Ok(ScriptJob {
        program: program.to_string(),
        args: std::iter::once(path.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .collect(),
        cwd: root.to_string_lossy().into_owned(),
        space_id: space_id.to_string(),
    })
}

/// Queue the same OCR flow exposed by the TUI for an active-space file.
pub fn queue_ocr(app: &mut App, space_id: &str, name: &str) -> Result<()> {
    if space_id != app.active_space.id {
        bail!("ocr can only run for the active space");
    }
    let (root, path) = target(app, space_id, "file", name)?;
    checked_file(&path, &root)?;
    app.reocr_file(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Db, space::Space};

    fn app() -> App {
        let root = std::env::temp_dir().join(format!("nexus-media-{}", uuid::Uuid::new_v4()));
        App::new(Db::open_in_memory().expect("db"), None, Space { root })
    }

    #[tokio::test]
    async fn catalog_and_blob_are_space_scoped() {
        let mut app = app();
        let id = app.active_space.id.clone();
        write(&mut app, &id, "image", "photo.png", b"png").expect("write");
        write(&mut app, &id, "script", "run.sh", b"echo ok").expect("write");
        let listing = catalog(&app, &id).expect("catalog");
        assert_eq!(listing["images"][0]["name"], "photo.png");
        assert_eq!(listing["scripts"][0]["name"], "run.sh");
        assert_eq!(
            read(&app, &id, "image", "photo.png").expect("read").mime,
            "image/png"
        );
        assert!(read(&app, &id, "image", "../run.sh").is_err());
    }

    #[test]
    fn script_run_returns_status_and_output() {
        let mut app = app();
        let id = app.active_space.id.clone();
        write_script(&mut app, &id, "hello.sh", "printf hello", None).expect("write");
        let result = prepare_script(&app, &id, "hello.sh", &[]).expect("prepare");
        assert_eq!(result.program, "sh");
        assert_eq!(result.args.len(), 1);
    }
}
