use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const OPENROUTER_ENV_KEY: &str = "OPENROUTER_API_KEY";
pub const OPENAI_ENV_KEY: &str = "OPENAI_API_KEY";
pub const OPENCODE_ENV_KEY: &str = "OPENCODE_API_KEY";

/// Credential environment variables never passed to children that run model-authored code.
pub const SECRET_ENV_VARS: &[&str] = &[
    OPENROUTER_ENV_KEY,
    OPENAI_ENV_KEY,
    OPENCODE_ENV_KEY,
    "CF_API_TOKEN",
];

/// Bundled base instructions, always used directly from the binary.
pub const BASE_SYSTEM_PROMPT: &str = include_str!("../assets/prompts/base.md");

// SHA-256 of every base prompt nexus ever copied into system_prompt.md.
// This list is frozen because nexus no longer copies it.
const SHIPPED_DEFAULT_SHA256: &[&str] = &[
    "17219b05c779dc4051d0fc2baf9440d658b82f22efbf6f8415a8c0892eeb75f9",
    "5a3b0d47f71d8aa2882a31cf3a96ac28afb2a8e285c6023ebb430ba41c4c6a0e",
];

/// Remove credential environment variables from children that run model-authored code.
pub fn scrub_secret_env(cmd: &mut std::process::Command) {
    for key in SECRET_ENV_VARS {
        cmd.env_remove(key);
    }
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    provider: Provider,
    #[serde(default)]
    host: Option<HostSettings>,
    #[serde(default)]
    local: Option<crate::provider::local::LocalConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct Provider {
    #[serde(default)]
    openrouter_key: String,
    #[serde(default)]
    openai_key: String,
    #[serde(default)]
    opencode_key: String,
    #[serde(default)]
    host_token: Option<String>,
    #[serde(default)]
    openai_codex: Option<CodexCredentials>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CodexCredentials {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub account_id: String,
}

/// Persisted, non-secret configuration for a named Cloudflare tunnel.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NamedTunnelConfig {
    pub tunnel_id: String,
    pub hostname: String,
    pub credentials_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct HostSettings {
    #[serde(default)]
    named_tunnel: Option<NamedTunnelConfig>,
}

/// Every credential the app knows about at once — every configured backend
/// is simultaneously usable (`/model` merges all of their catalogs).
#[derive(Debug, Clone, Default)]
pub struct SavedCreds {
    /// Independent local runtime configuration, never synced between machines.
    pub local: Option<crate::provider::local::LocalConfig>,
    pub openrouter_key: Option<String>,
    pub openai_key: Option<String>,
    pub opencode_key: Option<String>,
    pub codex: Option<CodexCredentials>,
    /// Bearer token protecting the local host API, persisted under
    /// `[provider].host_token` and never synced with space data.
    pub host_token: Option<String>,
}

/// Write a file that holds credentials, restricting it to the owner.
///
/// `config.toml` carries provider API keys, Codex OAuth tokens, and the host
/// bearer token. `std::fs::write` would leave it at the process umask
/// (commonly world-readable), so the mode is tightened the same way
/// `host/cloudflare.rs` protects tunnel credentials.
fn write_secret_file(path: &std::path::Path, body: &str) -> Result<()> {
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    restrict_to_owner(path);
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) {}

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

/// Optional user additions to the bundled base prompt. Lives beside config.toml,
/// not per-space — this is app-level, not chat-level. Editable via `$EDITOR`.
pub fn system_prompt_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("system_prompt.md"))
}

/// Read optional user additions without creating or writing the file.
pub fn load_system_prompt_overlay() -> String {
    system_prompt_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim_end().to_string())
        .unwrap_or_default()
}

/// Remove unedited shipped defaults, or warn when an edited copy remains.
pub fn migrate_system_prompt_overlay() -> Option<String> {
    migrate_overlay_at(&system_prompt_path().ok()?)
}

