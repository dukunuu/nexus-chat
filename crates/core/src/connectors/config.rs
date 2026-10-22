//! The `[connectors.*]` section of `config.toml`, typed but forgiving.
//!
//! Reading never fails the whole file: a connector that does not parse is
//! reported and skipped while the others load. Writing edits the file in
//! place (`config::edit_config_at`), touching only the keys it owns, so
//! `[provider]`, `[agent]`, comments-free formatting aside, every other
//! section and any key a newer build added survives a round trip.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// What the user decided about one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Runs without asking (as long as its definition is unchanged).
    Allow,
    /// Always shows the approval panel.
    Ask,
    /// Hidden from the model and refused if called.
    Deny,
}

impl Policy {
    /// Parse the config word; unknown words are `None` (treated as unset).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// The config word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// How a remote connector authenticates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    #[default]
    None,
    /// A static bearer token held in the keychain.
    Bearer,
    /// OAuth (C2); stored and shown, not yet driven.
    Oauth,
}

/// One `[connectors.<name>]` table.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// A hosted server (Streamable HTTP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A local server: argv, never a shell string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub auth: AuthKind,
    /// Child environment (stdio). Values may be `keychain:<name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Extra request headers (HTTP). Values may be `keychain:<name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Per-tool policy words (`allow` | `ask` | `deny`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
    /// Definition hashes of the tools the user always allowed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_timeout_secs: Option<u64>,
    /// Secrets kept here only when no keychain is usable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
}

const fn enabled_by_default() -> bool {
    true
}

// Debug must never print env/header values or fallback secrets.
impl fmt::Debug for ConnectorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorConfig")
            .field("target", &self.target_label())
            .field("enabled", &self.enabled)
            .field("auth", &self.auth)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("tools", &self.tools)
            .field("secrets", &self.secrets.len())
            .finish_non_exhaustive()
    }
}

impl ConnectorConfig {
    /// A hosted connector.
    #[must_use]
    pub fn remote(url: &str) -> Self {
        Self {
            url: Some(url.trim().to_string()),
            enabled: true,
            ..Self::default()
        }
    }

    /// A local connector.
    #[must_use]
    pub fn local(command: Vec<String>) -> Self {
        Self {
            command,
            enabled: true,
            ..Self::default()
        }
    }

    /// The explicit policy for a tool, if the user set one.
    #[must_use]
    pub fn policy(&self, tool: &str) -> Option<Policy> {
        self.tools.get(tool).and_then(|word| Policy::parse(word))
    }

    /// Where it connects, with any query string or credentials removed:
    /// safe for status lines, logs and snapshots.
    #[must_use]
    pub fn target_label(&self) -> String {
        match &self.url {
            Some(url) => redact_url(url),
            None => self.command.first().cloned().unwrap_or_default(),
        }
    }

    /// Structural checks shared by every way a connector gets added.
    ///
    /// # Errors
    /// The connector has neither or both of `url` and `command`, a bad URL
    /// scheme, or an empty command.
    pub fn validate(&self) -> Result<()> {
        match (&self.url, self.command.is_empty()) {
            (Some(_), false) => bail!("a connector is either a url or a command, not both"),
            (None, true) => bail!("a connector needs a url or a command"),
            (Some(url), true) => validate_url(url)?,
            (None, false) => {
                if self.command[0].trim().is_empty() {
                    bail!("the command is empty");
                }
            }
        }
        Ok(())
    }
}

/// `https` anywhere, `http` only for loopback (local servers and tests).
///
/// # Errors
/// The URL does not parse, or uses plain `http` for a non-loopback host.
pub fn validate_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| anyhow::anyhow!("not a valid url"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&parsed) => Ok(()),
        "http" => bail!("plain http is only allowed for localhost · use an https url"),
        other => bail!("unsupported url scheme: {other}"),
    }
}

/// Whether a URL points at this machine.
#[must_use]
pub fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
}

/// The URL without credentials, query or fragment.
#[must_use]
pub fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => "(invalid url)".to_string(),
    }
}

/// `[a-z0-9_-]{1,32}`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A valid name derived from free text (an imported server key, a host).
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_ascii_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            c
        } else {
            '-'
        };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_matches(['-', '_']).replace("__", "_");
    let mut out: String = out.chars().take(32).collect();
    while out.ends_with(['-', '_']) {
        out.pop();
    }
    if out.is_empty() {
        "connector".to_string()
    } else {
        out
    }
}

