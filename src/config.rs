use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

pub const OPENROUTER_ENV_KEY: &str = "OPENROUTER_API_KEY";
pub const OPENAI_ENV_KEY: &str = "OPENAI_API_KEY";
pub const OPENCODE_ENV_KEY: &str = "OPENCODE_API_KEY";

const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../assets/system-prompt-base.md");

#[derive(Debug, Deserialize)]
struct Config {
    provider: Provider,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    openrouter_key: String,
    #[serde(default)]
    openai_key: String,
    #[serde(default)]
    opencode_key: String,
    #[serde(default)]
    openai_codex: Option<CodexCredentials>,
    /// Which backend ("openrouter" / "openai" / "opencode" / "codex") was
    /// last made active via `/login` or `/backend` — read back at startup
    /// so the app resumes on the same backend instead of always defaulting
    /// to whichever credential wins a fixed priority order.
    #[serde(default)]
    active_backend: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CodexCredentials {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub account_id: String,
}

/// Every credential the app knows about at once — populated at startup and
/// kept in sync as `/login` adds more, so `/backend` can switch the active
/// provider without re-authenticating.
#[derive(Debug, Clone, Default)]
pub struct SavedCreds {
    pub openrouter_key: Option<String>,
    pub openai_key: Option<String>,
    pub opencode_key: Option<String>,
    pub codex: Option<CodexCredentials>,
    /// Which backend was last active ("openrouter" / "openai" / "opencode"
    /// / "codex"), if one was ever explicitly chosen.
    pub active_backend: Option<String>,
}

/// XDG dirs for the app: `~/.config/nexus-chat` and `~/.local/share/nexus-chat`.
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "nexus-chat").context("could not resolve home directory")
}

pub fn config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

/// Optional custom start-screen banner: paste any ASCII art into
/// `~/.config/nexus-chat/banner.txt` and it replaces the built-in one.
pub fn load_banner() -> Option<String> {
    let path = project_dirs().ok()?.config_dir().join("banner.txt");
    let art = std::fs::read_to_string(path).ok()?;
    (!art.trim().is_empty()).then(|| art.trim_end().to_string())
}

/// The base system prompt file (identity/formatting/scope, with a
/// `{{verbosity}}` placeholder App fills in). Lives beside config.toml, not
/// per-space — this is app-level, not chat-level. Editable via `$EDITOR`.
pub fn system_prompt_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("system_prompt.md"))
}

/// Read the base system prompt, scaffolding the built-in default on first run.
pub fn load_system_prompt() -> Result<String> {
    let path = system_prompt_path()?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, DEFAULT_SYSTEM_PROMPT)
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(DEFAULT_SYSTEM_PROMPT.to_string());
    }
    std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
}

/// Resolve every configured credential at once: `$OPENROUTER_API_KEY`/
/// `$OPENAI_API_KEY`/`$OPENCODE_API_KEY` (if set) win over the config file
/// for their respective slot, Codex creds always come from the config file
/// (refreshed if stale). Scaffolds an empty config on first run. Never
/// fails on missing credentials — the app launches regardless and they can
/// be set in-app with `/login`.
pub async fn load_all_providers() -> Result<SavedCreds> {
    let path = config_path()?;
    if !path.exists() {
        write_provider_config("", "", "", None, "")?; // scaffold template
    }
    let (mut openrouter_key, mut openai_key, mut opencode_key, codex, active_backend) =
        load_config_all().unwrap_or_default();
    if openrouter_key.is_empty()
        && let Ok(v) = std::env::var(OPENROUTER_ENV_KEY)
        && !v.trim().is_empty()
    {
        openrouter_key = v.trim().to_string();
    }
    if openai_key.is_empty()
        && let Ok(v) = std::env::var(OPENAI_ENV_KEY)
        && !v.trim().is_empty()
    {
        openai_key = v.trim().to_string();
    }
    if opencode_key.is_empty()
        && let Ok(v) = std::env::var(OPENCODE_ENV_KEY)
        && !v.trim().is_empty()
    {
        opencode_key = v.trim().to_string();
    }
    let codex = match codex {
        Some(creds) => {
            let creds = refresh_codex_if_needed(creds).await?;
            save_codex_credentials(&creds)?;
            Some(creds)
        }
        None => None,
    };
    Ok(SavedCreds {
        openrouter_key: (!openrouter_key.is_empty()).then_some(openrouter_key),
        openai_key: (!openai_key.is_empty()).then_some(openai_key),
        opencode_key: (!opencode_key.is_empty()).then_some(opencode_key),
        codex,
        active_backend: (!active_backend.is_empty()).then_some(active_backend),
    })
}