fn migrate_overlay_at(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hash = String::new();
    for b in Sha256::digest(&bytes) {
        let _ = write!(hash, "{b:02x}");
    }
    if SHIPPED_DEFAULT_SHA256.contains(&hash.as_str()) {
        let _ = std::fs::remove_file(path);
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    text.contains("You are the assistant inside nexus-chat").then(|| {
        "system_prompt.md now holds only your additions to the built-in prompt, and yours still contains a copy of the old built-in text; trim it to your own lines (Settings, Ctrl+E)".into()
    })
}

/// Resolve every configured credential from disk + env, with no network
/// access: Codex creds are returned exactly as stored (no refresh). Read-only
/// CLI commands (`status`, `usage`, `sessions`, …) use this so they never
/// touch the network; the TUI and `nexus ask` use `load_all_providers`,
/// which additionally refreshes a stale Codex token.
pub fn load_creds_offline() -> SavedCreds {
    let (mut openrouter_key, mut openai_key, mut opencode_key, codex) =
        load_config_all().unwrap_or_default();
    let host_token = load_host_token().ok().flatten();
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
    SavedCreds {
        local: load_local_config().ok().flatten(),
        openrouter_key: (!openrouter_key.is_empty()).then_some(openrouter_key),
        openai_key: (!openai_key.is_empty()).then_some(openai_key),
        opencode_key: (!opencode_key.is_empty()).then_some(opencode_key),
        codex,
        host_token,
    }
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
        write_provider_config("", "", "", None)?; // scaffold template
    }
    let mut saved = load_creds_offline();
    saved.local = load_local_config()?;
    if let Some(local) = &saved.local {
        local.validate()?;
    }
    if let Some(creds) = saved.codex.take() {
        let creds = refresh_codex_if_needed(creds).await?;
        save_codex_credentials(&creds)?;
        saved.codex = Some(creds);
    }
    Ok(saved)
}

fn load_local_config() -> Result<Option<crate::provider::local::LocalConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config = toml::from_str(&text).context("parsing local runtime configuration")?;
    Ok(config.local)
}

/// The first configured credential in a fixed priority order (openrouter >
/// openai > opencode > codex) — used only to seed `App::new`'s "reasonable
/// defaults" guess at startup; `App::rebuild_all_backends` populates every
/// configured backend regardless of which one this picks.
pub fn first_configured(saved: &SavedCreds) -> Option<(&'static str, String)> {
    saved
        .openrouter_key
        .clone()
        .map(|k| ("openrouter", k))
        .or_else(|| saved.openai_key.clone().map(|k| ("openai", k)))
        .or_else(|| saved.opencode_key.clone().map(|k| ("opencode", k)))
        .or_else(|| saved.codex.as_ref().map(|c| ("codex", c.access.clone())))
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
        .and_then(serde_json::Value::as_i64)
        .context("missing expires_in")?;
    Ok(CodexCredentials {
        account_id: codex_account_id(&access)?,
        access,
        refresh,
        expires: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    })
}

pub fn save_codex_credentials(creds: &CodexCredentials) -> Result<()> {
    let (openrouter_key, openai_key, opencode_key, _) = load_config_all().unwrap_or_default();
    write_provider_config(&openrouter_key, &openai_key, &opencode_key, Some(creds))
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
/// / "opencode") — called only from the `/login` provider selector, which
/// always knows exactly which flavor a pasted key is for (no shape-sniffing).
pub fn save_provider_key(flavor: &str, key: &str) -> Result<()> {
    let (mut openrouter_key, mut openai_key, mut opencode_key, codex) =
        load_config_all().unwrap_or_default();
    match flavor {
        "openrouter" => openrouter_key = key.to_string(),
        "openai" => openai_key = key.to_string(),
        "opencode" => opencode_key = key.to_string(),
        _ => {}
    }
    write_provider_config(&openrouter_key, &openai_key, &opencode_key, codex.as_ref())
}

/// Read all credential fields straight off disk, no env overrides.
fn load_config_all() -> Result<(String, String, String, Option<CodexCredentials>)> {
    let path = config_path()?;
    if !path.exists() {
        return Ok((String::new(), String::new(), String::new(), None));
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
    ))
}

/// Read the optional host bearer token without exposing it in provider
/// selection or diagnostics.
pub fn load_host_token() -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg
        .provider
        .host_token
        .filter(|token| !token.trim().is_empty()))
}

/// Persist the host bearer token in the existing provider table while keeping
/// all provider credentials and Codex fields intact.
pub fn save_host_token(token: &str) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = value
        .as_table_mut()
        .context("config root is not a TOML table")?;
    let provider = table
        .entry("provider".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let provider = provider
        .as_table_mut()
        .context("config provider is not a TOML table")?;
    provider.insert(
        "host_token".to_string(),
        toml::Value::String(token.to_string()),
    );
    let body = toml::to_string_pretty(&value).context("serializing config")?;
    write_secret_file(&path, &body)
}

/// Return the persisted token or create a random one on the first host run.
pub fn ensure_host_token() -> Result<String> {
    if let Some(token) = load_host_token()? {
        return Ok(token);
    }
    let token = uuid::Uuid::new_v4().to_string();
    save_host_token(&token)?;
    Ok(token)
}

/// Load a previously provisioned named tunnel without contacting Cloudflare.
pub fn load_named_tunnel() -> Result<Option<NamedTunnelConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg.host.and_then(|host| host.named_tunnel))
}