/// A connector name suggested by what the user typed: the service in a URL
/// (`https://mcp.linear.app/mcp` -> `linear`) or the package/program in a
/// command line (`npx -y @modelcontextprotocol/server-filesystem` ->
/// `filesystem`).
#[must_use]
pub fn suggest_name(target: &str) -> String {
    let target = target.trim();
    if let Ok(url) = reqwest::Url::parse(target)
        && let Some(host) = url.host_str()
    {
        if host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
            || host == "localhost"
        {
            return slug(host);
        }
        let labels: Vec<&str> = host
            .split('.')
            .filter(|label| {
                !matches!(
                    *label,
                    "mcp" | "api" | "www" | "app" | "com" | "io" | "net" | "org" | "dev" | "ai"
                )
            })
            .collect();
        return slug(labels.first().copied().unwrap_or(host));
    }
    let words = super::split_argv(target).unwrap_or_default();
    let subject = words
        .iter()
        .skip(1)
        .rev()
        .find(|word| !word.starts_with('-') && !word.contains(' '))
        .or_else(|| words.first());
    let Some(subject) = subject else {
        return "connector".to_string();
    };
    let base = subject
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(subject);
    let base = base
        .strip_suffix(".py")
        .or_else(|| base.strip_suffix(".js"))
        .unwrap_or(base);
    let mut base = base;
    loop {
        let stripped = ["server-", "mcp-"]
            .iter()
            .find_map(|prefix| base.strip_prefix(prefix))
            .or_else(|| {
                ["-mcp", "-server"]
                    .iter()
                    .find_map(|suffix| base.strip_suffix(suffix))
            });
        match stripped {
            Some(rest) if !rest.is_empty() => base = rest,
            _ => break,
        }
    }
    slug(base)
}

/// What reading the config found.
#[derive(Debug, Default)]
pub struct Loaded {
    pub connectors: BTreeMap<String, ConnectorConfig>,
    /// One line per entry that could not be used.
    pub problems: Vec<String>,
}

/// Read every `[connectors.*]` table from the config file at `path`.
///
/// # Errors
/// The file exists but is unreadable or not valid TOML.
pub fn load_at(path: &Path) -> Result<Loaded> {
    let root = crate::config::read_table_at(path)?;
    let mut loaded = Loaded::default();
    let Some(section) = root.get("connectors").and_then(toml::Value::as_table) else {
        return Ok(loaded);
    };
    for (name, value) in section {
        if !valid_name(name) {
            loaded
                .problems
                .push(format!("{name}: not a valid connector name"));
            continue;
        }
        match value.clone().try_into::<ConnectorConfig>() {
            Ok(config) => match config.validate() {
                Ok(()) => {
                    loaded.connectors.insert(name.clone(), config);
                }
                Err(error) => loaded.problems.push(format!("{name}: {error}")),
            },
            Err(_) => loaded
                .problems
                .push(format!("{name}: invalid connector configuration")),
        }
    }
    Ok(loaded)
}

fn section<'a>(root: &'a mut toml::Table, name: &str) -> Result<&'a mut toml::Table> {
    root.entry("connectors".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("[connectors] is not a table")?
        .entry(name.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .with_context(|| format!("[connectors.{name}] is not a table"))
}

/// Add a connector; refuses to replace an existing one.
///
/// # Errors
/// The name is invalid or taken, the connector is invalid, or the write fails.
pub fn add_at(path: &Path, name: &str, config: &ConnectorConfig) -> Result<()> {
    if !valid_name(name) {
        bail!("connector names are 1-32 characters of a-z, 0-9, - and _");
    }
    config.validate()?;
    crate::config::edit_config_at(path, |root| {
        let exists = root
            .get("connectors")
            .and_then(toml::Value::as_table)
            .is_some_and(|table| table.contains_key(name));
        if exists {
            bail!("a connector named {name} already exists");
        }
        let value = toml::Value::try_from(config).context("serializing connector")?;
        *section(root, name)? = value
            .as_table()
            .cloned()
            .context("connector is not a table")?;
        Ok(())
    })
}

/// Remove a connector's table. Returns whether it existed.
///
/// # Errors
/// The write fails.
pub fn remove_at(path: &Path, name: &str) -> Result<bool> {
    let mut existed = false;
    crate::config::edit_config_at(path, |root| {
        if let Some(table) = root
            .get_mut("connectors")
            .and_then(toml::Value::as_table_mut)
        {
            existed = table.remove(name).is_some();
            if table.is_empty() {
                root.remove("connectors");
            }
        }
        Ok(())
    })?;
    Ok(existed)
}

fn edit_connector(
    path: &Path,
    name: &str,
    edit: impl FnOnce(&mut toml::Table) -> Result<()>,
) -> Result<()> {
    crate::config::edit_config_at(path, |root| {
        let exists = root
            .get("connectors")
            .and_then(toml::Value::as_table)
            .is_some_and(|table| table.contains_key(name));
        if !exists {
            bail!("no connector named {name}");
        }
        edit(section(root, name)?)
    })
}

/// Enable or disable a connector.
///
/// # Errors
/// The connector does not exist or the write fails.
pub fn set_enabled_at(path: &Path, name: &str, on: bool) -> Result<()> {
    edit_connector(path, name, |table| {
        table.insert("enabled".into(), toml::Value::Boolean(on));
        Ok(())
    })
}

fn sub_table<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .with_context(|| format!("{key} is not a table"))
}

