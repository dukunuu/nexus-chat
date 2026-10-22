//! `nexus connectors list|add|remove|tools|import`: manage MCP connectors
//! from the shell. These edit `config.toml` and the keychain directly (no
//! TUI, no model); `tools` connects once to show what a server offers.

use std::io::{IsTerminal as _, Write as _};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use nexus_core::connectors::config::{AuthKind, ConnectorConfig};
use nexus_core::connectors::import::{self, ImportPaths, Source};
use nexus_core::connectors::{Registry, Status};

#[derive(Subcommand)]
pub enum ConnectorsCmd {
    /// List configured connectors.
    List {
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Add a connector: a hosted server (`--url`) or a local one
    /// (`--command -- <program> <args…>`).
    Add {
        /// Short name: a-z, 0-9, - and _ (it prefixes the tool names).
        name: String,
        /// Hosted server, e.g. `https://mcp.linear.app/mcp`.
        #[arg(long, conflicts_with = "command")]
        url: Option<String>,
        /// Local server: the program and its arguments follow after `--`.
        #[arg(long)]
        command: bool,
        /// Read a bearer token from stdin (hidden when typed) and keep it in
        /// the OS keychain.
        #[arg(long)]
        bearer: bool,
        /// The program and arguments for `--command`.
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// Remove a connector and its keychain entries.
    Remove {
        /// The connector's name.
        name: String,
    },
    /// Connect to a connector now and list the tools it offers.
    Tools {
        /// The connector's name.
        name: String,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Import MCP servers from Claude Desktop, Claude Code and Cursor.
    Import {
        /// Only this source: claude-desktop, claude-code or cursor.
        #[arg(long)]
        from: Option<String>,
        /// Import everything new without asking.
        #[arg(long, conflicts_with = "only")]
        all: bool,
        /// Import only these (comma-separated names).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
}

fn registry(autostart: bool) -> Result<Registry> {
    let space = nexus_core::space::Space::open()?;
    let registry = Registry::system(&space.root, None, autostart);
    let problems = registry.load()?;
    for problem in problems {
        eprintln!("connector config: {problem}");
    }
    Ok(registry)
}

pub async fn run(cmd: ConnectorsCmd) -> Result<()> {
    match cmd {
        ConnectorsCmd::List { json } => list(json),
        ConnectorsCmd::Add {
            name,
            url,
            command,
            bearer,
            argv,
        } => add(&name, url, command, bearer, argv),
        ConnectorsCmd::Remove { name } => {
            if registry(false)?.remove(&name)? {
                println!("removed {name}");
                Ok(())
            } else {
                bail!("no connector named {name}")
            }
        }
        ConnectorsCmd::Tools { name, json } => tools(&name, json).await,
        ConnectorsCmd::Import { from, all, only } => import_cmd(from.as_deref(), all, &only),
    }
}

fn list(json: bool) -> Result<()> {
    let registry = registry(false)?;
    let infos = registry.infos();
    if json {
        let rows: Vec<_> = infos
            .iter()
            .map(|info| {
                serde_json::json!({
                    "name": info.name,
                    "kind": if info.remote { "hosted" } else { "local" },
                    "target": info.target,
                    "enabled": info.enabled,
                    "auth": format!("{:?}", info.auth).to_lowercase(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if infos.is_empty() {
        println!(
            "no connectors yet · nexus connectors add NAME --url URL, or `nexus connectors import`"
        );
        return Ok(());
    }
    let width = infos.iter().map(|info| info.name.len()).max().unwrap_or(0);
    for info in infos {
        println!(
            "{:<width$}  {:<6}  {:<8}  {}{}",
            info.name,
            if info.remote { "hosted" } else { "local" },
            if info.enabled { "enabled" } else { "disabled" },
            info.target,
            if info.auth == AuthKind::Bearer {
                "  (bearer token)"
            } else {
                ""
            },
        );
    }
    println!(
        "\nlive status and tool policies: /connectors in the TUI, or `nexus connectors tools NAME`"
    );
    Ok(())
}

fn add(
    name: &str,
    url: Option<String>,
    command: bool,
    bearer: bool,
    argv: Vec<String>,
) -> Result<()> {
    if bearer && url.is_none() {
        bail!(
            "--bearer is for hosted servers (--url); a local server takes its secrets as environment variables"
        );
    }
    let mut connector = match (url, command || !argv.is_empty()) {
        (Some(url), false) => ConnectorConfig::remote(&url),
        (None, true) => {
            if argv.is_empty() {
                bail!(
                    "--command needs the program after `--`, e.g. --command -- npx -y server-name"
                );
            }
            ConnectorConfig::local(argv)
        }
        _ => bail!("give either --url URL or --command -- PROGRAM ARGS"),
    };
    let mut secrets = Vec::new();
    if bearer {
        connector.auth = AuthKind::Bearer;
        let token = read_secret("Bearer token (input hidden): ")?;
        if token.is_empty() {
            bail!("no token given");
        }
        secrets.push(("bearer".to_string(), token));
    }
    let registry = registry(false)?;
    registry.add_with_secrets(name, connector, &secrets)?;
    println!("added {name}");
    if !secrets.is_empty() {
        println!(
            "token stored in the OS keychain (or owner-only config.toml when none is available)"
        );
    }
    Ok(())
}

/// Read a secret from stdin: hidden while typing on a terminal, one line
/// when piped. Never echoed, never logged.
fn read_secret(prompt: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        return Ok(line.trim().to_string());
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let mut secret = String::new();
    let outcome = loop {
        match crossterm::event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Esc => break Err(anyhow::anyhow!("cancelled")),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    secret.pop();
                }
                KeyCode::Char(c) => secret.push(c),
                _ => {}
            },
            Ok(Event::Paste(text)) => secret.push_str(text.trim_end_matches(['\r', '\n'])),
            Ok(_) => {}
            Err(error) => break Err(error.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    outcome.map(|()| secret.trim().to_string())
}

async fn tools(name: &str, json: bool) -> Result<()> {
    let registry = registry(false)?;
    if !registry.infos().iter().any(|info| info.name == name) {
        bail!("no connector named {name}");
    }
    registry.restart(name);
    registry.wait_ready(Duration::from_secs(40)).await;
    let info = registry
        .infos()
        .into_iter()
        .find(|info| info.name == name)
        .context("the connector disappeared")?;
    match &info.status {
        Status::Connected { .. } => {}
        Status::Disabled => bail!("{name} is disabled · enable it in /connectors"),
        Status::NeedsAuth { .. } => bail!(
            "{name} needs credentials · its bearer token is missing or was rejected (OAuth sign-in arrives in the next release)"
        ),
        Status::Error(why) => bail!("{name}: {why}"),
        Status::Connecting => bail!("{name}: still connecting after 40s"),
    }
    let rows = registry.tool_rows(name);
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "tool": row.name,
                    "model_name": row.exposed,
                    "title": row.title,
                    "annotations": row.words,
                    "policy": row.policy.map(nexus_core::connectors::Policy::as_str),
                    "read_only": row.read_only,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{name} · {} tools{}",
        rows.len(),
        info.protocol
            .as_ref()
            .map_or(String::new(), |protocol| format!(" · protocol {protocol}"))
    );
    let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
    for row in &rows {
        println!(
            "{:<width$}  {:<5}  {}",
            row.name,
            row.policy
                .map_or("auto", nexus_core::connectors::Policy::as_str),
            row.words
        );
    }
    for skipped in &info.skipped {
        println!("skipped: {skipped}");
    }
    Ok(())
}

fn import_cmd(from: Option<&str>, all: bool, only: &[String]) -> Result<()> {
    let source = from
        .map(|word| {
            Source::parse(word).with_context(|| {
                format!("unknown source {word:?} · claude-desktop, claude-code or cursor")
            })
        })
        .transpose()?;
    let registry = registry(false)?;
    let paths = ImportPaths::system(std::env::current_dir().ok().as_deref());
    let mut found = import::discover(&paths, &registry.configs());
    found
        .proposals
        .retain(|proposal| source.is_none_or(|source| proposal.source == source));
    for skipped in &found.skipped {
        eprintln!("skipped {skipped}");
    }
    if found.proposals.is_empty() {
        println!("nothing found in Claude Desktop, Claude Code or Cursor");
        return Ok(());
    }
    for (index, proposal) in found.proposals.iter().enumerate() {
        println!(
            "{:>2}. {:<20} {:<15} {}{}{}",
            index + 1,
            proposal.name,
            proposal.source.label(),
            proposal.config.target_label(),
            if proposal.secrets.is_empty() {
                String::new()
            } else {
                format!("  ({} secret → keychain)", proposal.secrets.len())
            },
            if proposal.already {
                "  (already added)"
            } else {
                ""
            },
        );
    }
    let chosen: Vec<String> = if all {
        Vec::new()
    } else if !only.is_empty() {
        only.to_vec()
    } else if std::io::stdin().is_terminal() {
        eprint!("import which? numbers like 1,3 · all · enter for none: ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        if line == "all" {
            Vec::new()
        } else {
            line.split(',')
                .map(|number| {
                    number
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| found.proposals.get(n.checked_sub(1)?))
                        .map(|proposal| proposal.name.clone())
                        .context("invalid selection · enter listed numbers or all")
                })
                .collect::<Result<Vec<_>>>()?
        }
    } else {
        println!("\nre-run with --all or --only NAME,NAME to import");
        return Ok(());
    };
    let added = import::apply(&registry, &found.proposals, &chosen)?;
    if added.is_empty() {
        println!("nothing imported");
    } else {
        println!("imported {}", added.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser as _;

    fn parse(args: &[&str]) -> Result<ConnectorsCmd, clap::Error> {
        let mut argv = vec!["nexus", "connectors"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Some(Command::Connectors { cmd }) => cmd,
            _ => panic!("not a connectors command"),
        })
    }

    #[test]
    fn subcommands_parse_with_url_command_and_bearer() {
        assert!(matches!(
            parse(&["list", "--json"]),
            Ok(ConnectorsCmd::List { json: true })
        ));
        match parse(&[
            "add",
            "linear",
            "--url",
            "https://mcp.linear.app/mcp",
            "--bearer",
        ])
        .unwrap()
        {
            ConnectorsCmd::Add {
                name,
                url,
                bearer,
                command,
                argv,
            } => {
                assert_eq!(name, "linear");
                assert_eq!(url.as_deref(), Some("https://mcp.linear.app/mcp"));
                assert!(bearer && !command && argv.is_empty());
            }
            _ => panic!("add"),
        }
        match parse(&[
            "add",
            "files",
            "--command",
            "--",
            "npx",
            "-y",
            "srv",
            "--flag",
        ])
        .unwrap()
        {
            ConnectorsCmd::Add {
                command, argv, url, ..
            } => {
                assert!(command && url.is_none());
                assert_eq!(argv, ["npx", "-y", "srv", "--flag"]);
            }
            _ => panic!("add"),
        }
        assert!(matches!(
            parse(&["remove", "linear"]),
            Ok(ConnectorsCmd::Remove { .. })
        ));
        assert!(matches!(
            parse(&["tools", "linear", "--json"]),
            Ok(ConnectorsCmd::Tools { json: true, .. })
        ));
        match parse(&["import", "--from", "cursor", "--only", "a,b"]).unwrap() {
            ConnectorsCmd::Import { from, only, all } => {
                assert_eq!(from.as_deref(), Some("cursor"));
                assert_eq!(only, ["a", "b"]);
                assert!(!all);
            }
            _ => panic!("import"),
        }
        // --url and --command are exclusive; a bearer token needs a url.
        assert!(parse(&["add", "x", "--url", "https://a.example", "--command"]).is_err());
        assert!(parse(&["import", "--all", "--only", "a"]).is_err());
    }

    #[test]
    fn add_rejects_ambiguous_or_incomplete_targets_before_touching_anything() {
        assert!(add("x", None, false, false, Vec::new()).is_err());
        assert!(add("x", None, true, false, Vec::new()).is_err());
        assert!(
            add(
                "x",
                Some("https://a.example".into()),
                true,
                false,
                vec!["srv".into()]
            )
            .is_err()
        );
        assert!(add("x", None, true, true, vec!["srv".into()]).is_err());
    }
}
