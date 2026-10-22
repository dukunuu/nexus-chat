//! Bring MCP servers over from the tools people already use: Claude Desktop
//! (`claude_desktop_config.json`), Claude Code (`~/.claude.json` and a
//! project `.mcp.json`) and Cursor (`~/.cursor/mcp.json`). Each entry
//! becomes a proposed connector; secrets found in `env` and `headers` are
//! carried separately so they go to the keychain, never into `config.toml`.
//! Paths are injectable so tests read fixture files.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::config::{self, AuthKind, ConnectorConfig};
use super::registry::Registry;
use super::secrets::KEYCHAIN_PREFIX;

/// Which tool a proposal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
}

impl Source {
    /// A short label for lists.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }

    /// Parse a CLI/command word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "claude-desktop" | "desktop" | "claude_desktop" => Some(Self::ClaudeDesktop),
            "claude-code" | "code" | "claude_code" | "claude" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }
}

/// Where to look.
#[derive(Debug, Clone, Default)]
pub struct ImportPaths {
    pub claude_desktop: Option<PathBuf>,
    pub claude_code_user: Option<PathBuf>,
    pub claude_code_project: Option<PathBuf>,
    pub cursor: Option<PathBuf>,
}

impl ImportPaths {
    /// The real locations on this machine; `project_dir` is where to look
    /// for a `.mcp.json` (the working directory).
    #[must_use]
    pub fn system(project_dir: Option<&Path>) -> Self {
        let base = directories::BaseDirs::new();
        let home = base.as_ref().map(|dirs| dirs.home_dir().to_path_buf());
        Self {
            // macOS: ~/Library/Application Support; Windows: %APPDATA%;
            // Linux: ~/.config.
            claude_desktop: base.as_ref().map(|dirs| {
                dirs.config_dir()
                    .join("Claude")
                    .join("claude_desktop_config.json")
            }),
            claude_code_user: home.as_ref().map(|home| home.join(".claude.json")),
            claude_code_project: project_dir.map(|dir| dir.join(".mcp.json")),
            cursor: home.map(|home| home.join(".cursor").join("mcp.json")),
        }
    }
}

/// One server another tool has configured, ready to add.
#[derive(Clone)]
pub struct Proposal {
    pub source: Source,
    pub origin: PathBuf,
    pub name: String,
    pub config: ConnectorConfig,
    /// `(kind, value)` pairs for the keychain; `config` refers to them.
    pub secrets: Vec<(String, String)>,
    /// A connector with this target is already configured.
    pub already: bool,
    pub notes: Vec<String>,
}

impl fmt::Debug for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proposal")
            .field("source", &self.source)
            .field("name", &self.name)
            .field("config", &self.config)
            .field(
                "secrets",
                &self
                    .secrets
                    .iter()
                    .map(|(kind, _)| kind)
                    .collect::<Vec<_>>(),
            )
            .field("already", &self.already)
            .finish_non_exhaustive()
    }
}

/// Everything found, plus what could not be used.
#[derive(Debug, Default)]
pub struct Discovery {
    pub proposals: Vec<Proposal>,
    /// "source name: reason".
    pub skipped: Vec<String>,
}

