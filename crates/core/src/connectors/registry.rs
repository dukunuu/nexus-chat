//! The connector registry: which connectors exist, whether they are
//! connected, what tools they expose, and what the model is allowed to see
//! and call.
//!
//! One background task per enabled connector connects (never blocking the
//! UI), lists tools, refreshes them on `list_changed` and `ttlMs`, and
//! reconnects a crashed stdio server with backoff (three restarts per
//! minute, then an error state). Everything here is synchronous state
//! behind a mutex that is never held across an await, so the TUI can read
//! it from the render path.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::client::{Session, Timeouts};
use super::config::{self, AuthKind, ConnectorConfig, Policy};
use super::protocol::{CallResult, ToolInfo};
use super::secrets::Vault;
use super::transport::http::HttpTransport;
use super::transport::stdio::{LogSink, StdioSpec, StdioTransport};
use super::transport::{McpError, Signals, Transport};
use crate::provider::ToolDef;

/// More than this many exposed tools and the model gets the `connectors`
/// meta-tool instead of every definition.
pub const META_THRESHOLD: usize = 30;
/// A model-facing description is cut to this many bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
/// A tool whose input schema serializes larger than this is skipped.
pub const MAX_SCHEMA_BYTES: usize = 16 * 1024;
/// The longest tool name providers accept.
const MAX_NAME_CHARS: usize = 64;
/// Results returned by `connectors` `search`.
pub const SEARCH_LIMIT: usize = 8;

/// Where a connector stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected {
        tools: usize,
    },
    /// 401 or a missing bearer token. C2 adds OAuth sign-in.
    NeedsAuth {
        resource_metadata: Option<String>,
    },
    Error(String),
    Disabled,
}

/// Restart behaviour for crashed or unreachable servers.
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    pub restart_base: Duration,
    pub restart_window: Duration,
    pub restart_max: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            restart_base: Duration::from_secs(1),
            restart_window: Duration::from_secs(60),
            restart_max: 3,
        }
    }
}

/// What a registry needs from its surroundings.
pub struct Env {
    /// `config.toml`; `None` keeps everything in memory (tests, headless).
    pub config_path: Option<PathBuf>,
    /// `<data>/logs/connectors`.
    pub logs_dir: Option<PathBuf>,
    pub vault: Arc<Vault>,
    /// Short status-line notes ("linear: could not connect").
    pub notes: Option<UnboundedSender<String>>,
    pub tuning: Tuning,
    /// Connect in the background when a connector is added, enabled or
    /// reloaded. Off for one-shot CLI commands that only edit config.
    pub autostart: bool,
}

struct Entry {
    config: ConnectorConfig,
    status: Status,
    tools: Vec<ToolInfo>,
    skipped: Vec<String>,
    session: Option<Session>,
    task: Option<tokio::task::AbortHandle>,
    token: u64,
    protocol: Option<String>,
    server_name: Option<String>,
}

impl Entry {
    fn new(config: ConnectorConfig) -> Self {
        Self {
            status: if config.enabled {
                Status::Connecting
            } else {
                Status::Disabled
            },
            config,
            tools: Vec::new(),
            skipped: Vec::new(),
            session: None,
            task: None,
            token: 0,
            protocol: None,
            server_name: None,
        }
    }
}

#[derive(Default)]
struct State {
    entries: BTreeMap<String, Entry>,
    /// (connector, tool) -> definition hash the user allowed this session.
    grants: HashMap<(String, String), String>,
    /// Tools called through the meta-tool, in call order.
    promoted: Vec<(String, String)>,
    /// Bumped whenever anything the model can see changes.
    generation: u64,
    next_token: u64,
}

struct Shared {
    env: Env,
    state: Mutex<State>,
    cache: Mutex<Option<(u64, Arc<Exposure>)>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        for entry in self.state.get_mut().expect("registry").entries.values_mut() {
            if let Some(task) = entry.task.take() {
                task.abort();
            }
        }
    }
}

/// The connectors, shared by the app, the toolbox and the popup.
#[derive(Clone)]
pub struct Registry(Arc<Shared>);

/// One connector as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorInfo {
    pub name: String,
    /// Where it connects, without query strings or credentials.
    pub target: String,
    pub remote: bool,
    pub enabled: bool,
    pub auth: AuthKind,
    pub status: Status,
    pub tool_count: usize,
    /// "tool: reason" for every tool left out of the model's list.
    pub skipped: Vec<String>,
    pub log_path: Option<PathBuf>,
    pub protocol: Option<String>,
    pub server_name: Option<String>,
}

/// Whether a call needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Runs without asking.
    Free,
    /// Show the approval panel; `changed` when the definition moved since
    /// the user allowed it.
    Ask { changed: bool },
    /// The user denied this tool.
    Denied,
}

/// One tool in the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    /// Annotations as words ("writes · may delete").
    pub words: String,
    pub read_only: bool,
    /// The explicit policy, if any.
    pub policy: Option<Policy>,
    pub access: Access,
    /// The name the model sees.
    pub exposed: String,
}

/// A tool the model can call, found by its exposed name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub connector: String,
    pub tool: String,
    pub exposed: String,
}