pub fn codex_account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .context("invalid Codex access token")?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .context("decoding Codex access token")?;
    let v: serde_json::Value = serde_json::from_slice(&payload).context("parsing Codex token")?;
    let account = v
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|a| a.as_str())
        .context("Codex token missing ChatGPT account id")?;
    Ok(account.to_string())
}

async fn refresh_codex_if_needed(creds: CodexCredentials) -> Result<CodexCredentials> {
    if chrono::Utc::now().timestamp_millis() < creds.expires - 60_000 {
        return Ok(creds);
    }
    let resp = reqwest::Client::new()
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", creds.refresh.as_str()),
            ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
        ])
        .send()
        .await
        .context("refreshing OpenAI Codex token")?
        .error_for_status()
        .context("OpenAI Codex token refresh failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex token refresh")?;
    let access = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("missing access_token")?
        .to_string();
    let refresh = resp
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or(&creds.refresh)
        .to_string();
    let expires_in = resp
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .context("missing expires_in")?;
    Ok(CodexCredentials {
        account_id: codex_account_id(&access)?,
        access,
        refresh,
        expires: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    })
}

pub fn save_codex_credentials(creds: &CodexCredentials) -> Result<()> {
    let (openrouter_key, openai_key, opencode_key, _, active_backend) =
        load_config_all().unwrap_or_default();
    write_provider_config(
        &openrouter_key,
        &openai_key,
        &opencode_key,
        Some(creds),
        &active_backend,
    )
}

pub fn load_openrouter_key_only() -> Option<String> {
    if let Ok(v) = std::env::var(OPENROUTER_ENV_KEY) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    load_config_all()
        .ok()
        .and_then(|(openrouter_key, ..)| (!openrouter_key.is_empty()).then_some(openrouter_key))
}

/// Persist a provider's key by explicit flavor tag ("openrouter" / "openai"
/// / "opencode"), and mark it the active backend — called only from the
/// `/login` provider selector, which always knows exactly which flavor a
/// pasted key is for (no shape-sniffing).
pub fn save_provider_key(flavor: &str, key: &str) -> Result<()> {
    let (mut openrouter_key, mut openai_key, mut opencode_key, codex, _) =
        load_config_all().unwrap_or_default();
    match flavor {
        "openrouter" => openrouter_key = key.to_string(),
        "openai" => openai_key = key.to_string(),
        "opencode" => opencode_key = key.to_string(),
        _ => {}
    }
    write_provider_config(
        &openrouter_key,
        &openai_key,
        &opencode_key,
        codex.as_ref(),
        flavor,
    )
}

/// Persist which backend is now active, without disturbing any saved keys
/// or Codex login. Called by `/login` and `/backend`.
pub fn save_active_backend(tag: &str) -> Result<()> {
    let (openrouter_key, openai_key, opencode_key, codex, _) =
        load_config_all().unwrap_or_default();
    write_provider_config(
        &openrouter_key,
        &openai_key,
        &opencode_key,
        codex.as_ref(),
        tag,
    )
}

/// The key/token to boot the app's initial provider with: whichever backend
/// was last active, if its credential is still there, else a fixed priority
/// (openrouter > openai > opencode > codex). Returns the backend's tag too
/// (not just the key/token) — needed since OpenAI and OpenCode Go keys look
/// identical by shape, so the caller can't tell them apart on its own.
pub fn resolve_initial_backend(saved: &SavedCreds) -> Option<(&'static str, String)> {
    let try_tag = |tag: &str| -> Option<(&'static str, String)> {
        match tag {
            "openrouter" => saved.openrouter_key.clone().map(|k| ("openrouter", k)),
            "openai" => saved.openai_key.clone().map(|k| ("openai", k)),
            "opencode" => saved.opencode_key.clone().map(|k| ("opencode", k)),
            "codex" => saved.codex.as_ref().map(|c| ("codex", c.access.clone())),
            _ => None,
        }
    };
    saved
        .active_backend
        .as_deref()
        .and_then(try_tag)
        .or_else(|| try_tag("openrouter"))
        .or_else(|| try_tag("openai"))
        .or_else(|| try_tag("opencode"))
        .or_else(|| try_tag("codex"))
}