/// Set (or with `None`, clear) the policy for one tool.
///
/// # Errors
/// The connector does not exist or the write fails.
pub fn set_policy_at(path: &Path, name: &str, tool: &str, policy: Option<Policy>) -> Result<()> {
    edit_connector(path, name, |table| {
        let tools = sub_table(table, "tools")?;
        match policy {
            Some(policy) => {
                tools.insert(tool.into(), toml::Value::String(policy.as_str().into()));
            }
            None => {
                tools.remove(tool);
            }
        }
        if tools.is_empty() {
            table.remove("tools");
        }
        Ok(())
    })
}

/// Always allow a tool: write `allow` and pin the definition hash.
///
/// # Errors
/// The connector does not exist or the write fails.
pub fn always_allow_at(path: &Path, name: &str, tool: &str, hash: &str) -> Result<()> {
    edit_connector(path, name, |table| {
        sub_table(table, "tools")?.insert(tool.into(), toml::Value::String("allow".into()));
        sub_table(table, "pins")?.insert(tool.into(), toml::Value::String(hash.into()));
        Ok(())
    })
}

/// Set or clear a pin without touching the policy.
///
/// # Errors
/// The connector does not exist or the write fails.
pub fn set_pin_at(path: &Path, name: &str, tool: &str, hash: Option<&str>) -> Result<()> {
    edit_connector(path, name, |table| {
        let pins = sub_table(table, "pins")?;
        match hash {
            Some(hash) => {
                pins.insert(tool.into(), toml::Value::String(hash.into()));
            }
            None => {
                pins.remove(tool);
            }
        }
        if pins.is_empty() {
            table.remove("pins");
        }
        Ok(())
    })
}

