//! The command seam: one enum for every user intent, parsed from the
//! `/`-command line (or synthesized by the TUI keys, the CLI, or the host).
//! `run_command` is the string front; everything that mutates the app goes
//! through `App::execute`. The slash-command catalog (`COMMANDS`, `Command`,
//! `Match`, `fuzzy_score`) lives here too.

use anyhow::Result;

use super::{App, FilesTab, WorkMode};

/// A slash command: canonical `name`, a short `desc`, and alias
/// keywords. Names, aliases, and the description are all fuzzy-searchable, so
/// typing `/history` surfaces `session`.
pub struct Command {
    pub name: &'static str,
    pub desc: &'static str,
    pub aliases: &'static [&'static str],
}

/// One row of the slash-command autocomplete.
pub enum Match {
    Builtin(&'static Command),
}

impl Match {
    pub const fn name(&self) -> &str {
        match self {
            Self::Builtin(c) => c.name,
        }
    }

    pub const fn desc(&self) -> &str {
        match self {
            Self::Builtin(c) => c.desc,
        }
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        desc: "keys & commands",
        aliases: &["keys", "shortcuts", "?"],
    },
    Command {
        name: "new",
        desc: "start new chat",
        aliases: &["clear"],
    },
    Command {
        name: "compact",
        desc: "summarize old messages",
        aliases: &["compaction", "summarize"],
    },
    Command {
        name: "session",
        desc: "switch sessions",
        aliases: &["sessions", "history", "resume", "continue", "switch"],
    },
    Command {
        name: "space",
        desc: "switch spaces",
        aliases: &["spaces", "project", "workspace"],
    },
    Command {
        name: "model",
        desc: "pick a model",
        aliases: &["models", "llm"],
    },
    Command {
        name: "login",
        desc: "pick a backend to log into",
        aliases: &[
            "key",
            "apikey",
            "token",
            "auth",
            "codex",
            "subscription",
            "oauth",
            "chatgpt",
            "opencode",
        ],
    },
    Command {
        name: "local",
        desc: "local inference runtime",
        aliases: &[
            "ollama",
            "mlx",
            "mlx-serve",
            "lmstudio",
            "edge0",
            "offline",
            "runtime",
            "serve",
        ],
    },
    Command {
        name: "swarm",
        desc: "multi-persona roundtable roster",
        aliases: &["swarms", "personas", "panel"],
    },
    Command {
        name: "config",
        desc: "settings & stats",
        aliases: &["settings", "stats", "nerd", "params"],
    },
    Command {
        name: "theme",
        desc: "set UI background",
        aliases: &["appearance", "colors"],
    },
    Command {
        name: "skills",
        desc: "manage skills",
        aliases: &["addskill"],
    },
    Command {
        name: "files",
        desc: "browse space files / images / scripts",
        aliases: &[
            "file", "attach", "upload", "docs", "image", "images", "img", "pictures", "script",
            "scripts",
        ],
    },
    Command {
        name: "apps",
        desc: "view space apps",
        aliases: &["app", "webapps"],
    },
    Command {
        name: "research",
        desc: "deep multi-agent research (blank = scope topic from this chat)",
        aliases: &["deep-research"],
    },
    Command {
        name: "export",
        desc: "write session's report + sources to a file",
        aliases: &["save-report"],
    },
    Command {
        name: "watch",
        desc: "standing research, re-runs every 24h",
        aliases: &["watches"],
    },
    Command {
        name: "usage",
        desc: "token/cache/cost analytics by backend and model",
        aliases: &["analytics", "costs", "billing"],
    },
    Command {
        name: "harness",
        desc: "where the tools fail — last 7 days",
        aliases: &[],
    },
    Command {
        name: "procs",
        desc: "background processes started by the agent",
        aliases: &[],
    },
    Command {
        name: "web",
        desc: "toggle web answer mode (search-first, cited)",
        aliases: &["websearch"],
    },
    Command {
        name: "agent",
        desc: "work in a directory: read, edit, run — /agent [path]",
        aliases: &[],
    },
    Command {
        name: "chat",
        desc: "back to plain chat (no workspace)",
        aliases: &[],
    },
    Command {
        name: "auto",
        desc: "toggle agent / agent·auto approval policy",
        aliases: &[],
    },
    Command {
        name: "incognito",
        desc: "toggle incognito (no persistence, no apps)",
        aliases: &["private", "anon"],
    },
    Command {
        name: "training",
        desc: "record training data for this space (local, scrubbed)",
        aliases: &[],
    },
    Command {
        name: "retry",
        desc: "regenerate the last reply",
        aliases: &["regen"],
    },
    Command {
        name: "undo",
        desc: "restore the files the agent changed in its last turn",
        aliases: &[],
    },
    Command {
        name: "copy",
        desc: "copy last reply",
        aliases: &["yank", "clip"],
    },
    Command {
        name: "quit",
        desc: "exit the app",
        aliases: &["q", "exit"],
    },
];

/// Subsequence fuzzy score, case-insensitive. `None` if `needle` isn't a
/// subsequence of `hay`; higher is a better match (bonuses for contiguous runs
/// and matching at the start).
pub fn fuzzy_score(hay: &str, needle: &str) -> Option<i32> {
    let hay = hay.to_lowercase();
    let needle = needle.to_lowercase();
    let mut chars = hay.chars();
    let mut score = 0i32;
    let mut prev_matched = false;
    let mut pos = 0i32;
    for nc in needle.chars() {
        loop {
            let hc = chars.next()?;
            if hc == nc {
                score += 1;
                if prev_matched {
                    score += 2;
                }
                if pos == 0 {
                    score += 3;
                }
                prev_matched = true;
                pos += 1;
                break;
            }
            prev_matched = false;
            pos += 1;
        }
    }
    Some(score)
}

/// Best fuzzy score of `needle` across a command's name/aliases/desc, with the
/// name weighted highest and the description lowest.
pub fn command_score(c: &Command, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut best: Option<i32> = None;
    let mut upd = |s: &str, bonus: i32| {
        if let Some(sc) = fuzzy_score(s, needle) {
            let v = sc + bonus;
            best = Some(best.map_or(v, |b| b.max(v)));
        }
    };
    upd(c.name, 100);
    for a in c.aliases {
        upd(a, 50);
    }
    upd(c.desc, 0);
    best
}

/// One user intent, in the seam's own words. Parsed from the `/`-command
/// line (`App::parse_command`) or synthesized by the TUI keys, the CLI, or
/// the host; `App::execute` is the only mutation path.
///
/// Serde: `POST /v1/command` ships this enum directly — every payload is
/// plain (strings, bools, optionals, [`FilesTab`](super::FilesTab)), so the
/// seam itself is the wire type; no mirror needed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AppCommand {
    /// `/quit` — exit the app.
    Quit,
    /// Send a chat message (the composer's Enter, host messages).
    Send { text: String },
    /// Cancel an in-flight chat response; `None` = the active one (Esc).
    Cancel { task: Option<u64> },
    /// Answer the current workspace permission request, including wave review.
    Approve { decision: crate::tools::Decision },
    /// `/steer` — queue an extra instruction for the running research job.
    Steer { text: String },
    /// Reply to the parked survey/plan gate.
    AnswerGate { text: String },
    /// `/new` — fresh session.
    NewSession,
    /// `/compact` — compact the session transcript.
    Compact,
    /// `/session` — the session picker.
    OpenSessionPicker,
    /// `/space` — the space picker.
    OpenSpacePicker,
    /// `/model` — the model picker.
    OpenModelPicker,
    /// `/login` — the provider login popup.
    OpenLogin,
    /// `/local [<runtime> [endpoint] | off]` — pick the local inference
    /// runtime. An empty `spec` opens the picker (reports the current
    /// setting headless); `/ollama`, `/mlx`, `/mlx-serve`, `/lmstudio` and `/edge0` fill
    /// it in.
    ConfigureLocal { spec: String },
    /// `/local start|stop|restart|status [<runtime>]` — manage the local
    /// provider's own inference server. An empty `runtime` means the selected
    /// one; `/edge0 start` names it through the alias.
    LocalServer { verb: String, runtime: String },
    /// `/swarm` — the swarm roster popup.
    OpenSwarm,
    /// `/config` — the nerd-config popup.
    OpenSettings,
    /// `/theme [opaque|transparent]` — configure the TUI background.
    SetTheme { mode: String },
    /// `/retry` — regenerate the last reply.
    Retry,
    /// `/undo` — restore the files the last agent turn changed.
    Undo,
    /// `!cmd` in the composer — run it as the user, output captured into the
    /// conversation (terminal UI only).
    RunUserCommand { cmd: String },
    /// An interactive `!` command the terminal UI already ran on the real
    /// terminal — record its command and exit code, `None` when it was killed
    /// or (`started: false`) never started (terminal UI only).
    RecordUserCommand {
        cmd: String,
        exit: Option<i32>,
        started: bool,
    },
    /// `/copy` — the copy menu.
    OpenCopyMenu,
    /// `/skills` — the skills popup.
    OpenSkills,
    /// `/files`, `/image`, `/script` — the file popup on a tab.
    OpenFiles { tab: FilesTab },
    /// `/apps` — the apps popup.
    OpenApps,
    /// `/research [topic]` — `gated: false` skips the plan-approval gate
    /// (`/research!`); an empty topic distills one from recent chat.
    RunResearch { topic: String, gated: bool },
    /// `/export` — print the latest report + sources.
    Export,
    /// `/web` — toggle web-answer mode.
    ToggleWeb,
    /// `/auto` — toggle automatic approval while in an agent mode.
    ToggleAuto,
    /// `/agent [path]` or `/chat` — choose local workspace access.
    SetWorkMode {
        mode: WorkMode,
        root: Option<String>,
    },
    /// `/incognito` — toggle (`on` is absolute for the host).
    Incognito { on: bool },
    /// `/training [on|off]` — set or toggle device-local recording consent.
    SetTraining { on: Option<bool> },
    /// `/watch [topic]` — open the watch picker, or create a watch.
    Watch { topic: Option<String> },
    /// `/usage` — the analytics popup.
    OpenUsage,
    /// `/harness` — tool failures over the last seven days.
    Harness,
    /// `/procs` — background processes started by the agent.
    OpenProcs,
    /// `/help` — the keybinding + command reference.
    OpenHelp,
    /// `/<skill-name> [text]` — arm a skill; `text` sends it immediately.
    ArmSkill { name: String, rest: Option<String> },
    /// Switch the active space (CLI `--space`, host).
    SwitchSpace { name: String },
    /// Resolve and open a session by id/slug/prefix (CLI `open`, host).
    ResolveSession { id: String },
    /// Set the active model (CLI `--model`, host).
    SetModel { id: String },
    /// Set one named setting by key (host).
    SetSetting { key: String, value: String },
}