/// Read all credential fields (and the last-active-backend tag) straight
/// off disk, no env overrides.
fn load_config_all() -> Result<(String, String, String, Option<CodexCredentials>, String)> {
    let path = config_path()?;
    if !path.exists() {
        return Ok((
            String::new(),
            String::new(),
            String::new(),
            None,
            String::new(),
        ));
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok((
        cfg.provider.openrouter_key,
        cfg.provider.openai_key,
        cfg.provider.opencode_key,
        cfg.provider.openai_codex,
        cfg.provider.active_backend,
    ))
}

pub async fn login_openai_codex_device(
    status: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<CodexCredentials> {
    let client = reqwest::Client::new();
    let device = client
        .post("https://auth.openai.com/api/accounts/deviceauth/usercode")
        .json(&serde_json::json!({ "client_id": "app_EMoamEEZ73f0CkXaXp7hrann" }))
        .send()
        .await
        .context("starting OpenAI Codex device login")?
        .error_for_status()
        .context("OpenAI Codex device login failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex device login")?;
    let device_auth_id = device
        .get("device_auth_id")
        .and_then(|v| v.as_str())
        .context("missing device_auth_id")?
        .to_string();
    let user_code = device
        .get("user_code")
        .and_then(|v| v.as_str())
        .context("missing user_code")?
        .to_string();
    let interval = device
        .get("interval")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok()))
        .unwrap_or(5.0)
        .max(1.0);
    let url = "https://auth.openai.com/codex/device";
    let prefilled_url = format!("{url}?user_code={user_code}");
    // Put only the raw code first so it stays visible even on narrow status lines.
    let _ =
        arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(user_code.clone()));
    let _ = status.send(format!(
        "{user_code}  ← copied to clipboard; enter at {url}"
    ));
    let _ = open::that(&prefilled_url);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    let code = loop {
        if std::time::Instant::now() >= deadline {
            bail!("OpenAI Codex device login timed out");
        }
        tokio::time::sleep(std::time::Duration::from_secs_f64(interval)).await;
        let resp = client
            .post("https://auth.openai.com/api/accounts/deviceauth/token")
            .json(&serde_json::json!({ "device_auth_id": device_auth_id, "user_code": user_code }))
            .send()
            .await
            .context("polling OpenAI Codex device login")?;
        if resp.status().is_success() {
            let v = resp
                .json::<serde_json::Value>()
                .await
                .context("parsing OpenAI Codex device token")?;
            let authorization_code = v
                .get("authorization_code")
                .and_then(|v| v.as_str())
                .context("missing authorization_code")?
                .to_string();
            let code_verifier = v
                .get("code_verifier")
                .and_then(|v| v.as_str())
                .context("missing code_verifier")?
                .to_string();
            break (authorization_code, code_verifier);
        }
        if resp.status().as_u16() != 403 && resp.status().as_u16() != 404 {
            let status_code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("OpenAI Codex device login failed ({status_code}): {body}");
        }
    };

    let token = client
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
            ("code", code.0.as_str()),
            ("code_verifier", code.1.as_str()),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
        ])
        .send()
        .await
        .context("exchanging OpenAI Codex device code")?
        .error_for_status()
        .context("OpenAI Codex device code exchange failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex token")?;
    let access = token
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("missing access_token")?
        .to_string();
    let refresh = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .context("missing refresh_token")?
        .to_string();
    let expires_in = token
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .context("missing expires_in")?;
    let creds = CodexCredentials {
        account_id: codex_account_id(&access)?,
        access,
        refresh,
        expires: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    };
    save_codex_credentials(&creds)?;
    Ok(creds)
}

