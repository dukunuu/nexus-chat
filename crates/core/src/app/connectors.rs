//! `/connectors`: the domain half. Adding, removing, enabling, importing and
//! setting per-tool policy for MCP connectors; the popup (view state) lives
//! in the TUI. Secrets never travel through `AppCommand`: a bearer token is
//! stored by the frontend straight into the keychain.

use anyhow::{Result, bail};

use super::App;
use crate::connectors::config::{ConnectorConfig, Policy};
use crate::connectors::import::{self, ImportPaths, Source};

impl App {
    /// Local-command connectors start a process on this machine, so they are
    /// only added from the terminal UI (a person at the keyboard), never from
    /// a remote command channel.
    fn ensure_may_run_commands(&self) -> Result<()> {
        if self.interactive_ui {
            Ok(())
        } else {
            bail!(
                "adding a local command connector needs the terminal UI · use `nexus connectors add` on this machine"
            )
        }
    }

    pub(super) fn add_connector(
        &mut self,
        name: &str,
        url: Option<String>,
        command: Vec<String>,
        bearer: bool,
    ) -> Result<()> {
        let mut config = match (url, command.is_empty()) {
            (Some(url), true) => ConnectorConfig::remote(&url),
            (None, false) => {
                self.ensure_may_run_commands()?;
                ConnectorConfig::local(command)
            }
            _ => bail!("a connector is either a url or a command"),
        };
        if bearer {
            config.auth = crate::connectors::AuthKind::Bearer;
        }
        self.connectors.add(name, config)?;
        self.push_status(if bearer {
            format!("connector {name} added · it needs its token before it can connect")
        } else {
            format!("connector {name} added · connecting…")
        });
        Ok(())
    }

    pub(super) fn remove_connector(&mut self, name: &str) -> Result<()> {
        if self.connectors.remove(name)? {
            self.push_status(format!("connector {name} removed"));
            Ok(())
        } else {
            bail!("no connector named {name}")
        }
    }

    pub(super) fn set_connector_enabled(&mut self, name: &str, on: bool) -> Result<()> {
        self.connectors.set_enabled(name, on)?;
        self.push_status(format!(
            "connector {name} {}",
            if on { "enabled" } else { "disabled" }
        ));
        Ok(())
    }

    pub(super) fn set_connector_policy(
        &mut self,
        name: &str,
        tool: &str,
        policy: &str,
    ) -> Result<()> {
        let parsed = match policy {
            "default" | "reset" | "" => None,
            word => Some(Policy::parse(word).ok_or_else(|| {
                anyhow::anyhow!("policy must be allow, ask, deny or default, not {word:?}")
            })?),
        };
        self.connectors.set_policy(name, tool, parsed)?;
        self.push_status(format!(
            "{name} · {tool}: {}",
            parsed.map_or("default", Policy::as_str)
        ));
        Ok(())
    }

    pub(super) fn import_connectors(&mut self, source: &str, names: &[String]) -> Result<()> {
        let only = match source {
            "" | "all" => None,
            word => Some(Source::parse(word).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown source {word:?} · claude-desktop, claude-code, cursor or all"
                )
            })?),
        };
        let paths = self
            .import_paths
            .clone()
            .unwrap_or_else(|| ImportPaths::system(std::env::current_dir().ok().as_deref()));
        let mut found = import::discover(&paths, &self.connectors.configs());
        found
            .proposals
            .retain(|proposal| only.is_none_or(|source| proposal.source == source));
        if !self.interactive_ui {
            found
                .proposals
                .retain(|proposal| proposal.config.command.is_empty());
        }
        let added = import::apply(&self.connectors, &found.proposals, names)?;
        self.push_status(if added.is_empty() {
            "nothing new to import".to_string()
        } else {
            format!("imported {} · {}", added.len(), added.join(", "))
        });
        Ok(())
    }
}
