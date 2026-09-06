//! Host configuration and login. Credentials are write-only at the HTTP boundary.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::app::{App, AppEvent, LoginMsg};

#[derive(Default)]
pub(super) struct State {
    code: Option<String>,
    message: String,
    login_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(task) = self.login_task.take() {
            task.abort();
        }
    }
}

impl State {
    pub(super) fn observe(&mut self, event: &AppEvent) {
        match event {
            AppEvent::Login(Some(LoginMsg::Status(status))) => {
                if let Some(code) = status.strip_prefix("device-code:")
                    && code.len() <= 64
                    && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    self.code = Some(code.to_string());
                    self.message = "Enter the code at the verification page.".into();
                }
            }
            AppEvent::Login(Some(LoginMsg::Done(result))) => {
                self.code = None;
                self.message = if result.is_ok() {
                    "Login saved."
                } else {
                    "Login failed or expired. Try again."
                }
                .into();
                self.login_task = None;
            }
            _ => {}
        }
    }

    fn login(&mut self, app: &mut App, body: &[u8]) -> Result<Value> {
        #[derive(Deserialize)]
        struct Input {
            backend: String,
            #[serde(default)]
            key: String,
            #[serde(default)]
            cancel: bool,
        }
        let input: Input = serde_json::from_slice(body)?;
        if input.backend == "codex" {
            if let Some(task) = self.login_task.take() {
                task.abort();
            }
            app.login_rx = None;
            self.code = None;
            if input.cancel {
                self.message = "Login cancelled.".into();
            } else {
                self.message = "Starting device login…".into();
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                app.login_rx = Some(rx);
                self.login_task = Some(tokio::spawn(async move {
                    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel();
                    let login = crate::config::login_openai_codex_remote(status_tx);
                    tokio::pin!(login);
                    loop {
                        tokio::select! {
                            result = &mut login => { let _ = tx.send(LoginMsg::Done(result.map_err(|_| "Device login failed".to_string()))); break; }
                            Some(status) = status_rx.recv() => { let _ = tx.send(LoginMsg::Status(status)); }
                        }
                    }
                }));
            }
        } else {
            if !matches!(input.backend.as_str(), "openai" | "openrouter" | "opencode") {
                bail!("unknown backend");
            }
            let key = input.key.trim();
            if key.is_empty() || key.len() > 16_384 || key.chars().any(char::is_control) {
                bail!("invalid API key");
            }
            crate::config::save_provider_key(&input.backend, key)
                .map_err(|_| anyhow!("could not save provider key"))?;
            match input.backend.as_str() {
                "openai" => app.saved.openai_key = Some(key.into()),
                "openrouter" => app.saved.openrouter_key = Some(key.into()),
                _ => app.saved.opencode_key = Some(key.into()),
            }
            app.rebuild_all_backends();
            app.fetch_models();
            app.refresh_toolbox();
            self.message = "API key saved. Loading models…".into();
        }
        Ok(json!({ "ok": true }))
    }
}

