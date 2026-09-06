//! Space-scoped app catalog and bounded source-file editing for thin clients.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::app::App;

const MAX_SOURCE: u64 = 1024 * 1024;
const MAX_FILES: usize = 2000;

#[derive(Deserialize)]
pub(super) struct Edit {
    content: String,
    hash: String,
}

#[derive(Serialize)]
struct Source {
    content: String,
    hash: String,
}

fn component(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', '\0'])
}

fn space_name(app: &App, id: Option<&str>) -> Result<String> {
    let id = id.unwrap_or(&app.active_space.id);
    app.db
        .list_spaces()?
        .into_iter()
        .find(|space| space.id == id)
        .map(|space| space.name)
        .filter(|name| component(name))
        .ok_or_else(|| anyhow!("unknown space"))
}

fn app_root(app: &App, space: &str, name: &str) -> Result<PathBuf> {
    if !component(name) {
        bail!("invalid app name");
    }
    let base = app.space.apps_dir(space);
    let root = base.join(name);
    if std::fs::symlink_metadata(&root)?.file_type().is_symlink() {
        bail!("symlink apps are not editable");
    }
    let root = root.canonicalize()?;
    if !root.starts_with(base.canonicalize()?) || !root.is_dir() {
        bail!("invalid app directory");
    }
    Ok(root)
}

pub(super) fn catalog(app: &App, id: Option<&str>) -> Result<serde_json::Value> {
    let space = space_name(app, id)?;
    let mut apps = Vec::new();
    let dir = app.space.apps_dir(&space);
    if !dir.exists() {
        return Ok(serde_json::json!({ "apps": apps }));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !component(&name) || !entry.file_type()?.is_dir() {
            continue;
        }
        let registry = app
            .app_server
            .as_ref()
            .map(crate::appserver::AppServer::registry);
        let uuid = registry.and_then(|registry| registry.resolve(&space, &name));
        let served_from = uuid
            .as_deref()
            .and_then(|uuid| registry?.lookup(uuid))
            .and_then(|entry| entry.served_from);
        let url = uuid.map(|uuid| format!("/apps/{uuid}/"));
        apps.push(serde_json::json!({ "name": name, "url": url, "served_from": served_from }));
    }
    apps.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(serde_json::json!({ "apps": apps }))
}

/// Delete an app directory and its registry capability, scoped to one space.
pub(super) fn delete(app: &App, id: Option<&str>, name: &str) -> Result<serde_json::Value> {
    let space = space_name(app, id)?;
    let root = app_root(app, &space, name)?;
    std::fs::remove_dir_all(&root)?;
    if let Some(server) = &app.app_server {
        server.registry().remove(&space, name);
    }
    Ok(serde_json::json!({ "deleted": true, "name": name }))
}