fn write_provider_config(
    openrouter_key: &str,
    openai_key: &str,
    opencode_key: &str,
    codex: Option<&CodexCredentials>,
    active_backend: &str,
) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut body = format!(
        "[provider]\n\
         # OpenRouter key (or set ${OPENROUTER_ENV_KEY})\nopenrouter_key = \"{}\"\n\
         # OpenAI API key (or set ${OPENAI_ENV_KEY})\nopenai_key = \"{}\"\n\
         # OpenCode Go key (or set ${OPENCODE_ENV_KEY})\nopencode_key = \"{}\"\n\
         # Last active backend (\"openrouter\" / \"openai\" / \"opencode\" / \"codex\")\n\
         active_backend = \"{}\"\n",
        escape(openrouter_key),
        escape(openai_key),
        escape(opencode_key),
        escape(active_backend),
    );
    if let Some(c) = codex {
        body.push_str("\n[provider.openai_codex]\n");
        body.push_str(&format!("access = \"{}\"\n", escape(&c.access)));
        body.push_str(&format!("refresh = \"{}\"\n", escape(&c.refresh)));
        body.push_str(&format!("expires = {}\n", c.expires));
        body.push_str(&format!("account_id = \"{}\"\n", escape(&c.account_id)));
    }
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        let cfg: Config = toml::from_str(
            "[provider]\nopenrouter_key = \"sk-or-abc\"\nopenai_key = \"sk-proj-abc\"\n",
        )
        .unwrap();
        assert_eq!(cfg.provider.openrouter_key, "sk-or-abc");
        assert_eq!(cfg.provider.openai_key, "sk-proj-abc");
    }

    #[test]
    fn missing_keys_default_empty() {
        let cfg: Config = toml::from_str("[provider]\n").unwrap();
        assert!(cfg.provider.openrouter_key.is_empty());
        assert!(cfg.provider.openai_key.is_empty());
        assert!(cfg.provider.active_backend.is_empty());
    }

    #[test]
    fn parses_active_backend() {
        let cfg: Config = toml::from_str("[provider]\nactive_backend = \"codex\"\n").unwrap();
        assert_eq!(cfg.provider.active_backend, "codex");
    }

    fn codex_creds(access: &str) -> CodexCredentials {
        CodexCredentials {
            access: access.to_string(),
            refresh: "r".to_string(),
            expires: 0,
            account_id: "a".to_string(),
        }
    }

    #[test]
    fn resolve_initial_backend_prefers_the_recorded_active_backend() {
        // Both an OpenRouter key and Codex creds are present, but Codex was
        // the one last explicitly chosen — it should win, not the fixed
        // openrouter-first priority.
        let saved = SavedCreds {
            openrouter_key: Some("sk-or-abc".into()),
            openai_key: None,
            opencode_key: None,
            codex: Some(codex_creds("codex-token")),
            active_backend: Some("codex".to_string()),
        };
        assert_eq!(
            resolve_initial_backend(&saved),
            Some(("codex", "codex-token".to_string()))
        );
    }

    #[test]
    fn resolve_initial_backend_falls_back_to_priority_when_recorded_backend_is_gone() {
        // active_backend says "codex", but the codex login was revoked/removed.
        let saved = SavedCreds {
            openrouter_key: Some("sk-or-abc".into()),
            openai_key: None,
            opencode_key: None,
            codex: None,
            active_backend: Some("codex".to_string()),
        };
        assert_eq!(
            resolve_initial_backend(&saved),
            Some(("openrouter", "sk-or-abc".to_string()))
        );
    }

    #[test]
    fn resolve_initial_backend_falls_back_to_priority_when_never_recorded() {
        let saved = SavedCreds {
            openrouter_key: None,
            openai_key: Some("sk-proj-abc".into()),
            opencode_key: None,
            codex: Some(codex_creds("codex-token")),
            active_backend: None,
        };
        // openai comes before opencode/codex in the fixed priority.
        assert_eq!(
            resolve_initial_backend(&saved),
            Some(("openai", "sk-proj-abc".to_string()))
        );
    }

    #[test]
    fn resolve_initial_backend_prefers_recorded_opencode_backend() {
        let saved = SavedCreds {
            openrouter_key: Some("sk-or-abc".into()),
            openai_key: None,
            opencode_key: Some("oc-token".into()),
            codex: None,
            active_backend: Some("opencode".to_string()),
        };
        assert_eq!(
            resolve_initial_backend(&saved),
            Some(("opencode", "oc-token".to_string()))
        );
    }
}
