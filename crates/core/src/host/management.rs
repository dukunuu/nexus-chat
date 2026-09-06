//! Synchronous management operations used by the web host.
//!
//! The HTTP actor owns an [`App`] and calls these functions while it is the
//! only mutable owner.  Keeping the validation here makes the web surface
//! obey the same space and session boundaries as the TUI.

use std::fs;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::db::{Persona, Watch};

/// A persona row accepted by the management API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PersonaInput {
    pub name: String,
    pub model: String,
    pub blurb: String,
}

/// Public swarm management state.
#[derive(Debug, Clone, Serialize)]
pub struct SwarmState {
    pub session_id: String,
    pub enabled: bool,
    pub running: bool,
    pub personas: Vec<PersonaInput>,
}

/// Public skill metadata and, when requested, its markdown body.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub managed: bool,
    pub body: Option<String>,
}

/// Public watch row.
#[derive(Debug, Clone, Serialize)]
pub struct WatchInfo {
    pub id: String,
    pub space_id: String,
    pub topic: String,
    pub interval_hours: i64,
    pub session_id: String,
    pub last_run_at: Option<String>,
}

impl From<Persona> for PersonaInput {
    fn from(value: Persona) -> Self {
        Self {
            name: value.name,
            model: value.model,
            blurb: value.blurb,
        }
    }
}

impl From<Watch> for WatchInfo {
    fn from(value: Watch) -> Self {
        Self {
            id: value.id,
            space_id: value.space_id,
            topic: value.topic,
            interval_hours: value.interval_hours,
            session_id: value.session_id,
            last_run_at: value.last_run_at,
        }
    }
}

fn session_in_space(app: &App, session_id: &str, space_id: &str) -> Result<crate::db::Session> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    app.db
        .list_sessions(space_id)?
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| anyhow!("unknown session for space"))
}

fn personas(app: &App, session_id: &str) -> Result<Vec<PersonaInput>> {
    Ok(app
        .db
        .list_swarm_personas(session_id)?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Read the active session's swarm state.
pub fn swarm_state(app: &App, session_id: &str) -> Result<SwarmState> {
    let session = session_in_space(app, session_id, &app.active_space.id)?;
    Ok(SwarmState {
        session_id: session.id,
        enabled: session.swarm_mode,
        running: app.swarm_session.as_deref() == Some(session_id),
        personas: personas(app, session_id)?,
    })
}

/// Replace a session's swarm roster after validating every row.
pub fn save_swarm_roster(
    app: &mut App,
    space_id: &str,
    session_id: &str,
    rows: Vec<PersonaInput>,
) -> Result<SwarmState> {
    session_in_space(app, session_id, space_id)?;
    if rows.len() > 6 {
        bail!("a swarm roster may contain at most six personas")
    }
    let mut personas = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row.name.trim().to_string();
        let model = row.model.trim().to_string();
        let blurb = row.blurb.trim().to_string();
        if name.is_empty()
            || name.chars().count() > 80
            || model.is_empty()
            || model.chars().count() > 240
        {
            bail!("persona name and model are required and must be reasonably sized")
        }
        if blurb.chars().count() > 2_000 {
            bail!("persona blurb is too long")
        }
        personas.push(Persona { name, model, blurb });
    }
    app.db.save_swarm_personas(session_id, &personas)?;
    if app
        .session
        .as_ref()
        .is_some_and(|session| session.id == session_id)
    {
        app.swarm_cache.clone_from(&personas);
    }
    swarm_state(app, session_id)
}

/// Toggle swarm mode for a session.  The running state is intentionally left
/// to the app's existing start/stop lifecycle.
pub fn set_swarm_mode(
    app: &mut App,
    space_id: &str,
    session_id: &str,
    enabled: bool,
) -> Result<SwarmState> {
    let session = session_in_space(app, session_id, space_id)?;
    app.db.set_session_swarm_mode(&session.id, enabled)?;
    if app
        .session
        .as_ref()
        .is_some_and(|active| active.id == session_id)
        && let Some(active) = app.session.as_mut()
    {
        active.swarm_mode = enabled;
    }
    swarm_state(app, session_id)
}

/// Start a turn for the active session. Swarm streaming is owned by App and
/// is therefore deliberately not duplicated in the HTTP layer.
pub fn start_swarm(app: &mut App, space_id: &str, session_id: &str) -> Result<SwarmState> {
    session_in_space(app, session_id, space_id)?;
    if app
        .session
        .as_ref()
        .is_none_or(|session| session.id != session_id)
    {
        bail!("the swarm session must be active")
    }
    app.start_swarm_turn();
    swarm_state(app, session_id)
}

/// Stop the currently running swarm when it belongs to `session_id`.
pub fn stop_swarm(app: &mut App, space_id: &str, session_id: &str) -> Result<SwarmState> {
    session_in_space(app, session_id, space_id)?;
    if app.swarm_session.as_deref() == Some(session_id) {
        app.stop_swarm();
    }
    swarm_state(app, session_id)
}

/// List skills visible to the current data directory.
#[must_use]
pub fn list_skills(app: &App) -> Vec<SkillInfo> {
    app.skills
        .iter()
        .map(|skill| SkillInfo {
            name: skill.name.clone(),
            description: skill.description.clone(),
            managed: app.skill_is_app_managed(skill),
            body: None,
        })
        .collect()
}

/// Return one skill, including its body when requested.
pub fn skill_detail(app: &App, name: &str, include_body: bool) -> Result<SkillInfo> {
    let skill = app
        .skills
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| anyhow!("unknown skill: {name}"))?;
    let body = include_body
        .then(|| fs::read_to_string(skill.dir.join("SKILL.md")))
        .transpose()?;
    Ok(SkillInfo {
        name: skill.name.clone(),
        description: skill.description.clone(),
        managed: app.skill_is_app_managed(skill),
        body,
    })
}

