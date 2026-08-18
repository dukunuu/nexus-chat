//! CLI subcommands — everything `nexus` can do headless.
//!
//! Arg parsing is clap; dispatch happens in `main.rs` before the TUI boots.
//! Commands that only read state (`usage`, `sessions`, `spaces`, `export`,
//! `status`, `doctor`, `backup`, `memory`, …) open the `SQLite` db directly
//! and never touch the network, while `ask`/`chat`/`research`/`watch run`
//! boot the same `App` the TUI does and drive its pipelines headlessly (see
//! `app/headless.rs`).

// Casts here are on bounded display values (token counts, byte sizes) for
// human-readable scaling — never on unbounded input.
#![allow(clippy::cast_precision_loss)]

use std::io::{IsTerminal, Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};

use nexus_core::app;
use nexus_core::config;
use nexus_core::db;
use nexus_core::host::{HostConfig, HostServer};
use nexus_core::provider::Model;
use nexus_core::provider::openrouter::OpenRouter;
use nexus_core::space;

/// Local-first terminal chat for deep research and multi-agent work.
#[derive(Parser)]
#[command(name = "nexus", version, about, max_term_width = 100)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// One-shot question from the shell — the answer streams to stdout and
    /// is saved as a normal session you can reopen in the TUI. With no
    /// prompt argument, the question is read from stdin (pipe-friendly).
    Ask {
        /// The question to ask (omit to read stdin).
        prompt: Option<String>,
        /// Model id as shown in the TUI's /model picker (defaults to your
        /// most recently used model).
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space; `new:NAME`
        /// creates the space on the fly.
        #[arg(long)]
        space: Option<String>,
        /// Web mode: search-first, inline-cited answers.
        #[arg(long)]
        web: bool,
        /// Emit the answer as JSON (answer, session, usage) instead of
        /// streaming it.
        #[arg(long)]
        json: bool,
        /// Suppress stderr chatter (tool status, token summary).
        #[arg(long)]
        quiet: bool,
    },
    /// A bare REPL: one turn per line, all turns in one session.
    Chat {
        /// Model id as shown in the TUI's /model picker.
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space.
        #[arg(long)]
        space: Option<String>,
        /// Suppress stderr chatter.
        #[arg(long)]
        quiet: bool,
    },
    /// Deep research from the shell: survey → plan → searchers → synthesis
    /// → critic → verifier → writer. Without --approve, a gated run parks
    /// at the survey/plan checkpoints (interactive when stdin is a
    /// terminal, an error otherwise).
    Research {
        /// What to research.
        topic: String,
        /// Model id to run the pipeline with.
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space.
        #[arg(long)]
        space: Option<String>,
        /// Skip the survey and plan-approval gates entirely (like /research!).
        #[arg(long)]
        approve: bool,
        /// Emit the report as JSON (report, session) instead of raw text.
        #[arg(long)]
        json: bool,
        /// Suppress stderr chatter (stage progress).
        #[arg(long)]
        quiet: bool,
    },
    /// Standing research watches: list, or run them from cron.
    Watch {
        #[command(subcommand)]
        cmd: WatchCmd,
    },
    /// Token/cache/cost analytics, like /usage.
    Usage {
        /// Window: 24h, 7d, 30d, or all.
        #[arg(long, default_value = "all")]
        range: String,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
        /// Rows in the per-model and per-day sections.
        #[arg(long, default_value = "10")]
        top: u64,
        /// Add a per-day breakdown, newest day first.
        #[arg(long)]
        by_day: bool,
    },
    /// List sessions, newest first.
    Sessions {
        /// Only this space's sessions.
        #[arg(long)]
        space: Option<String>,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<SessionsCmd>,
    },
    /// List spaces.
    Spaces {
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Print a session's latest report + sources (or its full transcript),
    /// like /export.
    Export {
        /// Session id, slug, or id prefix.
        session: String,
        /// Export the whole conversation instead of just the latest report.
        #[arg(long)]
        transcript: bool,
        /// Write to this path instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Zip the local data (db + spaces + skills) into one file.
    Backup {
        /// Destination path (default: `nexus-backup-<date>.zip` next to the
        /// data dir).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Restore a backup into the data dir, overwriting current data.
    Restore {
        /// The backup zip to restore.
        file: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Print (or edit) a space's memory file.
    Memory {
        /// Which space (default: your default space).
        #[arg(long)]
        space: Option<String>,
        /// Open the file in $EDITOR instead of printing it.
        #[arg(long)]
        edit: bool,
    },
    /// Print (or edit) a space's instructions file.
    Instructions {
        /// Which space (default: your default space).
        #[arg(long)]
        space: Option<String>,
        /// Open the file in $EDITOR instead of printing it.
        #[arg(long)]
        edit: bool,
    },
    /// List a space's imported files.
    Files {
        /// Which space (default: your default space).
        #[arg(long)]
        space: Option<String>,
    },
    /// Fetch and list the model catalogs (network).
    Models {
        /// Only this backend's models (openrouter / openai / opencode / codex).
        #[arg(long)]
        backend: Option<String>,
    },
    /// Save a provider API key to the config file.
    Login {
        /// openrouter, openai, or opencode.
        provider: String,
        /// The API key.
        key: String,
        /// Verify the key with a live catalog fetch after saving.
        #[arg(long)]
        check: bool,
    },
    /// List or install skills.
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    /// Jump into a session: launches the TUI, already in that conversation.
    Open {
        /// Session id, slug, or id prefix.
        session: String,
    },
    /// Update to the latest release from crates.io — runs
    /// `cargo install nexus-chat` when a newer version exists.
    Update,
    /// Sync with another device: export/import changesets, a shared-dir
    /// mailbox, or over ssh. `nexus sync <dir>` and `nexus sync ssh://host`
    /// are the two transports — see `nexus sync --help`.
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
        /// A shared sync directory, or `ssh://user@host`.
        target: Option<PathBuf>,
    },
    /// Run the local HTTP/SSE daemon and provider gateway.
    Host {
        /// Loopback port for the host API.
        #[arg(long, default_value_t = 8643)]
        port: u16,
        /// Start a quick `cloudflared` tunnel and print its public URL.
        #[arg(long)]
        tunnel: bool,
        /// Do not hold the machine awake while hosting.
        #[arg(long)]
        no_sleep_guard: bool,
        /// Provision a named Cloudflare tunnel using `CF_API_TOKEN`.
        #[arg(long)]
        setup: bool,
    },
    /// Show data/config paths and which providers are configured.
    Status,
    /// Deeper diagnostics: db integrity, config, optional tools,
    /// `--network` also pings your providers. Exits non-zero on problems.
    Doctor {
        /// Also verify configured provider keys with live calls.
        #[arg(long)]
        network: bool,
    },
}

#[derive(Subcommand)]
pub enum WatchCmd {
    /// List watches (topic, interval, due state).
    List,
    /// Run watches' research now: the given watch, all watches (`--all`),
    /// or the due ones (default).
    Run {
        /// Watch id prefix or topic substring; default: due watches.
        watch: Option<String>,
        /// Run every watch, due or not.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum SessionsCmd {
    /// Delete one session (by id, slug, or id prefix).
    Rm {
        /// Session id, slug, or id prefix.
        session: String,
    },
    /// Delete old sessions.
    Prune {
        /// Keep the N most recent sessions per space.
        #[arg(long)]
        keep: Option<u64>,
        /// Delete sessions older than this many days.
        #[arg(long)]
        days: Option<u64>,
        /// Print what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// List installed skills.
    List,
    /// Install a skill from GitHub (`owner/repo` or `owner/repo/path`).
    Install {
        /// `owner/repo[/path]` shorthand.
        skill: String,
    },
}

#[derive(Subcommand)]
pub enum SyncCmd {
    /// Export a changeset (JSON on stdout). `--dir` also writes the file
    /// blobs into `DIR/blobs/`. Without a peer, exports everything.
    Export {
        /// Which peer to export for (the other device's id, from its
        /// changesets). Full export when unknown.
        #[arg(long)]
        peer: Option<String>,
        /// Also write the blob payloads into this directory.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Import a changeset or bundle (stdin when no file is given) and
    /// print the reply — your own export + ack — on stdout. Zip bundles
    /// (the ssh transport) are detected by magic bytes.
    Import {
        /// Changeset JSON or bundle zip; stdin when omitted.
        path: Option<PathBuf>,
        /// Reply as a bundle (zip with blobs) instead of plain JSON.
        #[arg(long)]
        bundle: bool,
    },
}

/// Parse argv and return the requested subcommand. `None` means no
/// subcommand — the caller launches the TUI. Help/version/parse errors are
/// handled by clap (it exits the process).
pub fn parse() -> Option<Command> {
    Cli::parse().command
}

/// Run a parsed subcommand.
pub async fn run(cmd: Command) -> Result<()> {
    match cmd {
        Command::Ask {
            prompt,
            model,
            space,
            web,
            json,
            quiet,
        } => ask(prompt, model, space, web, json, quiet).await,
        Command::Chat {
            model,
            space,
            quiet,
        } => chat(model, space, quiet).await,
        Command::Research {
            topic,
            model,
            space,
            approve,
            json,
            quiet,
        } => research(topic, model, space, approve, json, quiet).await,
        Command::Watch { cmd } => watch(cmd).await,
        Command::Usage {
            range,
            json,
            top,
            by_day,
        } => usage(&range, json, top, by_day),
        Command::Sessions { space, json, cmd } => sessions(space.as_deref(), json, cmd),
        Command::Spaces { json } => spaces(json),
        Command::Export {
            session,
            transcript,
            output,
        } => export(&session, transcript, output.as_deref()),
        Command::Backup { output } => backup(output.as_deref()),
        Command::Restore { file, yes } => restore(&file, yes),
        Command::Memory { space, edit } => memory(space.as_deref(), edit),
        Command::Instructions { space, edit } => instructions(space.as_deref(), edit),
        Command::Files { space } => files(space.as_deref()),
        Command::Models { backend } => models(backend.as_deref()).await,
        Command::Login {
            provider,
            key,
            check,
        } => login(&provider, &key, check).await,
        Command::Skills { cmd } => skills(cmd).await,
        Command::Open { session } => open(&session),
        Command::Sync { cmd, target } => sync_cmd(cmd, target.as_deref()),
        Command::Host {
            port,
            tunnel,
            no_sleep_guard,
            setup,
        } => host(port, tunnel, no_sleep_guard, setup).await,
        Command::Update => update().await,
        Command::Status => status(),
        Command::Doctor { network } => doctor(network).await,
    }
}

// --- app-booting commands (ask / chat / research / watch run) ---

/// Boot the same `App` the TUI boots (creds, backends, app server, toolbox)
/// and apply the shared `--model` / `--space` overrides.
async fn build_app(model: Option<&str>, space_name: Option<&str>) -> Result<app::App> {
    let saved = config::load_all_providers().await?;
    let mut app = nexus_core::boot(saved).await?;
    if let Some(name) = space_name {
        app.execute(nexus_core::app::AppCommand::SwitchSpace {
            name: name.to_string(),
        })?;
    }
    if let Some(m) = model {
        // Direct poke: no model catalog is fetched headless, so SetModel's
        // resolution has nothing to resolve against. The next turn validates
        // the backend.
        app.current_model = Some(m.to_string());
    }
    Ok(app)
}

/// `--space new:NAME` support: create the space if it doesn't exist yet.
fn ensure_space(name: &str) -> Result<()> {
    let name = name
        .strip_prefix("new:")
        .context("space must be an existing name or new:NAME")?;
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    if db.list_spaces()?.iter().any(|s| s.name == name) {
        return Ok(());
    }
    db.create_space(name)?;
    space.ensure_space_dir(name)?;
    Ok(())
}

async fn ask(
    prompt: Option<String>,
    model: Option<String>,
    space_name: Option<String>,
    web: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let prompt = if let Some(p) = prompt {
        p
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    };
    if prompt.trim().is_empty() {
        bail!("empty prompt — pass the question as an argument or pipe it on stdin");
    }
    // `--space new:NAME` creates the space before the app boots.
    let space_name = match space_name {
        Some(n) if n.starts_with("new:") => {
            ensure_space(&n)?;
            Some(n["new:".len()..].to_string())
        }
        n => n,
    };
    let mut app = build_app(model.as_deref(), space_name.as_deref()).await?;
    if web {
        // Boot always starts with web mode off, so the toggle is absolute here.
        app.execute(nexus_core::app::AppCommand::ToggleWeb)?;
    }
    let outcome = app
        .ask_headless(
            prompt,
            app::headless::TurnOpts {
                stream: !json,
                quiet,
            },
        )
        .await?;
    if json {
        let v = serde_json::json!({
            "answer": outcome.answer,
            "session_id": outcome.session_id,
            "session_title": outcome.session_title,
            "usage": outcome.usage.map(|u| serde_json::json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
                "cache_read_tokens": u.cache_read_tokens,
                "cache_creation_tokens": u.cache_creation_tokens,
                "cost": u.cost,
            })),
        });
        out(serde_json::to_string_pretty(&v)?);
    } else if !quiet {
        eprintln!(
            "saved as \"{}\" — reopen it in the TUI with /sessions (id {})",
            outcome.session_title,
            short_id(&outcome.session_id)
        );
    }
    Ok(())
}

async fn chat(model: Option<String>, space_name: Option<String>, quiet: bool) -> Result<()> {
    let mut app = build_app(model.as_deref(), space_name.as_deref()).await?;
    app.chat_headless(quiet).await
}

async fn research(
    topic: String,
    model: Option<String>,
    space_name: Option<String>,
    approve: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    if topic.trim().is_empty() {
        bail!("empty topic — pass what to research as the first argument");
    }
    let mut app = build_app(model.as_deref(), space_name.as_deref()).await?;
    let outcome = app
        .research_headless(
            topic,
            approve,
            app::headless::TurnOpts {
                stream: false,
                quiet,
            },
        )
        .await?;
    if json {
        let v = serde_json::json!({
            "report": outcome.report,
            "session_id": outcome.session_id,
            "session_title": outcome.session_title,
        });
        out(serde_json::to_string_pretty(&v)?);
    } else {
        out(outcome.report);
        if !quiet {
            eprintln!(
                "saved as \"{}\" — reopen it in the TUI with /sessions (id {})",
                outcome.session_title,
                short_id(&outcome.session_id)
            );
        }
    }
    Ok(())
}

async fn watch(cmd: WatchCmd) -> Result<()> {
    match cmd {
        WatchCmd::List => watch_list(),
        WatchCmd::Run { watch, all } => watch_run(watch.as_deref(), all).await,
    }
}

fn watch_list() -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let watches = db.list_all_watches()?;
    if watches.is_empty() {
        out("no watches yet — create one in the TUI with /watches");
        return Ok(());
    }
    let spaces = db.list_spaces()?;
    let now = chrono::Utc::now();
    for w in watches {
        let space_name = spaces
            .iter()
            .find(|s| s.id == w.space_id)
            .map_or("?", |s| s.name.as_str());
        let due = match &w.last_run_at {
            None => "due now (never run)".to_string(),
            Some(t) => {
                let next = chrono::DateTime::parse_from_rfc3339(t).map_or(now, |last| {
                    last.with_timezone(&chrono::Utc) + chrono::Duration::hours(w.interval_hours)
                });
                if next <= now {
                    "due now".to_string()
                } else {
                    format!("next {}", next.format("%Y-%m-%d %H:%M"))
                }
            }
        };
        out(format!(
            "{:<10} {:<8} {:<32} every {:>3}h  {:<19}  session {}",
            short_id(&w.id),
            truncate(space_name, 8),
            truncate(&w.topic, 32),
            w.interval_hours,
            due,
            short_id(&w.session_id),
        ));
    }
    Ok(())
}

async fn watch_run(watch_ref: Option<&str>, all: bool) -> Result<()> {
    let mut app = build_app(None, None).await?;
    let ran = app.watch_run_headless(watch_ref, all, false).await?;
    for (topic, outcome) in ran {
        out(format!("\n## {topic}\n"));
        out(outcome.report);
        eprintln!(
            "watch \"{topic}\" → session {} (id {})",
            outcome.session_title,
            short_id(&outcome.session_id)
        );
    }
    Ok(())
}

// --- read-only commands ---

fn usage(range_key: &str, json: bool, top: u64, by_day: bool) -> Result<()> {
    let space = space::Space::open()?;
    let mut db = db::Db::open(&space.db_path())?;
    let _ = db.backfill_usage_costs(); // price rows logged before pricing existed
    let range = db::UsageRange::from_key(range_key);
    let since = range.since().map(|t| t.to_rfc3339());
    let totals = db.usage_totals(since.as_deref())?;
    if json {
        return usage_json(&db, range, since.as_deref(), &totals, top, by_day);
    }

    out(format!("usage — {}", range.label()));
    if totals.requests == 0 {
        out(range.empty_message());
        return Ok(());
    }
    out(format!("requests: {}", fmt_req(totals.requests)));
    out(format!(
        "prompt: {}   completion: {}   cached reads: {}   cached writes: {}",
        fmt_tokens(totals.prompt_tokens),
        fmt_tokens(totals.completion_tokens),
        fmt_tokens(totals.cache_read_tokens),
        fmt_tokens(totals.cache_creation_tokens),
    ));
    out(format!("cost: ${:.4}", totals.cost));

    let by_backend = db.usage_by_backend(since.as_deref())?;
    if !by_backend.is_empty() {
        out("");
        out("by backend");
        for b in by_backend {
            out(format!(
                "  {:<24} {:>8} req   prompt {:<8} cached {:<8} ${:.4}",
                truncate(&b.backend, 24),
                fmt_req(b.requests),
                fmt_tokens(b.prompt_tokens),
                fmt_tokens(b.cache_read_tokens),
                b.cost,
            ));
        }
    }
    let by_model = db.usage_by_model(top, since.as_deref())?;
    if !by_model.is_empty() {
        out("");
        out(format!("by model (top {top})"));
        for m in by_model {
            out(format!(
                "  {:<32} {:>8} req   prompt {:<8} cached {:<8} ${:.4}",
                truncate(&m.model, 32),
                fmt_req(m.requests),
                fmt_tokens(m.prompt_tokens),
                fmt_tokens(m.cache_read_tokens),
                m.cost,
            ));
        }
    }
    if by_day {
        let days = db.usage_by_day(top, since.as_deref())?;
        if !days.is_empty() {
            out("");
            out(format!("by day (top {top})"));
            for d in days {
                out(format!(
                    "  {:<12} {:>8} req   prompt {:<8} completion {:<8} ${:.4}",
                    d.day,
                    fmt_req(d.requests),
                    fmt_tokens(d.prompt_tokens),
                    fmt_tokens(d.completion_tokens),
                    d.cost,
                ));
            }
        }
    }
    let recent = db.usage_recent(8, since.as_deref())?;
    if !recent.is_empty() {
        out("");
        out("recent requests");
        for r in recent {
            let cost = r.cost.map(|c| format!(" ${c:.4}")).unwrap_or_default();
            out(format!(
                "  {}  {:<16}  {:<32}  {}→{}  {}",
                fmt_ts(&r.created_at),
                truncate(&r.backend, 16),
                truncate(&r.model, 32),
                fmt_tokens(r.prompt_tokens),
                fmt_tokens(r.completion_tokens),
                cost,
            ));
        }
    }
    Ok(())
}

/// The `--json` variant of `usage` — the same aggregates, machine-readable.
fn usage_json(
    db: &db::Db,
    range: db::UsageRange,
    since: Option<&str>,
    totals: &db::UsageTotals,
    top: u64,
    by_day: bool,
) -> Result<()> {
    let by_backend = db.usage_by_backend(since)?;
    let by_model = db.usage_by_model(top, since)?;
    let recent = db.usage_recent(8, since)?;
    let mut v = serde_json::json!({
        "range": range.label(),
        "totals": {
            "requests": totals.requests,
            "prompt_tokens": totals.prompt_tokens,
            "completion_tokens": totals.completion_tokens,
            "cache_read_tokens": totals.cache_read_tokens,
            "cache_creation_tokens": totals.cache_creation_tokens,
            "cost": totals.cost,
        },
        "by_backend": by_backend.iter().map(|b| serde_json::json!({
            "backend": b.backend, "requests": b.requests,
            "prompt_tokens": b.prompt_tokens, "completion_tokens": b.completion_tokens,
            "cache_read_tokens": b.cache_read_tokens, "cost": b.cost,
        })).collect::<Vec<_>>(),
        "by_model": by_model.iter().map(|m| serde_json::json!({
            "model": m.model, "requests": m.requests,
            "prompt_tokens": m.prompt_tokens, "completion_tokens": m.completion_tokens,
            "cache_read_tokens": m.cache_read_tokens, "cost": m.cost,
        })).collect::<Vec<_>>(),
        "recent": recent.iter().map(|r| serde_json::json!({
            "created_at": r.created_at, "backend": r.backend, "model": r.model,
            "prompt_tokens": r.prompt_tokens, "completion_tokens": r.completion_tokens,
            "cache_read_tokens": r.cache_read_tokens, "cost": r.cost,
        })).collect::<Vec<_>>(),
    });
    if by_day {
        let days = db.usage_by_day(top, since)?;
        v["by_day"] = serde_json::json!(
            days.iter()
                .map(|d| serde_json::json!({
                    "day": d.day, "requests": d.requests,
                    "prompt_tokens": d.prompt_tokens, "completion_tokens": d.completion_tokens,
                    "cache_read_tokens": d.cache_read_tokens, "cost": d.cost,
                }))
                .collect::<Vec<_>>()
        );
    }
    out(serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn sessions(space_name: Option<&str>, json: bool, cmd: Option<SessionsCmd>) -> Result<()> {
    match cmd {
        Some(SessionsCmd::Rm { session }) => sessions_rm(&session),
        Some(SessionsCmd::Prune {
            keep,
            days,
            dry_run,
        }) => sessions_prune(keep, days, dry_run),
        None => sessions_list(space_name, json),
    }
}

fn sessions_list(space_name: Option<&str>, json: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let space_id = match space_name {
        Some(name) => db
            .list_spaces()?
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .ok_or_else(|| anyhow!("no space named {name:?} — `nexus spaces` lists them"))?,
        None => db.default_space_id()?,
    };
    let sessions = db.list_sessions(&space_id)?;
    if sessions.is_empty() {
        out("no sessions yet — ask something in the TUI or with `nexus ask`");
        return Ok(());
    }
    if json {
        let v: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id, "title": s.title, "model": s.model,
                    "slug": s.slug, "kind": s.kind, "created_at": s.created_at,
                })
            })
            .collect();
        out(serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for s in sessions {
        out(format!(
            "{:<8}  {:<40}  {:<32}  {:<8}  {}",
            short_id(&s.id),
            truncate(&s.title, 40),
            truncate(&s.model, 32),
            s.kind,
            fmt_ts(&s.created_at),
        ));
    }
    Ok(())
}

fn sessions_rm(reference: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (_, session) = resolve_session(&db, reference)?;
    db.delete_session(&session.id)?;
    out(format!(
        "deleted session \"{}\" ({})",
        session.title,
        short_id(&session.id)
    ));
    Ok(())
}

fn sessions_prune(keep: Option<u64>, days: Option<u64>, dry_run: bool) -> Result<()> {
    if keep.is_none() && days.is_none() {
        bail!("pass --keep N and/or --days N to choose what to prune");
    }
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let now = chrono::Utc::now();
    let mut doomed: Vec<db::Session> = Vec::new();
    for sp in db.list_spaces()? {
        let sessions = db.list_sessions(&sp.id)?; // newest first
        doomed.extend(
            prune_targets(&sessions, keep, days, now)
                .into_iter()
                .cloned(),
        );
    }
    if doomed.is_empty() {
        out("nothing to prune");
        return Ok(());
    }
    for s in &doomed {
        out(format!(
            "{}{:<8}  {:<40}  {}",
            if dry_run {
                "would delete "
            } else {
                "deleted     "
            },
            short_id(&s.id),
            truncate(&s.title, 40),
            fmt_ts(&s.created_at),
        ));
        if !dry_run {
            db.delete_session(&s.id)?;
        }
    }
    out(format!(
        "{} {} session(s)",
        if dry_run { "would prune" } else { "pruned" },
        doomed.len()
    ));
    Ok(())
}

fn spaces(json: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let rows = db.list_spaces()?;
    if rows.is_empty() {
        out("no spaces yet");
        return Ok(());
    }
    if json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "name": r.name, "created_at": r.created_at,
                    "sessions": db.count_sessions(&r.id).unwrap_or(0),
                })
            })
            .collect();
        out(serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let default_id = db.default_space_id().unwrap_or_default();
    for r in rows {
        let sessions = db.count_sessions(&r.id).unwrap_or(0);
        let marker = if r.id == default_id { " (default)" } else { "" };
        out(format!(
            "{:<8}  {:<24}  {:>4} sessions  {}{}",
            short_id(&r.id),
            truncate(&r.name, 24),
            sessions,
            fmt_ts(&r.created_at),
            marker,
        ));
    }
    Ok(())
}