fn looks_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "auth",
        "credential",
        "cookie",
        "session",
    ]
    .iter()
    .any(|word| key.contains(word))
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Turn one `mcpServers` entry into a proposal.
fn parse_entry(
    source: Source,
    origin: &Path,
    name: &str,
    entry: &Value,
) -> Result<Proposal, String> {
    let kind = entry.get("type").and_then(Value::as_str);
    if kind == Some("sse") {
        return Err("uses the legacy SSE transport, which nexus does not support".into());
    }
    let mut notes = Vec::new();
    let mut secrets: Vec<(String, String)> = Vec::new();
    let url = entry.get("url").and_then(Value::as_str);
    let command = entry.get("command").and_then(Value::as_str);
    let mut connector = match (url, command) {
        (Some(url), None) => {
            config::validate_url(url).map_err(|error| error.to_string())?;
            ConnectorConfig::remote(url)
        }
        (None, Some(command)) if !command.trim().is_empty() => {
            let mut argv = vec![command.to_string()];
            argv.extend(
                entry
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|arg| arg.as_str().map(str::to_string)),
            );
            ConnectorConfig::local(argv)
        }
        (Some(_), Some(_)) => return Err("has both a url and a command".into()),
        _ => return Err("has neither a url nor a command".into()),
    };
    for (key, value) in string_map(entry.get("env")) {
        if value.contains("${") {
            notes.push(format!("{key} uses variable expansion · set it by hand"));
        }
        if looks_secret(&key) && !value.is_empty() {
            let kind = format!("env-{key}");
            secrets.push((kind.clone(), value));
            connector
                .env
                .insert(key, format!("{KEYCHAIN_PREFIX}{kind}"));
        } else {
            connector.env.insert(key, value);
        }
    }
    for (key, value) in string_map(entry.get("headers")) {
        if key.eq_ignore_ascii_case("authorization")
            && let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        {
            connector.auth = AuthKind::Bearer;
            secrets.push(("bearer".to_string(), token.trim().to_string()));
        } else if looks_secret(&key) && !value.is_empty() {
            let kind = format!("header-{}", key.to_ascii_lowercase());
            secrets.push((kind.clone(), value));
            connector
                .headers
                .insert(key, format!("{KEYCHAIN_PREFIX}{kind}"));
        } else {
            connector.headers.insert(key, value);
        }
    }
    if connector.url.is_none() && !connector.headers.is_empty() {
        connector.headers.clear();
        notes.push("headers are only used by remote servers".into());
    }
    Ok(Proposal {
        source,
        origin: origin.to_path_buf(),
        name: config::slug(name),
        config: connector,
        secrets,
        already: false,
        notes,
    })
}

fn read_servers(path: &Path) -> Result<Vec<(String, Value)>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let root: Value = serde_json::from_str(&text).map_err(|_| "is not valid JSON".to_string())?;
    let mut servers = Vec::new();
    let mut take = |value: Option<&Value>, prefix: &str| {
        if let Some(object) = value.and_then(Value::as_object) {
            for (name, entry) in object {
                servers.push((format!("{prefix}{name}"), entry.clone()));
            }
        }
    };
    take(root.get("mcpServers"), "");
    // Claude Code keeps per-project servers under projects.<path>.mcpServers.
    // Only the user-level and the current project's `.mcp.json` are imported;
    // the per-project blocks are skipped on purpose (they belong to other
    // directories).
    Ok(servers)
}

/// Read every known location. `existing` is the set of connectors already
/// configured as `(name, target-key)` so repeats are recognised.
#[must_use]
pub fn discover(paths: &ImportPaths, existing: &BTreeMap<String, ConnectorConfig>) -> Discovery {
    let mut found = Discovery::default();
    let mut taken: HashSet<String> = existing.keys().cloned().collect();
    let key_of = |config: &ConnectorConfig| match &config.url {
        Some(url) => format!("url:{url}"),
        None => format!("cmd:{}", config.command.join("\u{1f}")),
    };
    let existing_targets: HashSet<String> = existing.values().map(key_of).collect();
    let mut seen_targets: HashSet<String> = HashSet::new();
    let locations = [
        (Source::ClaudeDesktop, paths.claude_desktop.as_ref()),
        (Source::ClaudeCode, paths.claude_code_user.as_ref()),
        (Source::ClaudeCode, paths.claude_code_project.as_ref()),
        (Source::Cursor, paths.cursor.as_ref()),
    ];
    for (source, path) in locations {
        let Some(path) = path.filter(|path| path.is_file()) else {
            continue;
        };
        let servers = match read_servers(path) {
            Ok(servers) => servers,
            Err(why) => {
                found
                    .skipped
                    .push(format!("{} {}: {why}", source.label(), path.display()));
                continue;
            }
        };
        for (name, entry) in servers {
            let mut proposal = match parse_entry(source, path, &name, &entry) {
                Ok(proposal) => proposal,
                Err(why) => {
                    found
                        .skipped
                        .push(format!("{} {name}: {why}", source.label()));
                    continue;
                }
            };
            let target = key_of(&proposal.config);
            if !seen_targets.insert(target.clone()) {
                continue;
            }
            proposal.already = existing_targets.contains(&target);
            if !proposal.already {
                let base = proposal.name.clone();
                let mut counter = 2;
                while taken.contains(&proposal.name) {
                    let suffix = format!("-{counter}");
                    let keep = 32 - suffix.len();
                    proposal.name =
                        format!("{}{suffix}", base.chars().take(keep).collect::<String>());
                    counter += 1;
                }
                taken.insert(proposal.name.clone());
            }
            found.proposals.push(proposal);
        }
    }
    found
}