/// What the model is told about connector tools right now.
#[derive(Debug, Default)]
pub struct Exposure {
    /// Definitions to append after the built-in tools.
    pub defs: Vec<ToolDef>,
    /// Too many tools: only the `connectors` meta-tool (plus promoted ones).
    pub meta: bool,
    targets: HashMap<String, Target>,
    /// Every callable tool in stable order, for `search`.
    catalog: Vec<CatalogEntry>,
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    target: Target,
    title: Option<String>,
    description: String,
    schema: Value,
    read_only: bool,
}

impl Exposure {
    /// Look a model-supplied name up.
    #[must_use]
    pub fn target(&self, exposed: &str) -> Option<&Target> {
        self.targets.get(exposed)
    }

    /// How many tools could be called.
    #[must_use]
    pub fn tool_count(&self) -> usize {
        self.catalog.len()
    }

    /// The `connectors` `search` answer: the best matches by token overlap.
    #[must_use]
    pub fn search(&self, query: &str) -> String {
        let wanted = tokens(query);
        let mut scored: Vec<(usize, &CatalogEntry)> = self
            .catalog
            .iter()
            .map(|entry| {
                let name = tokens(&entry.target.tool);
                let title = tokens(entry.title.as_deref().unwrap_or_default());
                let description = tokens(&entry.description);
                let score: usize = wanted
                    .iter()
                    .map(|token| {
                        3 * usize::from(name.contains(token))
                            + 2 * usize::from(title.contains(token))
                            + usize::from(description.contains(token))
                    })
                    .sum();
                (score, entry)
            })
            .filter(|(score, _)| wanted.is_empty() || *score > 0)
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.target.exposed.cmp(&b.1.target.exposed))
        });
        if scored.is_empty() {
            return format!(
                "no connector tools match {query:?} · try different words, or an empty query to list a few"
            );
        }
        let mut out = String::new();
        for (_, entry) in scored.into_iter().take(SEARCH_LIMIT) {
            let first_line = entry
                .description
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default();
            let first_line: String = first_line.chars().take(160).collect();
            let _ = write!(
                out,
                "{} · {}",
                entry.target.exposed,
                if entry.read_only {
                    "read-only"
                } else {
                    "writes"
                },
            );
            if !first_line.is_empty() {
                let _ = write!(out, " · {first_line}");
            }
            if let Some(title) = entry
                .title
                .as_deref()
                .filter(|title| *title != entry.target.tool)
            {
                let _ = write!(out, "\n  title: {title}");
            }
            let _ = writeln!(
                out,
                "\n  schema: {}",
                super::protocol::canonical_json(&entry.schema)
            );
        }
        out.push_str("call one with connectors {action: \"call\", tool: <name>, arguments: {…}}");
        out
    }
}

/// Note that something the model can see (or that decides what it may do)
/// changed, so the cached exposure is rebuilt.
fn bump(state: &mut State) {
    state.generation += 1;
}

fn tokens(text: &str) -> Vec<String> {
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_string)
        .collect()
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Cut `text` to at most `max` bytes (on a character boundary), ending in an
/// ellipsis when something was removed.
fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let ellipsis = '…';
    let mut end = max.saturating_sub(ellipsis.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push(ellipsis);
    out
}

/// Why a tool cannot be offered to the model, if it cannot.
fn invalid_reason(tool: &ToolInfo) -> Option<String> {
    let Some(schema) = tool.input_schema.as_object() else {
        return Some("input schema is not an object".into());
    };
    if schema
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("object"))
    {
        return Some("input schema is not an object".into());
    }
    if tool.input_schema.to_string().len() > MAX_SCHEMA_BYTES {
        return Some("input schema is larger than 16 KB".into());
    }
    None
}

/// Display titles by exposed name, for tool cards (which render from the
/// stored call alone, with no registry in reach).
fn titles() -> &'static std::sync::RwLock<HashMap<String, (String, String)>> {
    static TITLES: std::sync::OnceLock<std::sync::RwLock<HashMap<String, (String, String)>>> =
        std::sync::OnceLock::new();
    TITLES.get_or_init(Default::default)
}

/// The title a connector tool should be shown under, if one is known.
#[must_use]
pub fn display_title(exposed: &str) -> Option<String> {
    titles()
        .read()
        .ok()?
        .get(exposed)
        .map(|(_, title)| title.clone())
}

/// The connector name for a card, including names containing `__`.
#[must_use]
pub fn display_connector(exposed: &str) -> Option<String> {
    titles()
        .read()
        .ok()?
        .get(exposed)
        .map(|(connector, _)| connector.clone())
}

impl Registry {
    /// A registry over `env`. Nothing is read or started yet.
    #[must_use]
    pub fn new(env: Env) -> Self {
        Self(Arc::new(Shared {
            env,
            state: Mutex::default(),
            cache: Mutex::new(None),
        }))
    }