/// Print a session's latest assistant reply (its research report, for
/// research sessions) plus the sources it actually cites — the same
/// assembly as the TUI's /export — or, with `--transcript`, the whole
/// conversation.
fn export(reference: &str, transcript: bool, output: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (space_id, session) = resolve_session(&db, reference)?;
    let messages = db.load_messages(&session.id)?;
    let assembled = if transcript {
        transcript_markdown(&session, &messages)
    } else {
        let report = messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.clone())
            .ok_or_else(|| {
                anyhow!(
                    "session {} has no assistant reply yet",
                    short_id(&session.id)
                )
            })?;
        // Citations are space-wide; keep only the ones this report cites, so
        // an export never carries another session's sources (matches /export).
        let citations = db.search_citations(&space_id, None)?;
        let urls_in_report: std::collections::HashSet<String> =
            nexus_core::citations::parse_citations(&report)
                .into_iter()
                .map(|(_, url)| url)
                .collect();
        let cited: Vec<(String, String, String)> = citations
            .into_iter()
            .filter(|(_, url, _)| urls_in_report.contains(url))
            .collect();
        app::export::assemble_report(&report, &cited)
    };
    match output {
        Some(path) => {
            std::fs::write(path, &assembled)
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!("exported to {}", path.display());
        }
        None => out(assembled),
    }
    Ok(())
}