/// Add the chosen proposals (names empty = all not already present).
/// Returns the names added; secrets go to the keychain first-class.
///
/// # Errors
/// The first connector that could not be added (earlier ones stay added).
pub fn apply(
    registry: &Registry,
    proposals: &[Proposal],
    names: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut added = Vec::new();
    for proposal in proposals {
        if proposal.already || (!names.is_empty() && !names.contains(&proposal.name)) {
            continue;
        }
        registry.add_with_secrets(&proposal.name, proposal.config.clone(), &proposal.secrets)?;
        added.push(proposal.name.clone());
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::secrets::{MemoryStore, Vault};
    use std::sync::Arc;

    fn fixtures() -> (PathBuf, ImportPaths) {
        let dir = std::env::temp_dir().join(format!("nexus-conn-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let write = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            path
        };
        let desktop = write(
            "desktop.json",
            r#"{"mcpServers": {
                "Filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me"], "env": {"LOG_LEVEL": "warn", "API_KEY": "sk-desktop-secret"}},
                "linear": {"url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer lin_tok_123", "X-Team": "eng"}}
            }}"#,
        );
        let code = write(
            "claude.json",
            r#"{"mcpServers": {
                "legacy": {"type": "sse", "url": "https://old.example/sse"},
                "gh": {"type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": {"X-Api-Key": "ghp_secret"}},
                "linear-again": {"url": "https://mcp.linear.app/mcp"},
                "broken": {"nothing": true},
                "plain": {"url": "http://example.com/mcp"}
            }, "projects": {"/elsewhere": {"mcpServers": {"nope": {"command": "x"}}}}}"#,
        );
        let project = write(
            ".mcp.json",
            r#"{"mcpServers": {"local": {"command": "python3", "args": ["srv.py"]}}}"#,
        );
        let cursor = write(
            "cursor.json",
            r#"{"mcpServers": {"Filesystem": {"command": "other", "args": []}}}"#,
        );
        let paths = ImportPaths {
            claude_desktop: Some(desktop),
            claude_code_user: Some(code),
            claude_code_project: Some(project),
            cursor: Some(cursor),
        };
        (dir, paths)
    }

    #[test]
    fn entries_from_every_tool_become_proposals_with_secrets_split_out() {
        let (dir, paths) = fixtures();
        let found = discover(&paths, &BTreeMap::new());
        let names: Vec<&str> = found.proposals.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["filesystem", "linear", "gh", "local", "filesystem-2"]
        );
        let fs = &found.proposals[0];
        assert_eq!(fs.config.command[0], "npx");
        assert_eq!(fs.config.env["LOG_LEVEL"], "warn");
        assert_eq!(fs.config.env["API_KEY"], "keychain:env-API_KEY");
        assert_eq!(
            fs.secrets,
            [("env-API_KEY".to_string(), "sk-desktop-secret".to_string())]
        );
        let linear = &found.proposals[1];
        assert_eq!(linear.config.auth, AuthKind::Bearer);
        assert_eq!(
            linear.secrets,
            [("bearer".to_string(), "lin_tok_123".to_string())]
        );
        assert_eq!(linear.config.headers["X-Team"], "eng");
        let gh = &found.proposals[2];
        assert_eq!(gh.config.headers["X-Api-Key"], "keychain:header-x-api-key");
        // The duplicate target, the SSE server, the empty entry and the plain-http
        // server are skipped (the duplicate silently, the rest with reasons).
        let skipped = found.skipped.join("\n");
        assert!(skipped.contains("legacy SSE"), "{skipped}");
        assert!(skipped.contains("neither a url nor a command"), "{skipped}");
        assert!(skipped.contains("plain http"), "{skipped}");
        assert!(!found.proposals.iter().any(|p| p.name == "nope"));
        // Nothing secret survives in the config values or in Debug output.
        for proposal in &found.proposals {
            let shown = format!("{proposal:?} {:?}", proposal.config);
            assert!(!shown.contains("sk-desktop"), "{shown}");
            assert!(!shown.contains("ghp_secret"), "{shown}");
            assert!(!shown.contains("lin_tok"), "{shown}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn already_configured_targets_are_recognised() {
        let (dir, paths) = fixtures();
        let mut existing = BTreeMap::new();
        existing.insert(
            "lin".to_string(),
            ConnectorConfig::remote("https://mcp.linear.app/mcp"),
        );
        let found = discover(&paths, &existing);
        assert!(
            found
                .proposals
                .iter()
                .find(|p| p.name == "linear")
                .unwrap()
                .already
        );
        assert!(
            !found
                .proposals
                .iter()
                .find(|p| p.name == "gh")
                .unwrap()
                .already
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_or_corrupt_files_are_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("nexus-conn-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        let paths = ImportPaths {
            claude_desktop: Some(bad),
            cursor: Some(dir.join("absent.json")),
            ..ImportPaths::default()
        };
        let found = discover(&paths, &BTreeMap::new());
        assert!(found.proposals.is_empty());
        assert_eq!(found.skipped.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn applying_moves_secrets_into_the_keychain_and_keeps_them_out_of_config() {
        let (dir, paths) = fixtures();
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "[provider]\nopenrouter_key = \"keep\"\n").unwrap();
        let store = Arc::new(MemoryStore::default());
        let registry = Registry::new(crate::connectors::registry::Env {
            config_path: Some(config_path.clone()),
            logs_dir: None,
            vault: Arc::new(Vault::new(store.clone(), Some(config_path.clone()), None)),
            notes: None,
            tuning: crate::connectors::registry::Tuning::default(),
            autostart: false,
        });
        let found = discover(&paths, &BTreeMap::new());
        let added = apply(
            &registry,
            &found.proposals,
            &["linear".to_string(), "filesystem".to_string()],
        )
        .unwrap();
        assert_eq!(added, ["filesystem", "linear"]);
        assert_eq!(
            store.accounts(),
            [
                "connector:filesystem:env-API_KEY",
                "connector:linear:bearer"
            ]
        );
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("keep"), "{text}");
        assert!(text.contains("keychain:env-API_KEY"), "{text}");
        assert!(!text.contains("sk-desktop-secret"), "{text}");
        assert!(!text.contains("lin_tok_123"), "{text}");
        let loaded = config::load_at(&config_path).unwrap();
        assert_eq!(loaded.connectors.len(), 2);
        // Re-running recognises what is there.
        let again = discover(&paths, &loaded.connectors);
        assert!(again.proposals.iter().filter(|p| p.already).count() >= 2);
        assert!(
            apply(&registry, &again.proposals, &[])
                .unwrap()
                .iter()
                .all(|n| n != "linear")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