    /// A registry with no config file and an in-memory vault: connectors
    /// exist only if added at runtime. What tests and headless tools use.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Env {
            config_path: None,
            logs_dir: None,
            vault: Arc::new(Vault::in_memory()),
            notes: None,
            tuning: Tuning::default(),
            autostart: true,
        })
    }

    /// The registry for the real app: `config.toml`, the OS keychain and
    /// logs under the data directory.
    #[must_use]
    pub fn system(
        data_root: &std::path::Path,
        notes: Option<UnboundedSender<String>>,
        autostart: bool,
    ) -> Self {
        let config_path = crate::config::config_path().ok();
        Self::new(Env {
            vault: Arc::new(Vault::system(config_path.clone(), notes.clone())),
            config_path,
            logs_dir: Some(data_root.join("logs").join("connectors")),
            notes,
            tuning: Tuning::default(),
            autostart,
        })
    }

    /// The vault holding this registry's secrets.
    #[must_use]
    pub fn vault(&self) -> &Arc<Vault> {
        &self.0.env.vault
    }

    /// The config file connectors are saved to.
    #[must_use]
    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.0.env.config_path.as_deref()
    }

    fn note(&self, text: impl Into<String>) {
        if let Some(notes) = &self.0.env.notes {
            let _ = notes.send(text.into());
        }
    }

    fn log_path(&self, name: &str) -> Option<PathBuf> {
        self.0
            .env
            .logs_dir
            .as_ref()
            .map(|dir| dir.join(format!("{name}.log")))
    }

    fn log(&self, name: &str, line: &str) {
        if let Some(path) = self.log_path(name) {
            LogSink::new(path).write(line, &[]);
        }
    }

    // ----- configuration ------------------------------------------------

    /// Read `[connectors.*]` and make the registry match it: new entries
    /// appear, vanished ones are stopped, changed ones restart. Problems with
    /// individual entries are returned, not fatal.
    ///
    /// # Errors
    /// The config file exists but cannot be read or parsed.
    pub fn load(&self) -> anyhow::Result<Vec<String>> {
        let Some(path) = &self.0.env.config_path else {
            return Ok(Vec::new());
        };
        let loaded = config::load_at(path)?;
        let mut restart = Vec::new();
        {
            let mut state = self.0.state.lock().expect("registry");
            let gone: Vec<String> = state
                .entries
                .keys()
                .filter(|name| !loaded.connectors.contains_key(*name))
                .cloned()
                .collect();
            for name in gone {
                if let Some(entry) = state.entries.remove(&name)
                    && let Some(task) = entry.task
                {
                    task.abort();
                }
            }
            for (name, config) in loaded.connectors {
                match state.entries.get_mut(&name) {
                    Some(entry) if entry.config == config => {}
                    Some(entry) => {
                        let reconnect = entry.config.url != config.url
                            || entry.config.command != config.command
                            || entry.config.env != config.env
                            || entry.config.headers != config.headers
                            || entry.config.auth != config.auth
                            || entry.config.enabled != config.enabled
                            || entry.config.secrets != config.secrets
                            || entry.config.connect_timeout_secs != config.connect_timeout_secs
                            || entry.config.list_timeout_secs != config.list_timeout_secs
                            || entry.config.call_timeout_secs != config.call_timeout_secs;
                        entry.config = config;
                        if reconnect {
                            restart.push(name.clone());
                        }
                    }
                    None => {
                        state.entries.insert(name.clone(), Entry::new(config));
                        restart.push(name);
                    }
                }
            }
            bump(&mut state);
        }
        for name in restart {
            self.maybe_restart(&name);
        }
        Ok(loaded.problems)
    }

    /// Connect every enabled connector in the background.
    pub fn start(&self) {
        let names: Vec<String> = self
            .0
            .state
            .lock()
            .expect("registry")
            .entries
            .keys()
            .cloned()
            .collect();
        for name in names {
            self.restart(&name);
        }
    }

    fn maybe_restart(&self, name: &str) {
        if self.0.env.autostart {
            self.restart(name);
        }
    }

    /// (Re)start one connector's background task. A disabled connector just
    /// settles into the disabled state; with no async runtime (a bare unit
    /// test) the state is updated but nothing connects.
    pub fn restart(&self, name: &str) {
        let token = {
            let mut state = self.0.state.lock().expect("registry");
            state.next_token += 1;
            let token = state.next_token;
            let Some(entry) = state.entries.get_mut(name) else {
                return;
            };
            if let Some(task) = entry.task.take() {
                task.abort();
            }
            entry.session = None;
            entry.token = token;
            if entry.config.enabled {
                entry.status = Status::Connecting;
            } else {
                entry.status = Status::Disabled;
                entry.tools.clear();
                entry.skipped.clear();
            }
            let enabled = entry.config.enabled;
            bump(&mut state);
            if !enabled {
                return;
            }
            token
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let handle = runtime.spawn(supervise(Arc::downgrade(&self.0), name.to_string(), token));
        if let Some(entry) = self.0.state.lock().expect("registry").entries.get_mut(name) {
            entry.task = Some(handle.abort_handle());
        }
    }

    /// Add a connector: validates, saves to the config file, and connects.
    ///
    /// # Errors
    /// Invalid name or connector, a duplicate, or a failed write.
    pub fn add(&self, name: &str, config: ConnectorConfig) -> anyhow::Result<()> {
        self.add_with_secrets(name, config, &[])
    }

    /// Add a connector together with its secrets (`(kind, value)` pairs for
    /// the keychain). The config is written first, the secrets next, and only
    /// then does the connector start, so it never connects without its token.
    /// A secret that cannot be stored undoes the add.
    ///
    /// # Errors
    /// Invalid name or connector, a duplicate, or a failed write.
    pub fn add_with_secrets(
        &self,
        name: &str,
        config: ConnectorConfig,
        secrets: &[(String, String)],
    ) -> anyhow::Result<()> {
        if !config::valid_name(name) {
            anyhow::bail!("connector names are 1-32 characters of a-z, 0-9, - and _");
        }
        config.validate()?;
        if self
            .0
            .state
            .lock()
            .expect("registry")
            .entries
            .contains_key(name)
        {
            anyhow::bail!("a connector named {name} already exists");
        }
        if let Some(path) = &self.0.env.config_path {
            config::add_at(path, name, &config)?;
        }
        for (kind, value) in secrets {
            if let Err(why) = self.0.env.vault.set(name, kind, value) {
                if let Some(path) = &self.0.env.config_path {
                    let _ = config::remove_at(path, name);
                }
                self.forget_secrets(name, &config);
                for (kind, _) in secrets {
                    self.0.env.vault.delete(name, kind);
                }
                anyhow::bail!("{why}");
            }
        }
        {
            let mut state = self.0.state.lock().expect("registry");
            state.entries.insert(name.to_string(), Entry::new(config));
            bump(&mut state);
        }
        self.maybe_restart(name);
        Ok(())
    }

    /// Remove a connector, its saved config and its keychain entries.
    ///
    /// # Errors
    /// The config file could not be written.
    pub fn remove(&self, name: &str) -> anyhow::Result<bool> {
        let in_file = match &self.0.env.config_path {
            Some(path) => config::remove_at(path, name)?,
            None => false,
        };
        let removed = {
            let mut state = self.0.state.lock().expect("registry");
            let entry = state.entries.remove(name);
            state.grants.retain(|(connector, _), _| connector != name);
            state.promoted.retain(|(connector, _)| connector != name);
            bump(&mut state);
            entry
        };
        let Some(entry) = removed else {
            return Ok(in_file);
        };
        if let Some(task) = entry.task {
            task.abort();
        }
        self.forget_secrets(name, &entry.config);
        Ok(true)
    }

    /// Delete every keychain entry a connector owns.
    pub fn forget_secrets(&self, name: &str, config: &ConnectorConfig) {
        let vault = &self.0.env.vault;
        vault.delete(name, "bearer");
        for value in config.env.values().chain(config.headers.values()) {
            if let Some(kind) = value.strip_prefix(super::secrets::KEYCHAIN_PREFIX) {
                vault.delete(name, kind);
            }
        }
        for kind in config.secrets.keys() {
            vault.delete(name, kind);
        }
    }

    /// Enable or disable a connector (and connect or disconnect it).
    ///
    /// # Errors
    /// Unknown connector, or the config could not be written.
    pub fn set_enabled(&self, name: &str, on: bool) -> anyhow::Result<()> {
        if let Some(path) = &self.0.env.config_path {
            config::set_enabled_at(path, name, on)?;
        }
        {
            let mut state = self.0.state.lock().expect("registry");
            let entry = state
                .entries
                .get_mut(name)
                .ok_or_else(|| anyhow::anyhow!("no connector named {name}"))?;
            entry.config.enabled = on;
        }
        self.maybe_restart(name);
        Ok(())
    }

    /// Set or clear the policy for one tool.
    ///
    /// # Errors
    /// Unknown connector, or the config could not be written.
    pub fn set_policy(&self, name: &str, tool: &str, policy: Option<Policy>) -> anyhow::Result<()> {
        let hash = {
            let state = self.0.state.lock().expect("registry");
            let entry = state
                .entries
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("no connector named {name}"))?;
            entry
                .tools
                .iter()
                .find(|candidate| candidate.name == tool)
                .map(ToolInfo::definition_hash)
        };
        if let Some(path) = &self.0.env.config_path {
            if let (Some(Policy::Allow), Some(hash)) = (policy, &hash) {
                config::always_allow_at(path, name, tool, hash)?;
            } else {
                config::set_policy_at(path, name, tool, policy)?;
                if policy != Some(Policy::Allow) {
                    config::set_pin_at(path, name, tool, None)?;
                }
            }
        }
        let mut state = self.0.state.lock().expect("registry");
        if let Some(entry) = state.entries.get_mut(name) {
            match policy {
                Some(policy) => {
                    entry
                        .config
                        .tools
                        .insert(tool.into(), policy.as_str().into());
                }
                None => {
                    entry.config.tools.remove(tool);
                }
            }
            match (policy, hash) {
                (Some(Policy::Allow), Some(hash)) => {
                    entry.config.pins.insert(tool.into(), hash);
                }
                _ => {
                    entry.config.pins.remove(tool);
                }
            }
        }
        state.grants.remove(&(name.to_string(), tool.to_string()));
        bump(&mut state);
        Ok(())
    }

    // ----- reading state ------------------------------------------------

    /// Every connector, sorted by name.
    #[must_use]
    pub fn infos(&self) -> Vec<ConnectorInfo> {
        let state = self.0.state.lock().expect("registry");
        state
            .entries
            .iter()
            .map(|(name, entry)| ConnectorInfo {
                name: name.clone(),
                target: entry.config.target_label(),
                remote: entry.config.url.is_some(),
                enabled: entry.config.enabled,
                auth: entry.config.auth,
                status: entry.status.clone(),
                tool_count: entry.tools.len(),
                skipped: entry.skipped.clone(),
                log_path: self.log_path(name),
                protocol: entry.protocol.clone(),
                server_name: entry.server_name.clone(),
            })
            .collect()
    }

    /// Every connector's configuration, by name.
    #[must_use]
    pub fn configs(&self) -> BTreeMap<String, ConnectorConfig> {
        self.0
            .state
            .lock()
            .expect("registry")
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.config.clone()))
            .collect()
    }

    /// The configuration of one connector (secrets redacted by `Debug`).
    #[must_use]
    pub fn config_of(&self, name: &str) -> Option<ConnectorConfig> {
        self.0
            .state
            .lock()
            .expect("registry")
            .entries
            .get(name)
            .map(|entry| entry.config.clone())
    }

    /// One connector's tools with their policies, for the popup.
    #[must_use]
    pub fn tool_rows(&self, name: &str) -> Vec<ToolRow> {
        let exposure = self.exposure();
        let state = self.0.state.lock().expect("registry");
        let Some(entry) = state.entries.get(name) else {
            return Vec::new();
        };
        let mut rows: Vec<ToolRow> = entry
            .tools
            .iter()
            .map(|tool| ToolRow {
                name: tool.name.clone(),
                title: tool.title.clone(),
                description: tool.description.clone(),
                words: tool.annotations.words(),
                read_only: tool.annotations.is_free(),
                policy: entry.config.policy(&tool.name),
                access: access_of(&state, name, entry, tool),
                exposed: exposure
                    .catalog
                    .iter()
                    .find(|candidate| {
                        candidate.target.connector == name && candidate.target.tool == tool.name
                    })
                    .map(|candidate| candidate.target.exposed.clone())
                    .unwrap_or_default(),
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Wait until no connector is still connecting (or `limit` passes).
    pub async fn wait_ready(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let busy = self
                .0
                .state
                .lock()
                .expect("registry")
                .entries
                .values()
                .any(|entry| entry.status == Status::Connecting);
            if !busy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // ----- what the model sees -----------------------------------------

    /// The model-facing view of the connectors right now (cached until
    /// something changes).
    #[must_use]
    pub fn exposure(&self) -> Arc<Exposure> {
        let state = self.0.state.lock().expect("registry");
        let mut cache = self.0.cache.lock().expect("exposure cache");
        if let Some((generation, exposure)) = cache.as_ref()
            && *generation == state.generation
        {
            return Arc::clone(exposure);
        }
        let exposure = Arc::new(build_exposure(&state));
        if let Ok(mut titles) = titles().write() {
            for entry in &exposure.catalog {
                titles.insert(
                    entry.target.exposed.clone(),
                    (
                        entry.target.connector.clone(),
                        entry
                            .title
                            .clone()
                            .unwrap_or_else(|| entry.target.tool.clone()),
                    ),
                );
            }
        }
        *cache = Some((state.generation, Arc::clone(&exposure)));
        exposure
    }

    /// Whether any connector tool could reach the model (cheap check).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.state.lock().expect("registry").entries.is_empty()
    }

    /// The tool behind a model-supplied name.
    #[must_use]
    pub fn resolve(&self, exposed: &str) -> Option<Target> {
        self.exposure().target(exposed).cloned()
    }

    /// The tool's current definition.
    #[must_use]
    pub fn tool_info(&self, target: &Target) -> Option<ToolInfo> {
        self.0
            .state
            .lock()
            .expect("registry")
            .entries
            .get(&target.connector)?
            .tools
            .iter()
            .find(|tool| tool.name == target.tool)
            .cloned()
    }

    /// Whether the tool is classed read-only for scheduling: annotated
    /// read-only (and not destructive), or allowed by policy.
    #[must_use]
    pub fn is_read_only(&self, target: &Target) -> bool {
        let state = self.0.state.lock().expect("registry");
        let Some(entry) = state.entries.get(&target.connector) else {
            return false;
        };
        let Some(tool) = entry.tools.iter().find(|tool| tool.name == target.tool) else {
            return false;
        };
        access_of(&state, &target.connector, entry, tool) == Access::Free
            && (entry.config.policy(&tool.name) == Some(Policy::Allow)
                || tool.annotations.is_free())
    }

    /// What the user must decide before this tool runs.
    #[must_use]
    pub fn access(&self, target: &Target) -> Access {
        let state = self.0.state.lock().expect("registry");
        let Some(entry) = state.entries.get(&target.connector) else {
            return Access::Denied;
        };
        let Some(tool) = entry.tools.iter().find(|tool| tool.name == target.tool) else {
            return Access::Denied;
        };
        access_of(&state, &target.connector, entry, tool)
    }

    /// "Allow this tool for the session": remember the definition hash.
    pub fn grant_session(&self, target: &Target) {
        let Some(info) = self.tool_info(target) else {
            return;
        };
        let mut state = self.0.state.lock().expect("registry");
        state.grants.insert(
            (target.connector.clone(), target.tool.clone()),
            info.definition_hash(),
        );
        bump(&mut state);
    }

    /// "Always allow": policy `allow` plus the pinned definition hash.
    ///
    /// # Errors
    /// The config could not be written.
    pub fn allow_always(&self, target: &Target) -> anyhow::Result<()> {
        self.set_policy(&target.connector, &target.tool, Some(Policy::Allow))?;
        self.grant_session(target);
        Ok(())
    }

    /// Forget session grants and promoted tools (a new conversation).
    pub fn reset_session(&self) {
        let mut state = self.0.state.lock().expect("registry");
        if state.grants.is_empty() && state.promoted.is_empty() {
            return;
        }
        state.grants.clear();
        state.promoted.clear();
        bump(&mut state);
    }

    /// Append a tool called through the meta-tool to the regular list.
    pub fn promote(&self, target: &Target) {
        let mut state = self.0.state.lock().expect("registry");
        let key = (target.connector.clone(), target.tool.clone());
        if !state.promoted.contains(&key) {
            state.promoted.push(key);
            bump(&mut state);
        }
    }

    /// Call a tool on its connector.
    ///
    /// # Errors
    /// Not connected, or the call failed. A tool that ran and reported an
    /// error is `Ok` with `is_error`.
    pub async fn call(&self, target: &Target, arguments: Value) -> Result<CallResult, McpError> {
        let session = {
            let state = self.0.state.lock().expect("registry");
            let entry = state
                .entries
                .get(&target.connector)
                .ok_or_else(|| McpError::Closed("the connector was removed".into()))?;
            match (&entry.session, &entry.status) {
                (Some(session), Status::Connected { .. }) => session.clone(),
                (_, Status::NeedsAuth { resource_metadata }) => {
                    return Err(McpError::NeedsAuth {
                        resource_metadata: resource_metadata.clone(),
                    });
                }
                (_, Status::Disabled) => {
                    return Err(McpError::Closed("the connector is disabled".into()));
                }
                (_, Status::Error(why)) => {
                    return Err(McpError::Closed(format!("not connected ({why})")));
                }
                _ => return Err(McpError::Closed("still connecting".into())),
            }
        };
        let result = session.call_tool(&target.tool, arguments).await;
        if let Err(McpError::NeedsAuth { resource_metadata }) = &result {
            self.set_status(
                &target.connector,
                Status::NeedsAuth {
                    resource_metadata: resource_metadata.clone(),
                },
            );
        }
        result
    }

    /// Put a connector into a given state without connecting: lets UI tests
    /// show every status and a tool list with no network or process.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn force_state(&self, name: &str, status: Status, tools: Vec<ToolInfo>) {
        let mut state = self.0.state.lock().expect("registry");
        if let Some(entry) = state.entries.get_mut(name) {
            entry.status = status;
            entry.tools = tools;
        }
        bump(&mut state);
    }

    fn set_status(&self, name: &str, status: Status) {
        let mut state = self.0.state.lock().expect("registry");
        if let Some(entry) = state.entries.get_mut(name) {
            entry.status = status;
        }
        bump(&mut state);
    }
}

fn access_of(state: &State, connector: &str, entry: &Entry, tool: &ToolInfo) -> Access {
    let policy = entry.config.policy(&tool.name);
    if policy == Some(Policy::Deny) {
        return Access::Denied;
    }
    let hash = tool.definition_hash();
    let granted = state
        .grants
        .get(&(connector.to_string(), tool.name.clone()));
    let pinned = entry.config.pins.get(&tool.name);
    let changed = pinned.is_some_and(|pin| *pin != hash) || granted.is_some_and(|g| *g != hash);
    if granted == Some(&hash) {
        return Access::Free;
    }
    match policy {
        Some(Policy::Allow) if pinned.is_none_or(|pin| *pin == hash) => Access::Free,
        Some(Policy::Allow | Policy::Ask) => Access::Ask { changed },
        _ if tool.annotations.is_free() && !changed => Access::Free,
        _ => Access::Ask { changed },
    }
}

fn build_exposure(state: &State) -> Exposure {
    // Names are assigned over every valid tool, denied ones included, so
    // denying a tool never renames its neighbours.
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut catalog = Vec::new();
    let mut denied = std::collections::HashSet::new();
    for (name, entry) in &state.entries {
        if !matches!(entry.status, Status::Connected { .. }) {
            continue;
        }
        let mut tools: Vec<&ToolInfo> = entry.tools.iter().collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        for tool in tools {
            let prefix = format!("{name}__");
            let base: String = format!("{prefix}{}", sanitize(&tool.name))
                .chars()
                .take(MAX_NAME_CHARS)
                .collect();
            let mut exposed = base.clone();
            let mut counter = 2;
            while !used.insert(exposed.clone()) {
                let suffix = format!("_{counter}");
                let keep = MAX_NAME_CHARS - suffix.len();
                exposed = format!("{}{suffix}", base.chars().take(keep).collect::<String>());
                counter += 1;
            }
            let access = access_of(state, name, entry, tool);
            if access == Access::Denied {
                denied.insert(exposed.clone());
            }
            let description = if tool.description.trim().is_empty() {
                tool.title.clone().unwrap_or_else(|| tool.name.clone())
            } else {
                tool.description.clone()
            };
            let mut schema = tool.input_schema.clone();
            if let Some(object) = schema.as_object_mut() {
                object
                    .entry("type".to_string())
                    .or_insert_with(|| Value::String("object".into()));
            }
            catalog.push(CatalogEntry {
                target: Target {
                    connector: name.clone(),
                    tool: tool.name.clone(),
                    exposed,
                },
                title: tool.title.clone(),
                description,
                schema,
                read_only: tool.annotations.is_free(),
            });
        }
    }
    catalog.retain(|entry| !denied.contains(&entry.target.exposed));
    let targets: HashMap<String, Target> = catalog
        .iter()
        .map(|entry| (entry.target.exposed.clone(), entry.target.clone()))
        .collect();
    let def_of = |entry: &CatalogEntry| ToolDef {
        name: entry.target.exposed.clone(),
        description: truncate_bytes(
            &format!("[{}] {}", entry.target.connector, entry.description),
            MAX_DESCRIPTION_BYTES,
        ),
        parameters: entry.schema.clone(),
    };
    let meta = catalog.len() > META_THRESHOLD;
    let defs = if meta {
        let mut defs = vec![meta_def()];
        for (connector, tool) in &state.promoted {
            if let Some(entry) = catalog
                .iter()
                .find(|entry| entry.target.connector == *connector && entry.target.tool == *tool)
            {
                defs.push(def_of(entry));
            }
        }
        defs
    } else {
        catalog.iter().map(def_of).collect()
    };
    Exposure {
        defs,
        meta,
        targets,
        catalog,
    }
}

/// The single tool offered when connectors bring too many to list.
fn meta_def() -> ToolDef {
    ToolDef {
        name: "connectors".into(),
        description: "Search and call tools from connected services (MCP connectors). \
            action=search {query} finds tools by name or description and returns their names, \
            one-line descriptions and input schemas; action=call {tool, arguments} runs one by \
            the name search returned. Tools you have called become regular tools afterwards."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["search", "call"]},
                "query": {"type": "string", "description": "words to look for, for search"},
                "tool": {"type": "string", "description": "tool name from search, for call"},
                "arguments": {"type": "object", "description": "the tool's arguments, for call"}
            },
            "required": ["action"]
        }),
    }
}