/// The whole conversation as markdown: one `## role` section per message,
/// tool calls as JSON code blocks.
fn transcript_markdown(session: &db::Session, messages: &[db::Message]) -> String {
    let mut out = format!("# {}\n\n", session.title);
    for m in messages {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("## {}\n\n", m.role));
        if m.role == "tool_call" {
            out.push_str("```json\n");
            out.push_str(&m.content);
            out.push_str("\n```\n\n");
        } else {
            let _ =
                std::fmt::Write::write_fmt(&mut out, format_args!("{}\n\n", m.content.trim_end()));
        }
    }
    out
}

/// Which sessions would be pruned. `keep` keeps the N most recent per
/// space; `days` drops anything older than the cutoff; with both, either
/// rule alone is enough to delete. `sessions` must be newest-first.
fn prune_targets(
    sessions: &[db::Session],
    keep: Option<u64>,
    days: Option<u64>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<&db::Session> {
    let cutoff = days.map(|d| now - chrono::Duration::days(d.cast_signed()));
    sessions
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            let too_old = cutoff.is_some_and(|c| {
                chrono::DateTime::parse_from_rfc3339(&s.created_at).map_or(true, |t| t < c)
            });
            let beyond_keep = keep.is_some_and(|k| *i >= usize::try_from(k).unwrap_or(usize::MAX));
            too_old || beyond_keep
        })
        .map(|(_, s)| s)
        .collect()
}