/// Register an existing on-disk app as a public capability. The app server's
/// startup scanner normally does this automatically; this action handles an
/// orphan app created while the server was already running.
pub(super) fn register(app: &App, id: Option<&str>, name: &str) -> Result<serde_json::Value> {
    let space = space_name(app, id)?;
    let root = app.space.apps_dir(&space).join(name);
    if !component(name) || !root.is_dir() {
        bail!("unknown app directory")
    }
    let Some(server) = &app.app_server else {
        bail!("app server unavailable")
    };
    let uuid = server
        .registry()
        .resolve(&space, name)
        .unwrap_or_else(|| server.registry().assign(&space, name));
    Ok(serde_json::json!({ "name": name, "uuid": uuid, "url": format!("/apps/{uuid}/") }))
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 16 {
        bail!("app directory is too deep");
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !component(&name) || name == "node_modules" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &entry.path(), files, depth + 1)?;
        } else if kind.is_file() {
            if files.len() >= MAX_FILES {
                bail!("app has too many source files");
            }
            files.push(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

pub(super) fn files(app: &App, id: Option<&str>, name: &str) -> Result<serde_json::Value> {
    let root = app_root(app, &space_name(app, id)?, name)?;
    let mut files = Vec::new();
    walk(&root, &root, &mut files, 0)?;
    files.sort();
    Ok(serde_json::json!({ "files": files }))
}

fn source_path(root: &Path, path: &str) -> Result<PathBuf> {
    if !path
        .split('/')
        .all(|part| component(part) && part != "node_modules")
    {
        bail!("invalid source path");
    }
    let mut target = root.to_path_buf();
    for part in path.split('/') {
        target.push(part);
        if std::fs::symlink_metadata(&target)?.file_type().is_symlink() {
            bail!("symlink sources are not editable");
        }
    }
    if !target.is_file() {
        bail!("source file not found");
    }
    Ok(target)
}

fn read_source(path: &Path) -> Result<Source> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE {
        bail!("source files must be 1 MiB or smaller");
    }
    let hash = super::api::hex_string(&Sha256::digest(&bytes));
    let content =
        String::from_utf8(bytes).map_err(|_| anyhow!("only UTF-8 text files can be edited"))?;
    if content.contains('\0') {
        bail!("binary files cannot be edited");
    }
    Ok(Source { content, hash })
}

pub(super) fn source(
    app: &App,
    id: Option<&str>,
    name: &str,
    path: &str,
    edit: Option<Edit>,
) -> Result<serde_json::Value> {
    let root = app_root(app, &space_name(app, id)?, name)?;
    let target = source_path(&root, path)?;
    let current = read_source(&target)?;
    if let Some(edit) = edit {
        if edit.hash != current.hash {
            bail!("file changed on disk; reload before saving");
        }
        if edit.content.len() as u64 > MAX_SOURCE || edit.content.contains('\0') {
            bail!("invalid source content");
        }
        let temporary = target.with_extension(format!("nexus-edit-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.set_permissions(std::fs::metadata(&target)?.permissions())?;
            file.write_all(edit.content.as_bytes())?;
            std::fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        return Ok(serde_json::to_value(read_source(&target)?)?);
    }
    Ok(serde_json::to_value(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_and_editor_are_scoped_and_preserve_newer_content() {
        let root = std::env::temp_dir().join(format!("nexus-app-editor-{}", uuid::Uuid::new_v4()));
        let app = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            None,
            crate::space::Space { root: root.clone() },
        );
        let dir = app.space.apps_dir(&app.active_space.name).join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "original").unwrap();
        let other = app.db.create_space("other").unwrap();
        assert_eq!(
            catalog(&app, Some(&other.id)).unwrap()["apps"],
            serde_json::json!([])
        );
        assert_eq!(catalog(&app, None).unwrap()["apps"][0]["name"], "demo");
        assert_eq!(
            files(&app, None, "demo").unwrap()["files"],
            serde_json::json!(["index.html"])
        );
        assert!(source(&app, Some(&other.id), "demo", "index.html", None).is_err());
        assert!(source(&app, None, "demo", "../secret", None).is_err());
        let loaded = source(&app, None, "demo", "index.html", None).unwrap();
        let hash = loaded["hash"].as_str().unwrap().to_string();
        source(
            &app,
            None,
            "demo",
            "index.html",
            Some(Edit {
                content: "saved".into(),
                hash: hash.clone(),
            }),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("index.html")).unwrap(),
            "saved"
        );
        assert!(
            source(
                &app,
                None,
                "demo",
                "index.html",
                Some(Edit {
                    content: "stale".into(),
                    hash
                })
            )
            .is_err()
        );
        std::fs::write(dir.join("large.txt"), vec![b'a'; MAX_SOURCE as usize + 1]).unwrap();
        assert!(source(&app, None, "demo", "large.txt", None).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("index.html"), dir.join("link.html")).unwrap();
            assert!(source(&app, None, "demo", "link.html", None).is_err());
        }
        delete(&app, None, "demo").unwrap();
        assert!(!dir.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