/// Arm a skill for the next message, using the same one-shot field as `/skill`.
pub fn arm_skill(app: &mut App, space_id: &str, name: &str) -> Result<()> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    if !app.skills.iter().any(|skill| skill.name == name) {
        bail!("unknown skill: {name}")
    }
    app.forced_skill = Some(name.to_string());
    Ok(())
}

/// Remove an app-managed skill. External Agent Skills roots are read-only.
pub fn remove_skill(app: &mut App, space_id: &str, name: &str) -> Result<()> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    let skill = app
        .skills
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| anyhow!("unknown skill: {name}"))?;
    if !app.skill_is_app_managed(skill) {
        bail!("skill is read-only")
    }
    fs::remove_dir_all(&skill.dir)?;
    app.reload_skills();
    Ok(())
}

/// Validate a GitHub shorthand and start the existing background installer.
pub fn install_skill(app: &mut App, space_id: &str, source: &str) -> Result<()> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    if crate::skills::parse_gh_shorthand(source).is_none() {
        bail!("expected owner/repo/path")
    }
    app.start_skill_install(source);
    Ok(())
}

/// List watches belonging to the active space.
pub fn list_watches(app: &App) -> Result<Vec<WatchInfo>> {
    Ok(app
        .db
        .list_watches(&app.active_space.id)?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Create a watch and point it at an existing session in the active space.
pub fn create_watch(
    app: &mut App,
    space_id: &str,
    topic: &str,
    interval_hours: i64,
) -> Result<WatchInfo> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    let topic = topic.trim();
    if topic.is_empty() || topic.chars().count() > 1_000 {
        bail!("watch topic is required and must be at most 1000 characters")
    }
    if !(1..=8_760).contains(&interval_hours) {
        bail!("interval must be between 1 and 8760 hours")
    }
    let before: std::collections::HashSet<String> = app
        .db
        .list_watches(space_id)?
        .into_iter()
        .map(|watch| watch.id)
        .collect();
    app.create_watch(topic);
    let watch = app
        .db
        .list_watches(space_id)?
        .into_iter()
        .find(|watch| !before.contains(&watch.id) && watch.topic == topic)
        .ok_or_else(|| anyhow!("watch was not persisted"))?;
    if watch.interval_hours != interval_hours {
        app.db.set_watch_interval(&watch.id, interval_hours)?;
    }
    Ok(watch.into())
}

/// Delete a watch only when it belongs to the active space.
pub fn delete_watch(app: &mut App, space_id: &str, id: &str) -> Result<bool> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    if !app
        .db
        .list_watches(&app.active_space.id)?
        .iter()
        .any(|watch| watch.id == id)
    {
        return Ok(false);
    }
    app.db.delete_watch(id)?;
    app.watches_cache.retain(|watch| watch.id != id);
    Ok(true)
}