// --- local data commands ---

fn memory(space_name: Option<&str>, edit: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    show_or_edit(&space.memory_path(&name), "memory", edit)
}

fn instructions(space_name: Option<&str>, edit: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    show_or_edit(&space.instructions_path(&name), "instructions", edit)
}

fn show_or_edit(path: &Path, label: &str, edit: bool) -> Result<()> {
    if edit {
        if !path.exists() {
            std::fs::write(
                path,
                format!(
                    "<!-- {label} for this space — everything here is visible to the model -->\n"
                ),
            )
            .with_context(|| format!("writing {}", path.display()))?;
        }
        open_editor(path)?;
        return Ok(());
    }
    match std::fs::read_to_string(path) {
        Ok(text) => out(text.trim_end()),
        Err(_) => out(format!("(no {label} yet — run with --edit to create it)")),
    }
    Ok(())
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("running $EDITOR ({editor})"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

fn files(space_name: Option<&str>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let name = resolve_space_name(&db, space_name)?;
    let space_id = db
        .list_spaces()?
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
        .context("space vanished")?;
    let rows = db.list_files(&space_id)?;
    if rows.is_empty() {
        out(format!(
            "no files imported in \"{name}\" yet — drop files into {}",
            space.files_dir(&name).display()
        ));
        return Ok(());
    }
    for f in rows {
        out(format!(
            "{:<40} {:>10}  {}",
            truncate(&f.name, 40),
            human_size(f.size.cast_unsigned()),
            f.status,
        ));
    }
    Ok(())
}

fn backup(output: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let default = space.root.parent().unwrap_or(&space.root).join(format!(
        "nexus-backup-{}.zip",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    let path = output.unwrap_or(&default);
    zip_dir(&space.root, path)?;
    out(format!("backed up to {}", path.display()));
    Ok(())
}

fn restore(file: &Path, yes: bool) -> Result<()> {
    let space = space::Space::open()?;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("restore overwrites your local data — pass --yes to confirm");
        }
        eprint!("restore {} over current data? [y/N] ", file.display());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            bail!("aborted");
        }
    }
    // The device-local cache.db is keyed to the pre-restore db: stale chunks
    // and embeddings must not survive the restore. Drop it before unzipping,
    // then again after (pre-split backups may contain one) — the next launch
    // recreates it empty and re-indexes on demand.
    let cache_path = space.root.join("cache.db");
    let _ = std::fs::remove_file(&cache_path);
    unzip_into(file, &space.root)?;
    let _ = std::fs::remove_file(&cache_path);
    out("restored");
    Ok(())
}

/// Zip a directory tree into `dst` (relative paths, deflated).
fn zip_dir(src: &Path, dst: &Path) -> Result<()> {
    let file = std::fs::File::create(dst).with_context(|| format!("creating {}", dst.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip_entries(&mut zip, src, src, opts)?;
    zip.finish()?;
    Ok(())
}

fn zip_entries(
    zip: &mut zip::ZipWriter<std::fs::File>,
    root: &Path,
    dir: &Path,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("path outside root: {}", path.display()))?;
        // The device-local cache.db is excluded from backups by design — it
        // is disposable derived state that rebuilds on demand.
        if rel == std::path::Path::new("cache.db") {
            continue;
        }
        if entry.file_type()?.is_dir() {
            zip.add_directory(format!("{}/", rel.display()), opts)?;
            zip_entries(zip, root, &path, opts)?;
        } else {
            zip.start_file(rel.display().to_string(), opts)?;
            let mut f = std::fs::File::open(&path)?;
            std::io::copy(&mut f, zip)?;
        }
    }
    Ok(())
}

/// Extract a backup zip into `dest_root`, refusing any entry that could
/// escape it (absolute paths, `..`, drive prefixes).
fn unzip_into(archive_path: &Path, dest_root: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)
        .with_context(|| format!("opening {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {}", archive_path.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        let rel = Path::new(&name);
        if rel.is_absolute()
            || rel.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("refusing unsafe path in archive: {name}");
        }
        let dest = dest_root.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    Ok(())
}

// --- provider & skill commands ---

async fn models(backend: Option<&str>) -> Result<()> {
    let mut app = build_app(None, None).await?;
    if !app.backends.any() {
        bail!("no API keys configured — `nexus login <provider> <key>` or set the env vars");
    }
    app.init();
    // The catalog fetch is a network call; cap the wait so a hung provider
    // can't stall the command forever.
    let deadline = tokio::time::sleep(std::time::Duration::from_mins(1));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => bail!("timed out fetching model catalogs"),
            ev = app.next_event() => {
                if let app::AppEvent::Models(r) = ev {
                    app.on_models_result(r);
                    break;
                }
            },
        }
    }
    let mut models: Vec<&Model> = app
        .models
        .iter()
        .filter(|m| {
            // Match the CLI spelling (openrouter/openai/opencode/codex) or
            // the catalog display name ("OpenRouter", …), case-insensitive.
            let prefix = m.backend.key_prefix().trim_end_matches(':');
            backend.is_none_or(|b| {
                prefix.eq_ignore_ascii_case(b) || m.backend.name().eq_ignore_ascii_case(b)
            })
        })
        .collect();
    if app.models.is_empty() {
        // The fetch itself failed — status carries "model fetch failed: …".
        // The status is an event now (2e); drain locally-queued events and
        // surface the last status line.
        let mut status = String::new();
        while let Some(ev) = app.pop_pending_event() {
            if let app::AppEvent::Status(s) = ev {
                status = s;
            }
        }
        bail!("{status}");
    }
    if models.is_empty() {
        bail!("no models from backend {backend:?} — `nexus models` lists them");
    }
    models.sort_by(|a, b| a.backend.name().cmp(b.backend.name()).then(a.id.cmp(&b.id)));
    for m in models {
        let ctx = m
            .context_length
            .map_or_else(|| "—".to_string(), |c| format!("{}k", c / 1000));
        out(format!(
            "{:<44} {:<10} {:>7}  {}",
            truncate(&m.id, 44),
            m.backend.name(),
            ctx,
            if m.supports_images { "images" } else { "" },
        ));
    }
    Ok(())
}

async fn login(provider: &str, key: &str, check: bool) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("empty key");
    }
    if !matches!(provider, "openrouter" | "openai" | "opencode") {
        bail!("unknown provider {provider:?} — use openrouter, openai, or opencode");
    }
    config::save_provider_key(provider, key)?;
    out(format!("saved {provider} key"));
    if check {
        let client = OpenRouter::from_key_auto(key.to_string());
        match client.list_models().await {
            Ok(_) => out("key check: ok"),
            Err(e) => bail!("key check failed: {e}"),
        }
    }
    Ok(())
}

async fn skills(cmd: SkillsCmd) -> Result<()> {
    match cmd {
        SkillsCmd::List => skills_list(),
        SkillsCmd::Install { skill } => skills_install(&skill).await,
    }
}

fn skills_list() -> Result<()> {
    let space = space::Space::open()?;
    let skills = nexus_core::skills::load_skills(&nexus_core::skills::skills_dir(&space.root));
    if skills.is_empty() {
        out("no skills installed — `nexus skills install <owner/repo[/path]>`");
        return Ok(());
    }
    for s in skills {
        out(format!(
            "{:<32} {}",
            truncate(&s.name, 32),
            truncate(&s.description, 60),
        ));
    }
    Ok(())
}