/// Persist named-tunnel identifiers and local config paths. Credentials stay
/// in cloudflared's private file; this table contains no bearer/API secret.
pub fn save_named_tunnel(tunnel: &NamedTunnelConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root = value
        .as_table_mut()
        .context("config root is not a TOML table")?;
    let host = root
        .entry("host".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("config host is not a TOML table")?;
    host.insert(
        "named_tunnel".to_string(),
        toml::Value::try_from(tunnel).context("serializing named tunnel config")?,
    );
    let body = toml::to_string_pretty(&value).context("serializing config")?;
    write_secret_file(&path, &body)
}

pub async fn login_openai_codex_device(
    status: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<CodexCredentials> {
    login_openai_codex_device_with_browser(status, true).await
}

/// Device login for a host client; does not launch a browser or touch the host clipboard.
pub async fn login_openai_codex_remote(
    status: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<CodexCredentials> {
    login_openai_codex_device_with_browser(status, false).await
}

// Long by design (device-flow state machine).
#[allow(clippy::too_many_lines)]
async fn login_openai_codex_device_with_browser(
    status: tokio::sync::mpsc::UnboundedSender<String>,
    local_browser: bool,
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
    if local_browser {
        let _ = arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(user_code.clone()));
        let _ = status.send(format!(
            "{user_code}  ← copied to clipboard; enter at {url}"
        ));
        let _ = open::that(&prefilled_url);
    } else {
        let _ = status.send(format!("device-code:{user_code}"));
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(15);
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
        .and_then(serde_json::Value::as_i64)
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

/// Persist (or clear) the machine-local `[local]` block, leaving every
/// credential in the file untouched.
///
/// # Errors
/// Propagates read/serialize/write failures on the config file.
pub fn save_local_config(local: Option<&crate::provider::local::LocalConfig>) -> Result<()> {
    let (openrouter_key, openai_key, opencode_key, codex) = load_config_all().unwrap_or_default();
    write_config(
        &openrouter_key,
        &openai_key,
        &opencode_key,
        codex.as_ref(),
        local,
    )
}

fn write_provider_config(
    openrouter_key: &str,
    openai_key: &str,
    opencode_key: &str,
    codex: Option<&CodexCredentials>,
) -> Result<()> {
    // Credential writers leave the local runtime block exactly as it is.
    let local = load_local_config()?;
    write_config(
        openrouter_key,
        openai_key,
        opencode_key,
        codex,
        local.as_ref(),
    )
}

fn write_config(
    openrouter_key: &str,
    openai_key: &str,
    opencode_key: &str,
    codex: Option<&CodexCredentials>,
    local: Option<&crate::provider::local::LocalConfig>,
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
         # OpenCode Go key (or set ${OPENCODE_ENV_KEY})\nopencode_key = \"{}\"\n",
        escape(openrouter_key),
        escape(openai_key),
        escape(opencode_key),
    );
    if let Ok(Some(token)) = load_host_token() {
        let _ = writeln!(body, "host_token = \"{}\"", escape(&token));
    }
    if let Some(c) = codex {
        body.push_str("\n[provider.openai_codex]\n");
        let _ = writeln!(body, "access = \"{}\"", escape(&c.access));
        let _ = writeln!(body, "refresh = \"{}\"", escape(&c.refresh));
        let _ = writeln!(body, "expires = {}", c.expires);
        let _ = writeln!(body, "account_id = \"{}\"", escape(&c.account_id));
    }
    if let Ok(Some(tunnel)) = load_named_tunnel() {
        body.push_str("\n[host.named_tunnel]\n");
        let _ = writeln!(body, "tunnel_id = \"{}\"", escape(&tunnel.tunnel_id));
        let _ = writeln!(body, "hostname = \"{}\"", escape(&tunnel.hostname));
        let _ = writeln!(
            body,
            "credentials_path = \"{}\"",
            escape(&tunnel.credentials_path.display().to_string())
        );
        let _ = writeln!(
            body,
            "config_path = \"{}\"",
            escape(&tunnel.config_path.display().to_string())
        );
    }
    if let Some(local) = local {
        body.push_str("\n[local]\n");
        body.push_str(&toml::to_string(local).context("serializing local runtime configuration")?);
    }
    write_secret_file(&path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_migration_deletes_every_shipped_default() {
        let root =
            std::env::temp_dir().join(format!("nexus-prompt-overlay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("system_prompt.md");
        for fixture in [
            include_str!("../tests/fixtures/prompt-defaults/2026-07-02.md"),
            include_str!("../tests/fixtures/prompt-defaults/2026-08-12.md"),
        ] {
            let mut hash = String::new();
            for b in Sha256::digest(fixture.as_bytes()) {
                let _ = write!(hash, "{b:02x}");
            }
            assert!(SHIPPED_DEFAULT_SHA256.contains(&hash.as_str()));
            std::fs::write(&path, fixture).unwrap();
            assert_eq!(migrate_overlay_at(&path), None);
            assert!(!path.exists());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overlay_migration_keeps_edited_copies_with_a_note() {
        let root =
            std::env::temp_dir().join(format!("nexus-prompt-overlay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("system_prompt.md");
        let edited = format!(
            "{}\nAlways answer in French.",
            include_str!("../tests/fixtures/prompt-defaults/2026-08-12.md")
        );
        std::fs::write(&path, &edited).unwrap();
        assert!(migrate_overlay_at(&path).is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overlay_migration_leaves_plain_additions_silent() {
        let root =
            std::env::temp_dir().join(format!("nexus-prompt-overlay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("system_prompt.md");
        let additions = "Always answer in French.";
        std::fs::write(&path, additions).unwrap();
        assert_eq!(migrate_overlay_at(&path), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), additions);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overlay_migration_never_creates_a_file() {
        let root =
            std::env::temp_dir().join(format!("nexus-prompt-overlay-{}", uuid::Uuid::new_v4()));
        let path = root.join("system_prompt.md");
        assert_eq!(migrate_overlay_at(&path), None);
        assert!(!path.exists());
        assert!(!root.exists());
    }

    #[test]
    fn scrub_secret_env_removes_every_secret() {
        let mut cmd = std::process::Command::new("env");
        scrub_secret_env(&mut cmd);
        for key in SECRET_ENV_VARS {
            assert!(
                cmd.get_envs()
                    .any(|entry| entry == (std::ffi::OsStr::new(key), None))
            );
        }
    }

    #[test]
    fn local_runtime_config_round_trips_alongside_cloud_credentials() {
        let cfg: Config = toml::from_str(
            "[provider]\nopenai_key = 'test-key'\n[local]\nprovider = 'mlx'\nlist_command = ['my-list', '--installed']\n",
        ).unwrap();
        assert_eq!(cfg.provider.openai_key, "test-key");
        let local = cfg.local.unwrap();
        local.validate().unwrap();
        let encoded = toml::to_string(&local).unwrap();
        let decoded: crate::provider::local::LocalConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.provider, local.provider);
        assert_eq!(decoded.list_command, local.list_command);
        assert_eq!(decoded.endpoint(), "http://localhost:8080/v1");
        assert!(toml::from_str::<Config>("[local]\nprovider = 'unknown'").is_err());
    }

    #[test]
    fn local_context_settings_parse_and_round_trip_with_defaults_and_overrides() {
        for (settings, context_length, kv_cache_q8) in [
            ("", None, false),
            (
                "context_length = 65536\nkv_cache_q8 = true\n",
                Some(65_536),
                true,
            ),
            (
                "context_length = 2048\nkv_cache_q8 = false\n",
                Some(2048),
                false,
            ),
        ] {
            let cfg: Config = toml::from_str(&format!(
                "[provider]\nopenai_key = 'test-key'\n[local]\nprovider = 'ollama'\n{settings}"
            ))
            .unwrap();
            assert_eq!(cfg.provider.openai_key, "test-key");
            let local = cfg.local.unwrap();
            local.validate().unwrap();
            assert_eq!(local.context_length, context_length);
            assert_eq!(local.kv_cache_q8, kv_cache_q8);
            let encoded = toml::to_string(&local).unwrap();
            assert_eq!(encoded.contains("context_length"), context_length.is_some());
            assert_eq!(encoded.contains("kv_cache_q8"), kv_cache_q8);
            let decoded: crate::provider::local::LocalConfig = toml::from_str(&encoded).unwrap();
            assert_eq!(decoded.context_length, context_length);
            assert_eq!(decoded.kv_cache_q8, kv_cache_q8);
        }
    }

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
    fn first_configured_follows_fixed_priority() {
        let saved = SavedCreds {
            openrouter_key: Some("sk-or-abc".into()),
            openai_key: Some("sk-proj-abc".into()),
            opencode_key: Some("oc-token".into()),
            codex: Some(codex_creds("codex-token")),
            ..Default::default()
        };
        assert_eq!(
            first_configured(&saved),
            Some(("openrouter", "sk-or-abc".to_string()))
        );
    }

    #[test]
    fn first_configured_falls_through_to_whatever_is_set() {
        let saved = SavedCreds {
            openrouter_key: None,
            openai_key: None,
            opencode_key: Some("oc-token".into()),
            codex: Some(codex_creds("codex-token")),
            ..Default::default()
        };
        assert_eq!(
            first_configured(&saved),
            Some(("opencode", "oc-token".to_string()))
        );
    }

    #[test]
    fn first_configured_none_when_nothing_saved() {
        assert_eq!(first_configured(&SavedCreds::default()), None);
    }
}