/// Force-run a watch through the app's existing research lifecycle.
pub fn run_watch(app: &mut App, space_id: &str, id: &str) -> Result<bool> {
    if space_id != app.active_space.id {
        bail!("management operations require the active space")
    }
    let Some(watch) = app
        .db
        .list_watches(&app.active_space.id)?
        .into_iter()
        .find(|watch| watch.id == id)
    else {
        return Ok(false);
    };
    Ok(app.run_one_watch(&watch))
}

/// Dispatch the management surface used by the host actor.
///
/// Contract: `GET /v1/swarm?session_id=...`, `PUT /v1/swarm/roster`,
/// `POST /v1/swarm/mode|start|stop`; `GET /v1/skills`,
/// `GET /v1/skills/:name?detail=1`, `POST /v1/skills/arm|install`,
/// `DELETE /v1/skills/:name`; `GET /v1/watches`, `POST /v1/watches`,
/// `DELETE /v1/watches/:id`, and `POST /v1/watches/:id/run`.
/// Bodies are JSON and responses are JSON values.  Session and watch
/// mutations are always checked against the active space.
pub fn dispatch(
    app: &mut App,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> Result<serde_json::Value> {
    let payload: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(body)?
    };
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, path) {
        ("GET", "/v1/swarm")
        | ("PUT", "/v1/swarm/roster")
        | ("POST", "/v1/swarm/mode" | "/v1/swarm/start" | "/v1/swarm/stop") => {
            dispatch_swarm(app, method, path, query, &payload)
        }
        ("GET", "/v1/skills") | ("POST", "/v1/skills/arm" | "/v1/skills/install") => {
            dispatch_skills(app, method, path, query, &payload)
        }
        ("GET" | "DELETE", _path) if segments.len() == 3 && segments[..2] == ["v1", "skills"] => {
            dispatch_skill_item(app, method, query, segments[2], &payload)
        }
        ("GET" | "POST", "/v1/watches") => {
            dispatch_watches(app, method, segments[2..].first().copied(), &payload)
        }
        ("DELETE" | "POST", _path) if segments.len() >= 3 && segments[..2] == ["v1", "watches"] => {
            dispatch_watch_item(app, method, segments[2], &payload)
        }
        _ => bail!("unknown management route"),
    }
}

fn value_str<'a>(payload: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("missing {key}"))
}

fn dispatch_swarm(
    app: &mut App,
    method: &str,
    path: &str,
    query: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    let id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| super::api::query_param(query, "session_id"))
        .ok_or_else(|| anyhow!("missing session_id"))?;
    let space =
        value_str(payload, "space_id").map_or_else(|_| app.active_space.id.clone(), str::to_owned);
    let result = match path {
        "/v1/swarm" => swarm_state(app, &id),
        "/v1/swarm/roster" => save_swarm_roster(
            app,
            &space,
            &id,
            serde_json::from_value(
                payload
                    .get("personas")
                    .cloned()
                    .ok_or_else(|| anyhow!("missing personas"))?,
            )?,
        ),
        "/v1/swarm/mode" => set_swarm_mode(
            app,
            &space,
            &id,
            payload
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| anyhow!("missing enabled"))?,
        ),
        "/v1/swarm/start" => start_swarm(app, &space, &id),
        "/v1/swarm/stop" => stop_swarm(app, &space, &id),
        _ => bail!("unknown swarm route"),
    }?;
    let _ = method;
    Ok(serde_json::to_value(result)?)
}