async fn skills_install(spec: &str) -> Result<()> {
    let Some((owner, repo, path)) = nexus_core::skills::parse_gh_shorthand(spec) else {
        bail!("expected owner/repo[/path] shorthand, got {spec:?}");
    };
    let space = space::Space::open()?;
    let dest = nexus_core::skills::skills_dir(&space.root);
    let name = nexus_core::skills::install_from_github(
        &reqwest::Client::new(),
        &owner,
        &repo,
        &path,
        &dest,
    )
    .await?;
    out(format!("installed skill {name}"));
    Ok(())
}

// --- host daemon (Phase 4) ---

enum HostTunnel {
    Quick {
        port: u16,
    },
    Named {
        config_path: PathBuf,
        tunnel_id: String,
    },
}

async fn start_host_tunnel(spec: &HostTunnel) -> Result<nexus_core::host::process::Tunnel> {
    match spec {
        HostTunnel::Quick { port } => nexus_core::host::process::Tunnel::quick(*port).await,
        HostTunnel::Named {
            config_path,
            tunnel_id,
        } => nexus_core::host::process::Tunnel::named(config_path, tunnel_id),
    }
}

async fn wait_host_tunnel(tunnel: &mut Option<nexus_core::host::process::Tunnel>) {
    if let Some(tunnel) = tunnel.as_mut() {
        let _ = tunnel.wait().await;
    }
}

/// Run the headless app actor, HTTP/SSE API, optional tunnel, and sleep guard.
#[allow(clippy::too_many_lines)]
async fn host(port: u16, quick_tunnel: bool, no_sleep_guard: bool, setup: bool) -> Result<()> {
    let saved_named = config::load_named_tunnel()?;
    let named = if setup {
        Some(setup_named_tunnel(port).await?)
    } else if quick_tunnel {
        saved_named
            .as_ref()
            .filter(|tunnel| tunnel.credentials_path.is_file() && tunnel.config_path.is_file())
            .map(|tunnel| reuse_named_tunnel(tunnel, port))
            .transpose()?
    } else {
        None
    };
    // Providers first: `load_all_providers` only scaffolds the commented
    // credential template when no config exists yet, and `ensure_host_token`
    // creates the file. Minting the token first would leave a first-ever
    // `nexus host` run with a config holding nothing but `host_token`.
    let saved = config::load_all_providers().await?;
    let token = config::ensure_host_token()?;
    let mut app = nexus_core::boot(saved).await?;
    app.init();
    let mut server = HostServer::bind(app, HostConfig::new(port, token.clone())).await?;
    let local_url = format!("http://{}", server.local_addr());

    let tunnel_spec = if let Some(named) = &named {
        Some(HostTunnel::Named {
            config_path: named.config_path.clone(),
            tunnel_id: named.tunnel_id.clone(),
        })
    } else if quick_tunnel {
        nexus_core::host::process::require_cloudflared()?;
        Some(HostTunnel::Quick {
            port: server.local_addr().port(),
        })
    } else {
        // No sidecar: the server remains reachable on loopback and all
        // shutdown cleanup is still centralized below.
        None
    };
    let mut tunnel = match tunnel_spec.as_ref() {
        Some(spec) => Some(start_host_tunnel(spec).await?),
        None => None,
    };
    let public_host = named
        .as_ref()
        .map(|result| format!("https://{}", result.hostname))
        .or_else(|| {
            tunnel
                .as_ref()
                .and_then(|sidecar| sidecar.public_url().map(str::to_string))
        });
    if let Some(base) = &public_host {
        // Browser navigations cannot send Authorization. App links therefore
        // carry the same token as a query parameter; `/v1/*` remains header-only.
        server.set_public_base(Some(base.clone())).await?;
        out(format!("public host: {base}"));
        let healthy = nexus_core::host::process::health_check(base).await;
        out(if healthy {
            "public host health check: reachable"
        } else {
            "warning: public host health check failed; tunnel may still be starting"
        });
    } else if quick_tunnel || setup {
        out("tunnel started, but no public URL was discovered");
    }
    let enrollment_host = public_host.as_deref().unwrap_or(&local_url);
    let enrollment = format!("nexus://host={enrollment_host}&token={token}");
    out(format!("host listening at {local_url}"));
    out(format!("enrollment: {enrollment}"));
    match qrcode::QrCode::new(enrollment.as_bytes()) {
        Ok(code) => {
            let qr = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            out(qr);
        }
        Err(error) => out(format!("could not render enrollment QR: {error}")),
    }

    let mut sleep_guard = if no_sleep_guard {
        None
    } else {
        match nexus_core::host::process::sleep_guard() {
            Ok(guard) => guard,
            Err(error) => {
                out(format!("sleep guard unavailable: {error:#}"));
                None
            }
        }
    };
    if nexus_core::host::process::on_battery() == Some(true) {
        out("warning: hosting while on battery power");
    }
    let mut restart_delay = 1u64;
    loop {
        let exited = if tunnel.is_some() {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { let _ = result; false },
                () = wait_host_tunnel(&mut tunnel) => true,
            }
        } else {
            tokio::signal::ctrl_c()
                .await
                .context("waiting for shutdown")?;
            false
        };
        if !exited {
            break;
        }
        out("tunnel exited; restarting with backoff…");
        tunnel = None;
        let Some(spec) = tunnel_spec.as_ref() else {
            break;
        };
        tokio::time::sleep(std::time::Duration::from_secs(restart_delay)).await;
        match start_host_tunnel(spec).await {
            Ok(next) => {
                if let Some(base) = next.public_url().map(str::to_string) {
                    let _ = server.set_public_base(Some(base.clone())).await;
                    out(format!("new public host: {base}"));
                    restart_delay = 1;
                } else if matches!(spec, HostTunnel::Quick { .. }) {
                    // A replacement quick tunnel gets a *new* hostname, so a
                    // restart with no discovered URL means the advertised one
                    // is dead with nothing to replace it: stop handing it out.
                    // Keep backing off too — spawning succeeds even when
                    // cloudflared exits immediately, and resetting the delay
                    // here would respawn it once a second forever.
                    let _ = server.set_public_base(None).await;
                    out("tunnel restarted without a public URL; app links fall back to loopback");
                    restart_delay = (restart_delay * 2).min(30);
                } else {
                    // Named tunnels keep the hostname chosen during setup.
                    restart_delay = 1;
                }
                tunnel = Some(next);
            }
            Err(error) => {
                out(format!("tunnel restart failed: {error:#}"));
                restart_delay = (restart_delay * 2).min(30);
            }
        }
    }
    out("shutting down host…");
    let _ = server.set_public_base(None).await;
    if let Some(sidecar) = tunnel.as_mut() {
        sidecar.stop().await;
    }
    if let Some(guard) = sleep_guard.as_mut() {
        guard.stop().await;
    }
    server.shutdown().await;
    Ok(())
}

fn reuse_named_tunnel(
    saved: &config::NamedTunnelConfig,
    port: u16,
) -> Result<nexus_core::host::cloudflare::SetupResult> {
    nexus_core::host::cloudflare::write_named_config(
        &saved.config_path,
        &saved.credentials_path,
        &saved.tunnel_id,
        &saved.hostname,
        port,
    )?;
    Ok(nexus_core::host::cloudflare::SetupResult {
        tunnel_id: saved.tunnel_id.clone(),
        hostname: saved.hostname.clone(),
        credentials_path: saved.credentials_path.clone(),
        config_path: saved.config_path.clone(),
    })
}