impl App {
    /// Parse a `/`-command line (without the leading slash) into the command
    /// seam. Resolves aliases via the `COMMANDS` catalog and recognizes
    /// `/<skill-name>` arms. `Err` carries a status-line message for unknown
    /// commands — the TUI shows it without failing the key handler.
    pub fn parse_command(&self, cmd: &str) -> std::result::Result<AppCommand, String> {
        // `/research! <topic>` = research without the plan-approval gate.
        // Handled before command lookup: the `!` makes the token miss COMMANDS.
        if let Some(rest) = cmd.strip_prefix("research!") {
            return Ok(AppCommand::RunResearch {
                topic: rest.trim().to_string(),
                gated: false,
            });
        }
        let token = cmd.split_whitespace().next().unwrap_or("");
        // Resolve aliases (e.g. "history" -> "session") to a canonical name.
        let canonical = COMMANDS
            .iter()
            .find(|c| c.name == token || c.aliases.contains(&token))
            .map_or(token, |c| c.name);
        let rest = |cmd: &str, token: &str| cmd[token.len()..].trim().to_string();
        match canonical {
            "quit" => Ok(AppCommand::Quit),
            "new" => Ok(AppCommand::NewSession),
            "compact" => Ok(AppCommand::Compact),
            "session" => Ok(AppCommand::OpenSessionPicker),
            "space" => Ok(AppCommand::OpenSpacePicker),
            "model" => Ok(AppCommand::OpenModelPicker),
            "login" => Ok(AppCommand::OpenLogin),
            // `/local <spec>`, but also `/ollama` and friends: the runtime
            // aliases resolve to `local`, so the token *is* the spec.
            "local" => Ok(local_command(&match (token, rest(cmd, token)) {
                ("local", arg) => arg,
                (runtime, arg) if arg.is_empty() => runtime.to_string(),
                (runtime, arg) => format!("{runtime} {arg}"),
            })),
            "swarm" => Ok(AppCommand::OpenSwarm),
            "config" => Ok(AppCommand::OpenSettings),
            "theme" => Ok(AppCommand::SetTheme {
                mode: rest(cmd, token),
            }),
            "retry" => Ok(AppCommand::Retry),
            "undo" => Ok(AppCommand::Undo),
            "copy" => Ok(AppCommand::OpenCopyMenu),
            "skills" => Ok(AppCommand::OpenSkills),
            "files" => Ok(AppCommand::OpenFiles {
                tab: match token {
                    t if t == "image" || t == "images" || t == "img" || t == "pictures" => {
                        FilesTab::Images
                    }
                    t if t == "script" || t == "scripts" => FilesTab::Scripts,
                    _ => FilesTab::Files,
                },
            }),
            "apps" => Ok(AppCommand::OpenApps),
            "research" => Ok(AppCommand::RunResearch {
                topic: rest(cmd, token),
                gated: true,
            }),
            "export" => Ok(AppCommand::Export),
            "web" => Ok(AppCommand::ToggleWeb),
            "agent" => {
                let root = rest(cmd, token);
                Ok(AppCommand::SetWorkMode {
                    mode: WorkMode::Agent,
                    root: (!root.is_empty()).then_some(root),
                })
            }
            "auto" => Ok(AppCommand::ToggleAuto),
            "chat" => Ok(AppCommand::SetWorkMode {
                mode: WorkMode::Chat,
                root: None,
            }),
            "incognito" => Ok(AppCommand::Incognito {
                on: !self.incognito,
            }),
            "training" => Ok(AppCommand::SetTraining {
                on: match rest(cmd, token).as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => return Err("usage: /training [on|off]".to_string()),
                },
            }),
            "watch" => {
                let arg = rest(cmd, token);
                Ok(AppCommand::Watch {
                    topic: (!arg.is_empty()).then_some(arg),
                })
            }
            "usage" => Ok(AppCommand::OpenUsage),
            "harness" => Ok(AppCommand::Harness),
            "procs" => Ok(AppCommand::OpenProcs),
            "help" => Ok(AppCommand::OpenHelp),
            other => {
                if self.skills.iter().any(|s| s.name == other) {
                    let text = rest(cmd, token);
                    Ok(AppCommand::ArmSkill {
                        name: other.to_string(),
                        rest: (!text.is_empty()).then_some(text),
                    })
                } else {
                    Err(format!("unknown command: /{other}"))
                }
            }
        }
    }

    /// Run one parsed command — the mutation path for domain intents. The
    /// TUI's `AppView::execute` intercepts the view-only commands (quit,
    /// popup opens, the watch picker) before delegating here; headless
    /// consumers only ever send domain commands. `run_command` is the
    /// `/`-string parse front.
    #[allow(clippy::too_many_lines)] // Long by design: one arm per command.
    pub fn execute(&mut self, cmd: AppCommand) -> Result<()> {
        match cmd {
            // View-only commands — the TUI's `AppView::execute` handles these
            // (they need the popup/should-quit state the view owns). They
            // land here only from headless consumers, which never send them.
            AppCommand::Quit
            | AppCommand::OpenSessionPicker
            | AppCommand::OpenSpacePicker
            | AppCommand::OpenModelPicker
            | AppCommand::OpenLogin
            | AppCommand::OpenSwarm
            | AppCommand::OpenSettings
            | AppCommand::SetTheme { .. }
            | AppCommand::OpenCopyMenu
            | AppCommand::OpenSkills
            | AppCommand::OpenFiles { .. }
            | AppCommand::OpenApps
            | AppCommand::OpenUsage
            | AppCommand::OpenProcs
            | AppCommand::OpenHelp
            | AppCommand::Watch { .. } => {}
            AppCommand::Harness => {
                let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
                self.harness = Some(self.db.harness_summary(&since)?);
            }
            AppCommand::ConfigureLocal { spec } => self.configure_local(&spec),
            AppCommand::LocalServer { verb, runtime } => self.manage_local_server(&verb, &runtime),
            AppCommand::Send { text } => self.send_message(text)?,
            AppCommand::Retry => self.retry_last_reply()?,
            AppCommand::Undo => self.undo_last_turn()?,
            AppCommand::RunUserCommand { cmd } => self.run_user_command(cmd)?,
            AppCommand::RecordUserCommand { cmd, exit, started } => {
                self.record_user_command(&cmd, exit, started)?;
            }
            AppCommand::Cancel { task } => match task {
                Some(id) => self.cancel_chat_task(id)?,
                None => self.stop_stream()?,
            },
            AppCommand::Approve { decision } => self.answer_approval(decision),
            AppCommand::Steer { text } => self.steer_research(&text),
            AppCommand::AnswerGate { text } => self.reply_to_survey_gate(&text),
            AppCommand::NewSession => self.new_session(),
            AppCommand::Compact => self.force_compact(),
            AppCommand::RunResearch { topic, gated } => {
                if !gated {
                    self.start_research_with_gate(&topic, false);
                } else if topic.is_empty() {
                    self.start_research_from_chat();
                } else {
                    self.start_research(&topic);
                }
            }
            AppCommand::Export => {
                self.export_report()?;
            }
            AppCommand::ToggleWeb => self.toggle_web_mode(),
            AppCommand::ToggleAuto => {
                if self.work_mode.is_agent() {
                    let mode = if self.work_mode == WorkMode::AgentAuto {
                        WorkMode::Agent
                    } else {
                        WorkMode::AgentAuto
                    };
                    let root = self
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.root().to_string_lossy().into_owned());
                    self.set_work_mode(mode, root.as_deref())?;
                } else {
                    self.push_status("/auto needs agent mode · run /agent first");
                }
            }
            AppCommand::SetWorkMode { mode, root } => {
                self.set_work_mode(mode, root.as_deref())?;
            }
            AppCommand::Incognito { on } => {
                if on != self.incognito {
                    self.toggle_incognito()?;
                }
            }
            AppCommand::SetTraining { on } => {
                if self.incognito {
                    self.push_status("training is not available in incognito mode");
                } else {
                    let on = on.unwrap_or_else(|| !self.training_enabled());
                    self.db.set_setting(
                        &format!("training:{}", self.active_space.id),
                        if on { "on" } else { "off" },
                    )?;
                    if on {
                        self.push_status(format!(
                            "training on — local data in {}; secrets are scrubbed",
                            self.space.traces_dir().display()
                        ));
                    } else {
                        self.push_status("training off for this space");
                    }
                }
            }
            AppCommand::ArmSkill { name, rest } => {
                self.forced_skill = Some(name.clone());
                if let Some(text) = rest {
                    self.send_message(text)?;
                } else {
                    self.push_status(format!("skill {name} armed for next message"));
                }
            }
            AppCommand::SwitchSpace { name } => {
                self.switch_space_cli(&name)?;
            }
            AppCommand::ResolveSession { id } => {
                self.switch_to_session_by_id(&id)?;
            }
            AppCommand::SetModel { id } => {
                self.pick_model(&id)?;
            }
            AppCommand::SetSetting { key, value } => {
                self.set_setting(&key, &value)?;
            }
        }
        Ok(())
    }

    /// The `/`-string front: parse into the seam, then execute. Unknown
    /// commands surface as a status line rather than an error.
    pub fn run_command(&mut self, cmd: &str) -> Result<()> {
        match self.parse_command(cmd) {
            Ok(cmd) => self.execute(cmd),
            Err(message) => {
                self.push_status(message);
                Ok(())
            }
        }
    }
}