/// Store a fallback secret in the connector's table (no keychain available).
///
/// # Errors
/// The connector does not exist or the write fails.
pub fn set_secret_at(path: &Path, name: &str, kind: &str, value: Option<&str>) -> Result<()> {
    edit_connector(path, name, |table| {
        let secrets = sub_table(table, "secrets")?;
        match value {
            Some(value) => {
                secrets.insert(kind.into(), toml::Value::String(value.into()));
            }
            None => {
                secrets.remove(kind);
            }
        }
        if secrets.is_empty() {
            table.remove("secrets");
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("nexus-conn-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn names_validate_and_slugs_become_valid_names() {
        assert!(valid_name("linear"));
        assert!(valid_name("my-server_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Upper"));
        assert!(valid_name("a__b"));
        assert!(!valid_name(&"x".repeat(33)));
        for text in ["GitHub Copilot!", "__a__b__", "  ", "ünï", &"y".repeat(60)] {
            assert!(valid_name(&slug(text)), "{text:?} -> {:?}", slug(text));
        }
        assert_eq!(slug("GitHub Copilot!"), "github-copilot");
    }

    #[test]
    fn names_are_suggested_from_urls_and_commands() {
        for (target, expected) in [
            ("https://mcp.linear.app/mcp", "linear"),
            ("https://api.githubcopilot.com/mcp/", "githubcopilot"),
            ("http://127.0.0.1:8080/mcp", "127-0-0-1"),
            (
                "npx -y @modelcontextprotocol/server-filesystem /Users/me/notes",
                "notes",
            ),
            (
                "npx -y @modelcontextprotocol/server-filesystem",
                "filesystem",
            ),
            ("uvx mcp-server-git", "git"),
            ("python3 /opt/tools/weather.py", "weather"),
            ("", "connector"),
        ] {
            assert_eq!(suggest_name(target), expected, "{target:?}");
            assert!(valid_name(&suggest_name(target)));
        }
    }

    #[test]
    fn urls_need_https_unless_loopback() {
        assert!(validate_url("https://mcp.example.com/mcp").is_ok());
        assert!(validate_url("http://127.0.0.1:8080/mcp").is_ok());
        assert!(validate_url("http://localhost/mcp").is_ok());
        assert!(validate_url("http://example.com/mcp").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("not a url").is_err());
        assert_eq!(
            redact_url("https://u:p@x.example/mcp?key=SECRET#f"),
            "https://x.example/mcp"
        );
    }

    #[test]
    fn config_round_trips_and_keeps_every_other_section() {
        let (dir, path) = temp_config(
            "[provider]\nopenrouter_key = \"sk-keep\"\n\n[agent]\nfuture_knob = 7\n\n[connectors.linear]\nurl = \"https://mcp.linear.app/mcp\"\nfuture_key = \"kept\"\n",
        );
        add_at(
            &path,
            "files",
            &ConnectorConfig::local(vec!["npx".into(), "-y".into(), "srv".into()]),
        )
        .unwrap();
        set_policy_at(&path, "linear", "create_issue", Some(Policy::Ask)).unwrap();
        always_allow_at(&path, "linear", "list_issues", "abc123").unwrap();
        set_enabled_at(&path, "files", false).unwrap();
        set_secret_at(&path, "linear", "bearer", Some("tok")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("sk-keep"), "{text}");
        assert!(text.contains("future_knob"), "{text}");
        assert!(text.contains("future_key"), "{text}");
        let loaded = load_at(&path).unwrap();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let linear = &loaded.connectors["linear"];
        assert_eq!(linear.policy("create_issue"), Some(Policy::Ask));
        assert_eq!(linear.policy("list_issues"), Some(Policy::Allow));
        assert_eq!(linear.pins["list_issues"], "abc123");
        assert!(!loaded.connectors["files"].enabled);
        set_policy_at(&path, "linear", "create_issue", None).unwrap();
        set_secret_at(&path, "linear", "bearer", None).unwrap();
        assert!(remove_at(&path, "files").unwrap());
        assert!(!remove_at(&path, "files").unwrap());
        let loaded = load_at(&path).unwrap();
        assert_eq!(loaded.connectors.len(), 1);
        assert!(loaded.connectors["linear"].secrets.is_empty());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("future_knob")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_bad_connector_is_reported_without_losing_the_rest() {
        let (dir, path) = temp_config(
            "[connectors.good]\nurl = \"https://a.example/mcp\"\n[connectors.both]\nurl = \"https://a.example\"\ncommand = [\"x\"]\n[connectors.\"Bad Name\"]\nurl = \"https://a.example\"\n[connectors.plain]\nurl = \"http://example.com/mcp\"\n",
        );
        let loaded = load_at(&path).unwrap();
        assert_eq!(loaded.connectors.keys().collect::<Vec<_>>(), ["good"]);
        assert_eq!(loaded.problems.len(), 3, "{:?}", loaded.problems);
        assert!(add_at(&path, "good", &ConnectorConfig::remote("https://b.example")).is_err());
        assert!(add_at(&path, "Nope", &ConnectorConfig::remote("https://b.example")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_errors_never_echo_secret_source_lines_or_values() {
        for body in [
            "[connectors.demo]\nurl = \"https://demo.example/mcp\"\nauth = \"fake-config-secret\"\n",
            "[provider]\nopenrouter_key = \"fake-config-secret\" invalid\n",
        ] {
            let (dir, path) = temp_config(body);
            let errors = match load_at(&path) {
                Ok(loaded) => loaded.problems.join("\n"),
                Err(error) => format!("{error:#}"),
            };
            assert!(!errors.is_empty());
            assert!(!errors.contains("fake-config-secret"), "{errors}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn concurrent_config_edits_preserve_each_others_sections() {
        let (dir, path) = temp_config("[provider]\nopenrouter_key = \"keep\"\n");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for index in 0..8 {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    crate::config::edit_config_at(path, |root| {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        root.insert(format!("section_{index}"), toml::Value::Integer(index));
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        let root = crate::config::read_table_at(&path).unwrap();
        assert_eq!(root["provider"]["openrouter_key"].as_str(), Some("keep"));
        for index in 0..8 {
            assert_eq!(root[&format!("section_{index}")].as_integer(), Some(index));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn debug_output_hides_env_values_and_secrets() {
        let mut config = ConnectorConfig::local(vec!["srv".into()]);
        config
            .env
            .insert("API_KEY".into(), "super-secret-env".into());
        config
            .headers
            .insert("X".into(), "super-secret-header".into());
        config
            .secrets
            .insert("bearer".into(), "super-secret-token".into());
        let shown = format!("{config:?}");
        assert!(!shown.contains("super-secret"), "{shown}");
        assert!(shown.contains("API_KEY"));
    }
}