/// Provision a named tunnel interactively when account/zone IDs were not
/// supplied through `CF_ACCOUNT_ID`/`CF_ZONE_ID`. Existing local tunnel
/// configuration is reused without contacting Cloudflare.
async fn setup_named_tunnel(port: u16) -> Result<nexus_core::host::cloudflare::SetupResult> {
    use nexus_core::host::cloudflare::{
        SetupOptions, list_accounts, list_zones, provision_named_tunnel,
    };
    if let Some(saved) = config::load_named_tunnel()?
        && saved.credentials_path.is_file()
        && saved.config_path.is_file()
    {
        return reuse_named_tunnel(&saved, port);
    }
    // An env override skips both the prompt *and* its listing call — an
    // empty value is treated as unset rather than sent to the API as an id.
    let env_id = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let account_id = if let Some(id) = env_id("CF_ACCOUNT_ID") {
        id
    } else {
        let accounts = list_accounts().await?;
        if accounts.is_empty() {
            bail!("Cloudflare API returned no accounts");
        }
        out("Cloudflare accounts:");
        for (index, account) in accounts.iter().enumerate() {
            out(format!(
                "  {}. {} ({})",
                index + 1,
                account.name,
                account.id
            ));
        }
        accounts[choose_index("account number", accounts.len())? - 1]
            .id
            .clone()
    };
    let (zone_id, zone_name) = if let Some(id) = env_id("CF_ZONE_ID") {
        (id, None)
    } else {
        let zones = list_zones().await?;
        if zones.is_empty() {
            bail!("Cloudflare API returned no DNS zones");
        }
        out("Cloudflare zones:");
        for (index, zone) in zones.iter().enumerate() {
            out(format!("  {}. {} ({})", index + 1, zone.name, zone.id));
        }
        let zone = &zones[choose_index("zone number", zones.len())? - 1];
        (zone.id.clone(), Some(zone.name.clone()))
    };
    let hostname = env_id("CF_HOSTNAME").unwrap_or_else(|| {
        let suffix = zone_name.as_deref().unwrap_or("example.com");
        prompt("hostname", &format!("nexus.{suffix}"))
    });
    let tunnel_name = env_id("CF_TUNNEL_NAME").unwrap_or_else(|| prompt("tunnel name", "nexus"));
    out(format!("creating Cloudflare tunnel {tunnel_name:?}…"));
    let result = provision_named_tunnel(&SetupOptions {
        account_id,
        zone_id,
        hostname,
        tunnel_name,
        port,
    })
    .await?;
    config::save_named_tunnel(&config::NamedTunnelConfig {
        tunnel_id: result.tunnel_id.clone(),
        hostname: result.hostname.clone(),
        credentials_path: result.credentials_path.clone(),
        config_path: result.config_path.clone(),
    })?;
    Ok(result)
}

fn choose_index(label: &str, count: usize) -> Result<usize> {
    let value = prompt(label, "1");
    let index = value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("{label} must be a number"))?;
    if !(1..=count).contains(&index) {
        bail!("{label} must be between 1 and {count}");
    }
    Ok(index)
}

fn prompt(label: &str, default: &str) -> String {
    print!("{label} [{default}]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return default.to_string();
    }
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}

// --- sync commands (Phase 3 merge engine) ---

/// Temporary workspace for bundle extraction and SSH replies. Cleanup runs
/// on every return path, including malformed bundles and failed SSH calls.
struct SyncTempDir {
    path: PathBuf,
}

impl SyncTempDir {
    fn new(prefix: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating temporary sync directory {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SyncTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `nexus sync` dispatch: the export/import subcommands, or the positional
/// target — a shared dir (mailbox sync) or `ssh://user@host`.
fn sync_cmd(cmd: Option<SyncCmd>, target: Option<&Path>) -> Result<()> {
    match cmd {
        Some(SyncCmd::Export { peer, dir }) => sync_export(peer.as_deref(), dir.as_deref()),
        Some(SyncCmd::Import { path, bundle }) => sync_import(path.as_deref(), bundle),
        None => match target {
            Some(t) => {
                let spec = t.to_string_lossy();
                if let Some(host) = spec.strip_prefix("ssh://") {
                    sync_ssh(host)
                } else if spec.contains('@') && !spec.contains('/') && !spec.contains('\\') {
                    sync_ssh(&spec)
                } else {
                    sync_dir(t)
                }
            }
            None => bail!(
                "pass a sync directory, `ssh://user@host`, or a subcommand — `nexus sync --help`"
            ),
        },
    }
}

/// `nexus sync export`: the changeset JSON on stdout; with `--dir`, the
/// blobs go into `DIR/blobs/` too. The cursor advance waits for the peer's
/// ack (see the roadmap's ack design).
fn sync_export(peer: Option<&str>, dir: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let cs = nexus_core::sync::build_changeset(&db, peer, &nexus_core::sync::device_name())?;
    if let Some(dir) = dir {
        let n = nexus_core::sync::export_blobs(&db, &space, &cs, dir)?;
        eprintln!("wrote {n} blob(s) to {}", dir.join("blobs").display());
    }
    eprintln!(
        "exported {} rows, {} tombstones, {} files{}",
        cs.rows.len(),
        cs.tombstones.len(),
        cs.files.len(),
        match peer {
            Some(p) => format!(" for peer {p}"),
            None => " (full export — first sync?)".to_string(),
        }
    );
    out(serde_json::to_string(&cs)?);
    Ok(())
}

/// `nexus sync import`: apply a changeset (or zip bundle) from a file or
/// stdin, then print the reply — this device's export + ack — on stdout.
/// The ssh transport pipes a bundle in and expects a bundle out
/// (`--bundle`); plain JSON is the default for interactive use.
fn sync_import(path: Option<&Path>, bundle_reply: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let bytes = if let Some(p) = path {
        std::fs::read(p).with_context(|| format!("reading {}", p.display()))?
    } else {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        buf
    };
    let tmp = SyncTempDir::new("nexus-import")?;
    let (cs, blob_source) = if bytes.starts_with(b"PK") {
        let bundle = tmp.path().join("in.bundle");
        std::fs::write(&bundle, &bytes)?;
        let unpacked = tmp.path().join("unpacked");
        let cs = nexus_core::sync::unpack_bundle(&bundle, &unpacked)?;
        (cs, Some(unpacked))
    } else {
        (
            serde_json::from_slice(&bytes).context("parsing changeset JSON")?,
            None,
        )
    };
    let (summary, cursors) =
        nexus_core::sync::apply_changeset(&db, &space, &cs, blob_source.as_deref())?;
    print_sync_summary(&cs, &summary);
    let mut reply = nexus_core::sync::build_changeset(
        &db,
        Some(&cs.device_id),
        &nexus_core::sync::device_name(),
    )?;
    reply.ack = Some(cursors);
    if bundle_reply {
        // Zip writers need a seekable sink — write the reply bundle to a
        // temp file, then stream it out.
        let reply_path = tmp.path().join("reply.bundle");
        nexus_core::sync::write_bundle(&db, &space, &reply, std::fs::File::create(&reply_path)?)?;
        std::io::copy(
            &mut std::fs::File::open(&reply_path)?,
            &mut std::io::stdout(),
        )?;
    } else {
        out(serde_json::to_string(&reply)?);
    }
    Ok(())
}

/// The import's log line — stderr, because stdout carries the reply.
fn print_sync_summary(cs: &nexus_core::sync::Changeset, summary: &nexus_core::sync::ApplySummary) {
    eprintln!(
        "imported from {} ({}) — {} rows applied, {} skipped, {} tombstones, {} acks, {} files kept, {} pulled, {} missing",
        short_id(&cs.device_id),
        cs.device_name,
        summary.rows_applied,
        summary.rows_skipped,
        summary.tombstones_applied,
        summary.acks_applied,
        summary.files_kept,
        summary.files_pulled,
        summary.files_missing.len(),
    );
    for w in &summary.warnings {
        eprintln!("  warning: {w}");
    }
}

/// Write a mailbox file through a temporary sibling and rename it into place.
/// Readers either see the previous complete changeset or the new complete
/// one, never a half-written JSON document.
fn write_sync_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("changeset.json");
    let temporary = path.with_file_name(format!(".{name}.nexus-write-{}", uuid::Uuid::new_v4()));
    if let Err(error) = std::fs::write(&temporary, bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("writing {}", temporary.display()));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("installing {}", path.display()));
    }
    Ok(())
}

