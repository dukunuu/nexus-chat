//! Browser transport for the existing sync merge engine and offline bundles.

use std::io::Cursor;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::app::App;

const MAX_BUNDLE: u64 = 64 * 1024 * 1024;
const MAX_EXPANDED: u64 = 256 * 1024 * 1024;

pub(super) fn dispatch(app: &mut App, method: &str, path: &str, query: &str) -> Result<Value> {
    if method != "GET" {
        bail!("unsupported sync operation");
    }
    match path {
        "/v1/sync/state" => {
            let peers = app.db.load_sync_state()?.into_iter().map(|state| json!({
                "peer_id": state.peer_id, "table": state.table_name, "last_synced_at": state.last_synced_at
            })).collect::<Vec<_>>();
            Ok(
                json!({ "device_id": app.db.device_id()?, "device_name": crate::sync::device_name(), "peers": peers }),
            )
        }
        "/v1/sync" => {
            let peer = super::api::query_param(query, "peer_id");
            Ok(serde_json::to_value(crate::sync::build_changeset(
                &app.db,
                peer.as_deref(),
                &crate::sync::device_name(),
            )?)?)
        }
        _ => bail!("unknown sync route"),
    }
}

pub(super) fn refresh_after_merge(app: &mut App) -> Result<()> {
    app.sessions_cache.clear();
    match app
        .db
        .list_spaces()?
        .into_iter()
        .find(|space| space.id == app.active_space.id)
    {
        Some(space) if space.name != app.active_space.name => app.set_active_space(space),
        None => app.switch_to_default_space()?,
        _ => {}
    }
    if !app.incognito
        && let Some(selected) = app.session.clone()
    {
        if let Some(updated) = app.db.get_session(&selected.id)? {
            if app.chat_task_for_session(&selected.id).is_none()
                && app
                    .research_running
                    .as_ref()
                    .is_none_or(|(id, _)| id != &selected.id)
            {
                app.messages = app.db.load_messages(&selected.id)?;
                app.session = Some(updated);
            }
        } else {
            app.new_session();
        }
    }
    app.files_cache = app.db.list_files(&app.active_space.id)?;
    app.refresh_memory_snapshot();
    app.refresh_toolbox();
    Ok(())
}

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("nexus-web-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pack(app: &App, changeset: &crate::sync::Changeset) -> Result<Vec<u8>> {
    let mut size = 0u64;
    for file in &changeset.files {
        size = size.saturating_add(
            u64::try_from(file.size)
                .unwrap_or(u64::MAX)
                .saturating_mul(2),
        );
    }
    if size > MAX_EXPANDED {
        bail!("bundle is too large; use direct host sync");
    }
    let mut output = Cursor::new(Vec::new());
    crate::sync::write_bundle(&app.db, &app.space, changeset, &mut output)?;
    let bytes = output.into_inner();
    if bytes.len() as u64 > MAX_BUNDLE {
        bail!("bundle exceeds 64 MiB; use direct host sync");
    }
    Ok(bytes)
}

pub(super) fn bundle(app: &mut App, input: Option<&[u8]>) -> Result<Vec<u8>> {
    let Some(bytes) = input else {
        return pack(
            app,
            &crate::sync::build_changeset(&app.db, None, &crate::sync::device_name())?,
        );
    };
    if bytes.len() as u64 > MAX_BUNDLE {
        bail!("bundle exceeds 64 MiB");
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > 20_000 {
        bail!("too many bundle entries");
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_EXPANDED
            || (entry.name() == "changeset.json" && entry.size() > 10 * 1024 * 1024)
        {
            bail!("expanded bundle exceeds limits");
        }
    }
    let scratch = Scratch::new()?;
    let path = scratch.0.join("bundle.zip");
    std::fs::write(&path, bytes)?;
    let changeset = crate::sync::unpack_bundle(&path, &scratch.0)?;
    let (summary, cursors) =
        crate::sync::apply_changeset(&app.db, &app.space, &changeset, Some(&scratch.0))?;
    refresh_after_merge(app)?;
    let mut reply = crate::sync::build_changeset(
        &app.db,
        Some(&changeset.device_id),
        &crate::sync::device_name(),
    )?;
    reply.ack = Some(cursors);
    app.push_status(format!(
        "sync: {} rows, {} files, {} warnings",
        summary.rows_applied,
        summary.files_pulled,
        summary.warnings.len()
    ));
    pack(app, &reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_bundle_transfers_rows_and_rejects_non_zip() {
        let scratch = Scratch::new().unwrap();
        let mut first = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            None,
            crate::space::Space {
                root: scratch.0.join("first"),
            },
        );
        let mut second = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            None,
            crate::space::Space {
                root: scratch.0.join("second"),
            },
        );
        first.db.create_space("transfer").unwrap();
        let bytes = bundle(&mut first, None).unwrap();
        let reply = bundle(&mut second, Some(&bytes)).unwrap();
        assert!(!reply.is_empty());
        assert!(
            second
                .db
                .list_spaces()
                .unwrap()
                .iter()
                .any(|space| space.name == "transfer")
        );
        assert!(bundle(&mut second, Some(b"invalid")).is_err());
    }
}