/// Split `/local`'s argument into the two things it can mean: managing a
/// runtime's server, or selecting the runtime itself.
///
/// The verb is looked for in any position because the runtime aliases fold
/// themselves into the spec — `/local start edge0` and `/edge0 start` both
/// arrive here as the same two words in the opposite order.
fn local_command(spec: &str) -> AppCommand {
    let words: Vec<&str> = spec.split_whitespace().collect();
    let verb = words.iter().position(|word| {
        matches!(
            word.to_ascii_lowercase().as_str(),
            "start" | "serve" | "stop" | "restart" | "status"
        )
    });
    match verb {
        Some(index) => AppCommand::LocalServer {
            verb: match words[index].to_ascii_lowercase().as_str() {
                "serve" => "start".to_string(),
                other => other.to_string(),
            },
            runtime: words
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, word)| *word)
                .collect::<Vec<_>>()
                .join(" "),
        },
        None => AppCommand::ConfigureLocal {
            spec: spec.to_string(),
        },
    }
}

#[cfg(test)]
mod harness_tests {
    use super::*;

    #[test]
    fn harness_command_parses_and_loads_seven_days_including_incognito() {
        let mut app = crate::app::tests::app_with_key();
        let now = chrono::Utc::now();
        for days in [0, 6, 8] {
            app.db
                .conn_for_test()
                .execute(
                    "INSERT INTO cache.tool_events
                     (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
                     VALUES (?1, 'session', 'scripts', 'edit', 0, 'stale_edit', 1, 1)",
                    [(now - chrono::Duration::days(days)).to_rfc3339()],
                )
                .unwrap();
        }
        assert_eq!(app.parse_command("harness"), Ok(AppCommand::Harness));
        for incognito in [false, true] {
            app.incognito = incognito;
            app.run_command("harness").unwrap();
            let summary = app.harness.as_ref().unwrap();
            assert_eq!((summary.calls, summary.failures), (2, 2));
            assert_eq!(summary.rows.len(), 1);
            assert_eq!(summary.rows[0].count, 2);
        }
        let root = app.space.root.clone();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