/// Bidirectional mailbox sync over a shared directory: consume every
/// foreign `*.changeset.json` (rows, tombstones, embedded acks, blobs),
/// then leave this device's own changeset + blobs + ack behind for the
/// peer to pick up. Works on a shared/USB/scp'd dir.
fn sync_dir(dir: &Path) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let my_id = db.device_id()?;
    let own_path = dir.join(format!("{my_id}.changeset.json"));
    // Our previous reply is still an outbound message until a peer imports
    // it. Preserve its ack when this device is run again before that peer;
    // dropping the file would silently lose the only copy of the ack.
    let pending_ack = if own_path.is_file() {
        let bytes = std::fs::read(&own_path)
            .with_context(|| format!("reading pending {}", own_path.display()))?;
        let pending: nexus_core::sync::Changeset = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing pending {}", own_path.display()))?;
        if pending.device_id != my_id {
            bail!(
                "mailbox file {} belongs to another device",
                own_path.display()
            );
        }
        pending.ack
    } else {
        None
    };
    let mut reply_cursors = pending_ack.unwrap_or_default();
    let mut senders: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.ends_with(".changeset.json") {
            continue;
        }
        let cs: nexus_core::sync::Changeset = serde_json::from_slice(&std::fs::read(entry.path())?)
            .with_context(|| format!("parsing {}", entry.path().display()))?;
        if cs.device_id == my_id {
            // Keep our outbound file until a peer consumes it. It is read
            // above only to preserve its ack; it is replaced atomically below.
            continue;
        }
        let (summary, cursors) = nexus_core::sync::apply_changeset(&db, &space, &cs, Some(dir))?;
        print_sync_summary(&cs, &summary);
        reply_cursors.extend(cursors);
        if !senders.contains(&cs.device_id) {
            senders.push(cs.device_id);
        }
        // Consume the changeset. Blob files are retained as a bounded
        // per-name mailbox cache: deleting them here races with another
        // device's outbound export when peers share one directory, while
        // hash verification makes stale payloads harmless.
        std::fs::remove_file(entry.path())?;
    }
    // Our reply: an export for whoever we just imported from (a full
    // export on a cold dir) plus the ack of what we took in. The peer is
    // the mail's sender, not this run's reply cursors — on an ack-only
    // round the cursors are empty but the push cursors were just advanced.
    // With no mail at all (a solo re-run), fall back to the peer we most
    // recently synced with so we don't blast a full export.
    // A single changeset can carry an ack for many senders, but its row
    // export can only be delta-filtered for one peer. When several peers
    // posted mail in the same pass, use a full export so none of them gets
    // its push cursor acknowledged without receiving this device's rows.
    let peer: Option<String> = match senders.as_slice() {
        [peer] if reply_cursors.is_empty() => Some(peer.clone()),
        [] if reply_cursors.is_empty() => db
            .load_sync_state()?
            .iter()
            .filter(|s| s.push_cursor.is_some())
            .max_by(|a, b| a.last_synced_at.cmp(&b.last_synced_at))
            .map(|s| s.peer_id.clone()),
        // A pending ack means another outbound exchange is still in flight;
        // use a full export so replacing the mailbox file cannot drop rows
        // associated with that pending message.
        _ => None,
    };
    let mut cs =
        nexus_core::sync::build_changeset(&db, peer.as_deref(), &nexus_core::sync::device_name())?;
    cs.ack = Some(reply_cursors);
    let path = dir.join(format!("{my_id}.changeset.json"));
    let encoded = serde_json::to_vec(&cs)?;
    let blobs = nexus_core::sync::export_blobs(&db, &space, &cs, dir)?;
    write_sync_file(&path, &encoded)?;
    eprintln!(
        "exported {} rows, {} tombstones, {blobs} blob(s) → {}",
        cs.rows.len(),
        cs.tombstones.len(),
        path.display()
    );
    Ok(())
}

/// Sync over ssh: bundle this device's export (+ blobs) into a zip, pipe
/// it into `ssh host "nexus sync import --bundle"`, and apply the reply
/// bundle (the peer's export + blobs + ack). The peer's device id is
/// remembered in the local `sync_ssh_peer` setting — per-peer cursors
/// live under it.
fn sync_ssh(host: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let remote_peer: Option<String> = db
        .load_settings()?
        .into_iter()
        .find(|(k, _)| k == "sync_ssh_peer")
        .map(|(_, v)| v);
    let mut cs = nexus_core::sync::build_changeset(
        &db,
        remote_peer.as_deref(),
        &nexus_core::sync::device_name(),
    )?;
    // Carry our pull cursors for the remote — without the ack it would
    // re-export everything on every round.
    if let Some(peer) = &remote_peer {
        cs.ack = Some(nexus_core::sync::build_ack(&db, peer)?);
    }
    let tmp = SyncTempDir::new("nexus-ssh")?;
    let bundle = tmp.path().join("out.bundle");
    let n = nexus_core::sync::write_bundle(&db, &space, &cs, std::fs::File::create(&bundle)?)?;
    eprintln!(
        "exported {} rows, {} tombstones, {n} blob(s)",
        cs.rows.len(),
        cs.tombstones.len()
    );
    let output = std::process::Command::new("ssh")
        .arg(host)
        .args(["nexus", "sync", "import", "--bundle"])
        .stdin(std::process::Stdio::from(std::fs::File::open(&bundle)?))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running ssh {host}"))?;
    if !output.status.success() {
        bail!(
            "ssh {host} failed ({}) — is `nexus` installed there?",
            output.status
        );
    }
    let reply_path = tmp.path().join("reply.bundle");
    std::fs::write(&reply_path, &output.stdout)?;
    let unpacked = tmp.path().join("unpacked");
    let reply = nexus_core::sync::unpack_bundle(&reply_path, &unpacked)?;
    let (summary, _) = nexus_core::sync::apply_changeset(&db, &space, &reply, Some(&unpacked))?;
    print_sync_summary(&reply, &summary);
    db.set_setting("sync_ssh_peer", &reply.device_id)?;
    Ok(())
}

// --- status-ish commands ---

fn status() -> Result<()> {
    let creds = config::load_creds_offline();
    let dirs = config::project_dirs()?;
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let spaces = db.list_spaces()?;
    let sessions: u64 = spaces
        .iter()
        .map(|s| db.count_sessions(&s.id).unwrap_or(0))
        .sum();
    out(format!("nexus {}", env!("CARGO_PKG_VERSION")));
    out(format!("data dir:  {}", dirs.data_dir().display()));
    out(format!("config:    {}", config::config_path()?.display()));
    out(format!("db:        {}", space.db_path().display()));
    out(format!("device id: {}", db.device_id()?));
    let mark = |on: bool| if on { "✓" } else { "✗" };
    out(format!(
        "providers: openrouter {}  openai {}  opencode {}  codex {}",
        mark(creds.openrouter_key.is_some()),
        mark(creds.openai_key.is_some()),
        mark(creds.opencode_key.is_some()),
        mark(creds.codex.is_some()),
    ));
    out(format!("spaces: {}   sessions: {sessions}", spaces.len()));
    Ok(())
}

// Long by design: one checklist fn per check line.
#[allow(clippy::too_many_lines)]
async fn doctor(network: bool) -> Result<()> {
    let dirs = config::project_dirs()?;
    let mut problems = 0usize;
    let mut check = |label: &str, ok: bool, detail: &str| {
        out(format!("{} {label}: {detail}", if ok { "✓" } else { "✗" }));
        if !ok {
            problems += 1;
        }
    };

    // Data dir + db.
    let data_dir = dirs.data_dir();
    let db_path = data_dir.join("nexus.db");
    check(
        "data dir",
        data_dir.is_dir(),
        &data_dir.display().to_string(),
    );
    check("db file", db_path.is_file(), &db_path.display().to_string());
    let integrity = db::Db::open(&db_path)
        .ok()
        .and_then(|db| db.integrity_check().ok());
    check(
        "db integrity",
        integrity.as_deref() == Some("ok"),
        integrity.as_deref().unwrap_or("unreadable"),
    );

    // Device-local cache db: disposable, so absent or empty is healthy — it
    // is recreated on the next launch and re-indexes on demand.
    let cache_path = data_dir.join("cache.db");
    match std::fs::metadata(&cache_path) {
        Err(_) => check("cache db", true, "absent (created on next launch)"),
        Ok(meta) if meta.len() == 0 => check("cache db", true, "empty (rebuilds on demand)"),
        Ok(_) => {
            let ok = db::open_attached(&db_path)
                .ok()
                .and_then(|conn| {
                    conn.query_row("PRAGMA cache.integrity_check", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .ok()
                })
                .as_deref()
                == Some("ok");
            check(
                "cache db",
                ok,
                if ok {
                    "integrity ok"
                } else {
                    "unreadable (deleting it is safe — rebuilds on demand)"
                },
            );
        }
    }

    // Config parses.
    let cfg_path = config::config_path()?;
    let cfg_ok = match std::fs::read_to_string(&cfg_path) {
        Ok(text) => toml::from_str::<toml::Value>(&text).is_ok(),
        Err(_) => false,
    };
    check(
        "config",
        cfg_ok,
        if cfg_path.exists() {
            "parse ok"
        } else {
            "missing (scaffolded on first launch)"
        },
    );

    // Optional external tools.
    for tool in [
        "ffmpeg",
        "tesseract",
        "ollama",
        "node",
        "npm",
        "cloudflared",
    ] {
        let present = std::process::Command::new(tool)
            .arg("--version")
            .output()
            .is_ok();
        check(
            tool,
            present,
            if present {
                "found"
            } else {
                "not in PATH (optional)"
            },
        );
    }

    // Live provider check.
    if network {
        let creds = config::load_creds_offline();
        let providers = [
            ("openrouter", creds.openrouter_key.as_deref()),
            ("openai", creds.openai_key.as_deref()),
            ("opencode", creds.opencode_key.as_deref()),
        ];
        for (name, key) in providers {
            if let Some(key) = key {
                let client = OpenRouter::from_key_auto(key.to_string());
                let ok = client.list_models().await.is_ok();
                check(name, ok, if ok { "reachable" } else { "request failed" });
            } else {
                check(name, true, "no key configured");
            }
        }
    }

    if problems > 0 {
        bail!("{problems} problem(s) found");
    }
    out("all good");
    Ok(())
}

async fn update() -> Result<()> {
    match nexus_core::update::latest_version().await {
        Some(latest) if nexus_core::update::version_gt(&latest, nexus_core::update::CURRENT) => {
            out(format!(
                "updating v{} → v{latest} — `cargo install nexus-chat` (this can take a few minutes)",
                nexus_core::update::CURRENT
            ));
            let status = nexus_core::update::install_now()?;
            if !status.success() {
                bail!("cargo install failed ({status}) — see the output above");
            }
            out(format!("updated to v{latest} — relaunch nexus"));
        }
        _ => out(format!("up to date — v{}", nexus_core::update::CURRENT)),
    }
    Ok(())
}

/// Jump into a session: write a handoff file the TUI reads at boot, then
/// launch it.
fn open(reference: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (_, session) = resolve_session(&db, reference)?;
    std::fs::write(space.root.join("pending-open"), &session.id)?;
    let exe = std::env::current_exe().context("resolving this binary")?;
    std::process::Command::new(exe)
        .spawn()
        .context("launching the TUI")?;
    out(format!("opening \"{}\" in the TUI…", session.title));
    Ok(())
}

// --- helpers ---

/// Find a session by full id, kebab slug, or uuid prefix, across all
/// spaces. Returns the owning space id alongside — `Session` rows don't
/// carry it, and citations are keyed by space.
pub(crate) fn resolve_session(db: &db::Db, reference: &str) -> Result<(String, db::Session)> {
    for sp in db.list_spaces()? {
        for s in db.list_sessions(&sp.id)? {
            if s.id == reference
                || s.slug.as_deref() == Some(reference)
                || s.id.starts_with(reference)
            {
                return Ok((sp.id.clone(), s));
            }
        }
    }
    bail!("no session matching {reference:?} — `nexus sessions` lists them")
}

/// Resolve a `--space` name to the name of an existing space (defaulting to
/// the default space when no name is given).
fn resolve_space_name(db: &db::Db, space_name: Option<&str>) -> Result<String> {
    if let Some(name) = space_name {
        db.list_spaces()?
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.name)
            .ok_or_else(|| anyhow!("no space named {name:?} — `nexus spaces` lists them"))
    } else {
        let id = db.default_space_id()?;
        db.list_spaces()?
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.name)
            .context("default space missing")
    }
}

