//! Space-scoped session management for thin clients.

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::app::App;

#[derive(Debug, Serialize)]
pub(super) struct SessionResult {
    pub id: String,
    pub title: String,
    pub slug: Option<String>,
}

fn session_in_space(app: &App, space_id: &str, id: &str) -> Result<crate::db::Session> {
    if space_id != app.active_space.id {
        bail!("session space is not active")
    }
    app.db
        .list_sessions(space_id)?
        .into_iter()
        .find(|session| session.id == id)
        .ok_or_else(|| anyhow!("unknown session"))
}

pub(super) fn rename(
    app: &mut App,
    space_id: &str,
    id: &str,
    title: &str,
) -> Result<SessionResult> {
    let session = session_in_space(app, space_id, id)?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        bail!("title must be 1–200 characters")
    }
    app.db.set_session_title(id, title, None)?;
    if let Some(cached) = app.sessions_cache.iter_mut().find(|cached| cached.id == id) {
        cached.title = title.to_string();
    }
    if app.session.as_ref().is_some_and(|active| active.id == id)
        && let Some(active) = app.session.as_mut()
    {
        active.title = title.to_string();
    }
    Ok(SessionResult {
        id: session.id,
        title: title.to_string(),
        slug: session.slug,
    })
}

pub(super) fn delete(app: &mut App, space_id: &str, id: &str) -> Result<serde_json::Value> {
    session_in_space(app, space_id, id)?;
    if app.chat_task_for_session(id).is_some()
        || app
            .research_running
            .as_ref()
            .is_some_and(|(session, _)| session == id)
    {
        bail!("stop the session's task before deleting it")
    }
    app.db.delete_session(id)?;
    app.sessions_cache.retain(|session| session.id != id);
    if app.session.as_ref().is_some_and(|active| active.id == id) {
        app.new_session();
    }
    Ok(serde_json::json!({ "deleted": true, "id": id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_is_scoped_and_validated() {
        let root =
            std::env::temp_dir().join(format!("nexus-session-view-{}", uuid::Uuid::new_v4()));
        let mut app = App::new(
            crate::db::Db::open_in_memory().expect("db"),
            None,
            crate::space::Space { root },
        );
        let space = app.active_space.id.clone();
        let session = app
            .db
            .create_session("old", "model", &space, "chat")
            .expect("session");
        assert!(rename(&mut app, "wrong", &session.id, "new").is_err());
        assert!(rename(&mut app, &space, &session.id, " ").is_err());
        assert_eq!(
            rename(&mut app, &space, &session.id, "new")
                .expect("rename")
                .title,
            "new"
        );
    }
}