// ----- the per-connector task --------------------------------------------

fn is_current(registry: &Registry, name: &str, token: u64) -> bool {
    registry
        .0
        .state
        .lock()
        .expect("registry")
        .entries
        .get(name)
        .is_some_and(|entry| entry.token == token && entry.config.enabled)
}

fn timeouts_of(config: &ConnectorConfig) -> Timeouts {
    let defaults = Timeouts::default();
    let secs = |value: Option<u64>, default: Duration| {
        value.map_or(default, |secs| Duration::from_secs(secs.max(1)))
    };
    Timeouts {
        connect: secs(config.connect_timeout_secs, defaults.connect),
        list: secs(config.list_timeout_secs, defaults.list),
        call: secs(config.call_timeout_secs, defaults.call),
    }
}

/// Resolve secrets and open the transport.
async fn open(
    registry: &Registry,
    name: &str,
    config: &ConnectorConfig,
    signals: &Arc<Signals>,
) -> Result<Transport, McpError> {
    let vault = Arc::clone(&registry.0.env.vault);
    let lookup_name = name.to_string();
    let lookup_config = config.clone();
    // The keychain can block (a prompt, a locked store): keep it off the
    // async workers.
    let resolved = tokio::task::spawn_blocking(move || {
        let bearer = (lookup_config.auth == AuthKind::Bearer)
            .then(|| vault.get(&lookup_name, "bearer"))
            .map(|token| {
                token.ok_or(McpError::NeedsAuth {
                    resource_metadata: None,
                })
            })
            .transpose()?;
        let resolve = |values: &BTreeMap<String, String>, what: &str| {
            values
                .iter()
                .map(|(key, value)| {
                    vault
                        .resolve(&lookup_name, value)
                        .map(|resolved| (key.clone(), resolved))
                        .ok_or_else(|| {
                            McpError::Protocol(format!("{what} {key}: the keychain has no value"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let headers = resolve(&lookup_config.headers, "header")?;
        let env = resolve(&lookup_config.env, "environment variable")?;
        Ok::<_, McpError>((bearer, headers, env))
    })
    .await
    .map_err(|_| McpError::Transport("keychain lookup failed".into()))??;
    let (bearer, headers, env) = resolved;
    if let Some(url) = &config.url {
        return HttpTransport::new(
            url,
            bearer,
            headers,
            Arc::clone(signals),
            timeouts_of(config).connect,
        )
        .map(|transport| Transport::Http(Box::new(transport)));
    }
    let mut redact: Vec<String> = env.iter().map(|(_, value)| value.clone()).collect();
    redact.extend(bearer);
    StdioTransport::spawn(
        &StdioSpec {
            argv: config.command.clone(),
            env,
            log_path: registry.log_path(name),
            redact,
        },
        Arc::clone(signals),
    )
    .map(Transport::Stdio)
}

fn retryable(error: &McpError) -> bool {
    match error {
        McpError::Transport(_)
        | McpError::Timeout { .. }
        | McpError::Closed(_)
        | McpError::Interrupted => true,
        McpError::Http { status, .. } => *status >= 500,
        _ => false,
    }
}

async fn supervise(shared: Weak<Shared>, name: String, token: u64) {
    let Some(registry) = shared.upgrade().map(Registry) else {
        return;
    };
    let tuning = registry.0.env.tuning;
    drop(registry);
    let mut crashes: Vec<Instant> = Vec::new();
    loop {
        let Some(registry) = shared.upgrade().map(Registry) else {
            return;
        };
        if !is_current(&registry, &name, token) {
            return;
        }
        let Some(config) = registry.config_of(&name) else {
            return;
        };
        let signals = Signals::new();
        let outcome = async {
            let transport = open(&registry, &name, &config, &signals).await?;
            Session::connect(transport, Arc::clone(&signals), timeouts_of(&config)).await
        }
        .await;
        let failure = match outcome {
            Ok(session) => {
                if !is_current(&registry, &name, token) {
                    return;
                }
                drop(registry);
                match serve(&shared, &name, token, &session, &signals).await {
                    Served::Superseded => return,
                    Served::Lost(error) => error,
                }
            }
            Err(error) => {
                drop(registry);
                error
            }
        };
        let Some(registry) = shared.upgrade().map(Registry) else {
            return;
        };
        if !is_current(&registry, &name, token) {
            return;
        }
        registry.log(&name, &format!("connect error: {failure}"));
        crashes.push(Instant::now());
        crashes.retain(|at| at.elapsed() < tuning.restart_window);
        let give_up = !retryable(&failure) || crashes.len() > tuning.restart_max;
        let status = match &failure {
            McpError::NeedsAuth { resource_metadata } => Status::NeedsAuth {
                resource_metadata: resource_metadata.clone(),
            },
            other if give_up && crashes.len() > tuning.restart_max => Status::Error(format!(
                "{other} · gave up after {} restarts in {}s",
                tuning.restart_max,
                tuning.restart_window.as_secs()
            )),
            other => Status::Error(other.to_string()),
        };
        {
            let mut state = registry.0.state.lock().expect("registry");
            if let Some(entry) = state.entries.get_mut(&name)
                && entry.token == token
            {
                entry.session = None;
                entry.status = if give_up {
                    status.clone()
                } else {
                    Status::Connecting
                };
                if give_up {
                    entry.tools.clear();
                }
            }
            bump(&mut state);
        }
        if give_up {
            registry.note(match status {
                Status::NeedsAuth { .. } => format!("{name}: sign-in needed · /connectors"),
                _ => format!("{name}: could not connect · /connectors"),
            });
            return;
        }
        let backoff = tuning.restart_base
            * 2_u32.saturating_pow(u32::try_from(crashes.len().saturating_sub(1)).unwrap_or(0));
        drop(registry);
        tokio::time::sleep(backoff).await;
    }
}

enum Served {
    Superseded,
    Lost(McpError),
}

/// Keep a connected session fresh until it dies.
async fn serve(
    shared: &Weak<Shared>,
    name: &str,
    token: u64,
    session: &Session,
    signals: &Arc<Signals>,
) -> Served {
    let mut first = true;
    loop {
        let Some(registry) = shared.upgrade().map(Registry) else {
            return Served::Superseded;
        };
        if !is_current(&registry, name, token) {
            return Served::Superseded;
        }
        let ttl = match refresh_tools(&registry, name, token, session, first).await {
            Ok(ttl) => ttl,
            Err(error) => return Served::Lost(error),
        };
        first = false;
        // Sleeping tasks must not keep their own registry (and child) alive.
        drop(registry);
        let wait = async {
            match ttl {
                Some(ttl) => tokio::time::sleep(ttl).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = signals.list_changed.notified() => {}
            () = wait => {}
            () = signals.wait_closed() => return Served::Lost(McpError::Closed("the connection closed".into())),
        }
    }
}

async fn refresh_tools(
    registry: &Registry,
    name: &str,
    token: u64,
    session: &Session,
    first: bool,
) -> Result<Option<Duration>, McpError> {
    let (listed, ttl_ms) = session.list_tools().await?;
    let negotiated = session.negotiated();
    let mut valid = Vec::new();
    let mut skipped = Vec::new();
    for tool in listed {
        match invalid_reason(&tool) {
            None => valid.push(tool),
            Some(reason) => skipped.push(format!("{}: {reason}", tool.name)),
        }
    }
    let note = (!skipped.is_empty()).then(|| {
        format!(
            "{name}: skipped {} tool{} with unusable schemas · /connectors",
            skipped.len(),
            if skipped.len() == 1 { "" } else { "s" }
        )
    });
    // A hand-written `allow` policy has no pin yet: pin its first discovered
    // definition before exposing it, so later server changes cannot inherit it.
    let config = registry.config_of(name).unwrap_or_default();
    let mut pins = Vec::new();
    for tool in &valid {
        if config.policy(&tool.name) == Some(Policy::Allow) && !config.pins.contains_key(&tool.name)
        {
            let hash = tool.definition_hash();
            if let Some(path) = registry.config_path() {
                config::set_pin_at(path, name, &tool.name, Some(&hash)).map_err(|_| {
                    McpError::Protocol("could not save connector definition pins".into())
                })?;
            }
            pins.push((tool.name.clone(), hash));
        }
    }
    {
        let mut state = registry.0.state.lock().expect("registry");
        let Some(entry) = state.entries.get_mut(name) else {
            return Ok(None);
        };
        if entry.token != token {
            return Ok(None);
        }
        entry.config.pins.extend(pins);
        let changed = entry.tools != valid || entry.skipped != skipped;
        entry.tools = valid;
        entry.skipped = skipped;
        entry.session = Some(session.clone());
        entry.protocol = Some(negotiated.protocol);
        entry.server_name = negotiated.server_name;
        entry.status = Status::Connected {
            tools: entry.tools.len(),
        };
        if first || changed {
            bump(&mut state);
        }
    }
    if first && let Some(note) = note {
        registry.note(note);
    }
    Ok(ttl_ms.map(|ms| Duration::from_millis(ms.max(250))))
}

#[cfg(test)]
mod tests {
    use super::truncate_bytes;

    #[test]
    fn descriptions_are_cut_on_character_boundaries_within_the_byte_cap() {
        assert_eq!(truncate_bytes("short", 1024), "short");
        for text in ["é".repeat(2000), "日本語".repeat(900), "a".repeat(5000)] {
            let cut = truncate_bytes(&text, 1024);
            assert!(cut.len() <= 1024, "{}", cut.len());
            assert!(cut.ends_with('…'));
            assert!(cut.len() > 1020);
        }
    }
}