fn dispatch_skills(
    app: &mut App,
    method: &str,
    path: &str,
    query: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    match path {
        "/v1/skills" => Ok(serde_json::json!({ "skills": list_skills(app) })),
        "/v1/skills/arm" => {
            arm_skill(
                app,
                value_str(payload, "space_id")?,
                value_str(payload, "name")?,
            )?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "/v1/skills/install" => {
            install_skill(
                app,
                value_str(payload, "space_id")?,
                value_str(payload, "source")?,
            )?;
            Ok(serde_json::json!({ "ok": true }))
        }
        _ => {
            let _ = (method, query);
            bail!("unknown skill route")
        }
    }
}

fn dispatch_skill_item(
    app: &mut App,
    method: &str,
    query: &str,
    name: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    if method == "GET" {
        return Ok(serde_json::to_value(skill_detail(
            app,
            name,
            super::api::query_param(query, "detail").is_some_and(|v| v == "1" || v == "true"),
        )?)?);
    }
    remove_skill(app, value_str(payload, "space_id")?, name)?;
    Ok(serde_json::json!({ "ok": true }))
}

fn dispatch_watches(
    app: &mut App,
    method: &str,
    _id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    if method == "GET" {
        return Ok(serde_json::json!({ "watches": list_watches(app)? }));
    }
    Ok(serde_json::to_value(create_watch(
        app,
        value_str(payload, "space_id")?,
        value_str(payload, "topic")?,
        payload
            .get("interval_hours")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(24),
    )?)?)
}

fn dispatch_watch_item(
    app: &mut App,
    method: &str,
    id: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    let space = value_str(payload, "space_id")?;
    if method == "DELETE" {
        return Ok(serde_json::json!({ "deleted": delete_watch(app, space, id)? }));
    }
    Ok(serde_json::json!({ "started": run_watch(app, space, id)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!("nexus-management-{}", uuid::Uuid::new_v4()));
        App::new(
            crate::db::Db::open_in_memory().expect("db"),
            None,
            crate::space::Space { root },
        )
    }

    #[test]
    fn github_validation_is_reused_for_install() {
        assert!(crate::skills::parse_gh_shorthand("owner/repo/path").is_some());
        assert!(crate::skills::parse_gh_shorthand("../escape").is_none());
    }

    #[test]
    fn swarm_roster_and_mode_are_scoped_and_persisted() {
        let mut app = test_app();
        let space = app.active_space.id.clone();
        let session = app
            .db
            .create_session("chat", "model", &space, "chat")
            .expect("session");
        let state = save_swarm_roster(
            &mut app,
            &space,
            &session.id,
            vec![PersonaInput {
                name: "Ada".into(),
                model: "model".into(),
                blurb: "careful".into(),
            }],
        )
        .expect("roster");
        assert_eq!(state.personas[0].name, "Ada");
        assert!(
            set_swarm_mode(&mut app, &space, &session.id, true)
                .expect("mode")
                .enabled
        );
        assert!(save_swarm_roster(&mut app, "wrong-space", &session.id, Vec::new()).is_err());
    }

    #[test]
    fn watches_validate_space_and_interval_and_delete_only_owned_rows() {
        let mut app = test_app();
        let space = app.active_space.id.clone();
        assert!(create_watch(&mut app, &space, "topic", 0).is_err());
        assert!(delete_watch(&mut app, "wrong-space", "missing").is_err());
        assert!(!run_watch(&mut app, &space, "missing").expect("missing watch"));
    }

    #[test]
    fn managed_skill_boundary_rejects_unknown_and_wrong_space() {
        let mut app = test_app();
        assert!(arm_skill(&mut app, "wrong-space", "missing").is_err());
        let space = app.active_space.id.clone();
        assert!(remove_skill(&mut app, &space, "missing").is_err());
    }
}