/// Write a line to stdout; exit quietly when the reader closed the pipe
/// (`nexus usage | head`) — coreutils behavior, not Rust's default panic
/// on EPIPE.
fn out(line: impl std::fmt::Display) {
    let mut stdout = std::io::stdout();
    if writeln!(stdout, "{line}").is_err() {
        std::process::exit(0);
    }
}

/// Tokens in human scale: `122.2k`, `5.9M`.
fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Thousands-separated request count — std format! has no grouping spec.
fn fmt_req(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// First 8 chars of a session uuid — enough to disambiguate in listings,
/// and accepted by `nexus export` (prefix match).
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Truncate to `max` chars (not bytes — titles can carry multi-byte
/// glyphs), with an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

/// `2026-08-12T23:15:00Z` → `2026-08-12T23:15` — ASCII-safe char take.
fn fmt_ts(rfc3339: &str) -> String {
    rfc3339.chars().take(16).collect()
}

/// File size in human scale.
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / KB;
    if kb < KB {
        return format!("{kb:.1} KiB");
    }
    let mb = kb / KB;
    if mb < KB {
        return format!("{mb:.1} MiB");
    }
    format!("{:.1} GiB", mb / KB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_by_id_slug_and_prefix() {
        let db = db::Db::open_in_memory().unwrap();
        let space_id = db.default_space_id().unwrap();
        let s = db
            .create_session("my chat", "gpt-x", &space_id, "chat")
            .unwrap();
        db.set_session_title(&s.id, "my chat", Some("my-chat"))
            .unwrap();

        let (sid, found) = resolve_session(&db, &s.id).unwrap();
        assert_eq!(sid, space_id);
        assert_eq!(found.id, s.id);

        let (_, found) = resolve_session(&db, "my-chat").unwrap();
        assert_eq!(found.id, s.id);

        let prefix = &s.id[..8];
        let (_, found) = resolve_session(&db, prefix).unwrap();
        assert_eq!(found.id, s.id);
    }

    #[test]
    fn resolve_session_unknown_bails() {
        let db = db::Db::open_in_memory().unwrap();
        assert!(resolve_session(&db, "nope").is_err());
    }

    #[test]
    fn fmt_tokens_scales() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(122_221), "122.2k");
        assert_eq!(fmt_tokens(5_900_000), "5.9M");
    }

    #[test]
    fn fmt_req_groups_thousands() {
        assert_eq!(fmt_req(0), "0");
        assert_eq!(fmt_req(999), "999");
        assert_eq!(fmt_req(1_000), "1,000");
        assert_eq!(fmt_req(28_172), "28,172");
        assert_eq!(fmt_req(1_000_000), "1,000,000");
    }

    #[test]
    fn truncate_shortens_with_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long title indeed", 10), "a very lo…");
    }

    #[test]
    fn short_id_takes_eight_chars() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
    }

    #[test]
    fn fmt_ts_trims_seconds() {
        assert_eq!(fmt_ts("2026-08-12T23:15:00Z"), "2026-08-12T23:15");
    }

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }

    fn session(id: &str, created: &str) -> db::Session {
        db::Session {
            id: id.to_string(),
            title: id.to_string(),
            model: "m".to_string(),
            slug: None,
            created_at: created.to_string(),
            compact_summary: None,
            compact_through: 0,
            web_mode: false,
            swarm_mode: false,
            kind: "chat".to_string(),
            research_parent_id: None,
        }
    }

    #[test]
    fn prune_keeps_newest_per_list() {
        let sessions = vec![
            session("a", "2026-08-10T00:00:00Z"),
            session("b", "2026-08-09T00:00:00Z"),
            session("c", "2026-08-08T00:00:00Z"),
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let doomed = prune_targets(&sessions, Some(1), None, now);
        assert_eq!(doomed.len(), 2);
        assert_eq!(doomed[0].id, "b");
        assert_eq!(doomed[1].id, "c");
    }

    #[test]
    fn prune_drops_older_than_days() {
        let sessions = vec![
            session("a", "2026-08-11T00:00:00Z"),
            session("b", "2026-08-01T00:00:00Z"),
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let doomed = prune_targets(&sessions, None, Some(7), now);
        assert_eq!(doomed.len(), 1);
        assert_eq!(doomed[0].id, "b");
    }

    #[test]
    fn prune_never_touches_when_nothing_matches() {
        let sessions = vec![session("a", "2026-08-11T00:00:00Z")];
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(prune_targets(&sessions, Some(5), None, now).is_empty());
        assert!(prune_targets(&sessions, None, Some(7), now).is_empty());
    }

    #[test]
    fn transcript_includes_all_roles() {
        let messages = vec![
            nexus_core::db::Message {
                role: "user".to_string(),
                content: "hello".to_string(),
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            },
            nexus_core::db::Message {
                role: "assistant".to_string(),
                content: "hi there".to_string(),
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            },
            nexus_core::db::Message {
                role: "tool_call".to_string(),
                content: r#"{"name":"search"}"#.to_string(),
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            },
        ];
        let out = transcript_markdown(&session("s", ""), &messages);
        assert!(out.starts_with("# s"));
        assert!(out.contains("## user\n\nhello"));
        assert!(out.contains("## assistant\n\nhi there"));
        assert!(out.contains("## tool_call\n\n```json\n{\"name\":\"search\"}\n```"));
    }

    #[test]
    fn zip_round_trip_preserves_tree() {
        let root = std::env::temp_dir().join(format!("nexus-zip-{}", uuid::Uuid::new_v4()));
        let nested = root.join("spaces").join("default");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("memory.md"), "hello").unwrap();
        std::fs::write(root.join("nexus.db"), b"\x00\x01").unwrap();
        // The device-local cache db must never ride along in a backup.
        std::fs::write(root.join("cache.db"), b"\x02\x03").unwrap();

        let zip_path = root.with_extension("zip");
        zip_dir(&root, &zip_path).unwrap();

        let dest = std::env::temp_dir().join(format!("nexus-unzip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dest).unwrap();
        unzip_into(&zip_path, &dest).unwrap();
        assert_eq!(
            std::fs::read(dest.join("nexus.db")).unwrap(),
            b"\x00\x01".to_vec()
        );
        assert!(
            !dest.join("cache.db").exists(),
            "cache.db must be excluded from backups"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("spaces/default/memory.md")).unwrap(),
            "hello"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&zip_path);
    }

    #[test]
    fn unzip_rejects_escaping_paths() {
        let root = std::env::temp_dir().join(format!("nexus-zipbad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // Hand-build an archive with a traversal entry via the zip API.
        let zip_path = root.join("bad.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("../escape.txt", opts).unwrap();
        zip.write_all(b"nope").unwrap();
        zip.finish().unwrap();

        let dest = std::env::temp_dir().join(format!("nexus-unzipbad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dest).unwrap();
        assert!(unzip_into(&zip_path, &dest).is_err());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
