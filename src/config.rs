use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;

pub const ENV_KEY: &str = "OPENROUTER_API_KEY";

const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../assets/system-prompt-base.md");

#[derive(Debug, Deserialize)]
struct Config {
    provider: Provider,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    openrouter_key: String,
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

/// Resolve the OpenRouter key: `$OPENROUTER_API_KEY` wins, else the config file.
/// Scaffolds an empty config on first run. Never fails on a missing key — the app
/// launches regardless and the key can be set in-app with `/key`.
pub fn load_key() -> Result<Option<String>> {
    if let Ok(v) = std::env::var(ENV_KEY) {
        let v = v.trim();
        if !v.is_empty() {
            return Ok(Some(v.to_string()));
        }
    }

    let path = config_path()?;
    if !path.exists() {
        save_key("")?; // scaffold template
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let key = cfg.provider.openrouter_key.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}

/// Persist the key to the config file (overwrites it).
pub fn save_key(key: &str) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    let body = format!(
        "[provider]\n# OpenRouter key (or set ${ENV_KEY})\nopenrouter_key = \"{escaped}\"\n"
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key() {
        let cfg: Config =
            toml::from_str("[provider]\nopenrouter_key = \"sk-or-abc\"\n").unwrap();
        assert_eq!(cfg.provider.openrouter_key, "sk-or-abc");
    }

    #[test]
    fn missing_key_defaults_empty() {
        let cfg: Config = toml::from_str("[provider]\n").unwrap();
        assert!(cfg.provider.openrouter_key.is_empty());
    }
}