pub(super) fn dispatch(
    app: &mut App,
    state: &mut State,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> Result<Value> {
    match (method, path) {
        ("GET", "/v1/settings") => {
            let mut settings = serde_json::to_value(app.snapshot()?.settings)?;
            settings["show_stats"] = json!(app.settings.show_stats);
            settings["show_reasoning"] = json!(app.settings.show_reasoning);
            settings["hide_hints"] = json!(app.settings.hide_hints);
            settings["local_ocr_model"] = json!(app.local_ocr_model);
            settings["usage_range"] = json!(app.usage_range.key());
            Ok(settings)
        }
        ("PUT", "/v1/settings") => {
            #[derive(Deserialize)]
            struct Input {
                key: String,
                value: String,
                space_id: String,
            }
            let input: Input = serde_json::from_slice(body)?;
            if input.space_id != app.active_space.id {
                bail!("active space changed; refresh settings");
            }
            // Don't echo secrets or credential-bearing URLs in validation errors.
            app.set_setting(&input.key, input.value.trim())
                .map_err(|_| anyhow!("setting could not be saved; check the value"))?;
            Ok(json!({ "ok": true }))
        }
        ("GET", "/v1/login") => Ok(json!({
            "pending": app.login_rx.is_some(), "user_code": state.code,
            "verification_url": "https://auth.openai.com/codex/device", "message": state.message,
            "configured": app.backends.configured_tags().iter().map(|tag| tag.name()).collect::<Vec<_>>()
        })),
        ("POST", "/v1/login") => state.login(app, body),
        ("POST", "/v1/attachments") => attach_image(app, query, body),
        ("POST", "/v1/models/refresh") => {
            app.fetch_models();
            Ok(json!({"ok": true}))
        }
        ("GET" | "PUT", "/v1/settings/document") => document(app, method, query, body),
        _ => bail!("unsupported configuration operation"),
    }
}

fn attach_image(app: &mut App, query: &str, body: &[u8]) -> Result<Value> {
    if super::api::query_param(query, "space_id").as_deref() != Some(&app.active_space.id) {
        bail!("active space changed; upload again");
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(body)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.to_rgba8();
    let markdown = app
        .save_clipboard_image(
            usize::try_from(image.width())?,
            usize::try_from(image.height())?,
            image.as_raw(),
        )
        .ok_or_else(|| anyhow!("could not save attachment"))?;
    Ok(json!({ "markdown": markdown }))
}

fn document(app: &mut App, method: &str, query: &str, body: &[u8]) -> Result<Value> {
    #[derive(Deserialize)]
    struct Edit {
        content: String,
        hash: String,
    }
    let id = super::api::query_param(query, "space_id").unwrap_or_default();
    if id != app.active_space.id {
        bail!("active space changed; reload document");
    }
    let kind = super::api::query_param(query, "kind").unwrap_or_default();
    let path = match kind.as_str() {
        "system" => crate::config::system_prompt_path()?,
        "instructions" => app.space.instructions_path(&app.active_space.name),
        "memory" => app.space.memory_path(&app.active_space.name),
        _ => bail!("unknown document"),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if kind == "system" {
                app.base_system_prompt.clone()
            } else {
                String::new()
            }
        }
        Err(error) => return Err(error.into()),
    };
    let hash = super::api::hex_string(&Sha256::digest(content.as_bytes()));
    if method == "GET" {
        return Ok(json!({ "content": content, "hash": hash }));
    }
    let edit: Edit = serde_json::from_slice(body)?;
    if edit.hash != hash {
        bail!("document changed; reload before saving");
    }
    if edit.content.len() > 1024 * 1024 {
        bail!("document must be 1 MiB or smaller");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &edit.content)?;
    if kind == "system" {
        app.reload_base_system_prompt();
    } else {
        app.refresh_memory_snapshot();
        app.refresh_toolbox();
    }
    Ok(
        json!({"content": edit.content, "hash": super::api::hex_string(&Sha256::digest(edit.content.as_bytes()))}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_rejects_stale_edits_and_wrong_spaces() {
        let root = std::env::temp_dir().join(format!("nexus-admin-{}", uuid::Uuid::new_v4()));
        let mut app = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            None,
            crate::space::Space { root: root.clone() },
        );
        let query = format!("space_id={}&kind=instructions", app.active_space.id);
        let first = document(&mut app, "GET", &query, b"").unwrap();
        let edit =
            serde_json::to_vec(&json!({"hash": first["hash"], "content": "first edit"})).unwrap();
        document(&mut app, "PUT", &query, &edit).unwrap();
        assert!(document(&mut app, "PUT", &query, &edit).is_err());
        assert!(document(&mut app, "GET", "space_id=wrong&kind=instructions", b"").is_err());
        assert_eq!(
            document(&mut app, "GET", &query, b"").unwrap()["content"],
            "first edit"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn login_status_accepts_only_device_challenges_and_clears_on_completion() {
        let mut state = State::default();
        state.observe(&AppEvent::Login(Some(LoginMsg::Status(
            "provider-secret".into(),
        ))));
        assert!(state.code.is_none());
        state.observe(&AppEvent::Login(Some(LoginMsg::Status(
            "device-code:ABCD-1234".into(),
        ))));
        assert_eq!(state.code.as_deref(), Some("ABCD-1234"));
        state.observe(&AppEvent::Login(Some(LoginMsg::Done(Err(
            "provider-secret".into(),
        )))));
        assert!(state.code.is_none());
        assert!(!state.message.contains("provider-secret"));
    }

    #[test]
    fn image_upload_uses_incognito_storage_and_validates_space() {
        let root = std::env::temp_dir().join(format!("nexus-attachment-{}", uuid::Uuid::new_v4()));
        let mut app = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            None,
            crate::space::Space { root },
        );
        app.incognito = true;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(attach_image(&mut app, "space_id=wrong", bytes.get_ref()).is_err());
        let query = format!("space_id={}", app.active_space.id);
        assert!(attach_image(&mut app, &query, b"invalid image").is_err());
        let result = attach_image(&mut app, &query, bytes.get_ref()).unwrap();
        assert!(
            result["markdown"]
                .as_str()
                .unwrap()
                .starts_with("![pasted image](")
        );
        let directory = app.incognito_img_dir.clone().unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        assert!(app.db.list_files(&app.active_space.id).unwrap().is_empty());
        app.cleanup_incognito_images();
        assert!(!directory.exists());
    }
}
