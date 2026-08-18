//! The command seam: one enum for every user intent, parsed from the
//! `/`-command line (or synthesized by the TUI keys, the CLI, or the Phase 4
//! host). `run_command` stays the string front; everything that mutates the
//! app goes through `App::execute`. The slash-command catalog (`COMMANDS`,
//! `Command`, `Match`, `fuzzy_score`) also lives here — it's the pure,
//! dependency-free half of the old `input.rs`, which 2e split into this
//! catalog plus the TUI's composer ops (`crates/tui/src/composer.rs`).

use anyhow::Result;

use super::{App, FilesTab};

/// A slash command: canonical `name`, a short (≤20 char) `desc`, and alias
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
        name: "new",
        desc: "start new chat",
        aliases: &["chat", "clear"],
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
        name: "web",
        desc: "toggle web answer mode (search-first, cited)",
        aliases: &["websearch"],
    },
    Command {
        name: "incognito",
        desc: "toggle incognito (no persistence, no apps)",
        aliases: &["private", "anon"],
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
/// the Phase 4 host; `App::execute` is the only mutation path.
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
    /// `/swarm` — the swarm roster popup.
    OpenSwarm,
    /// `/config` — the nerd-config popup.
    OpenSettings,
    /// `/theme [opaque|transparent]` — configure the TUI background.
    SetTheme { mode: String },
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
    /// `/incognito` — toggle (`on` is absolute for the host).
    Incognito { on: bool },
    /// `/watch [topic]` — open the watch picker, or create a watch.
    Watch { topic: Option<String> },
    /// `/usage` — the analytics popup.
    OpenUsage,
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
            "swarm" => Ok(AppCommand::OpenSwarm),
            "config" => Ok(AppCommand::OpenSettings),
            "theme" => Ok(AppCommand::SetTheme {
                mode: rest(cmd, token),
            }),
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
            "incognito" => Ok(AppCommand::Incognito {
                on: !self.incognito,
            }),
            "watch" => {
                let arg = rest(cmd, token);
                Ok(AppCommand::Watch {
                    topic: (!arg.is_empty()).then_some(arg),
                })
            }
            "usage" => Ok(AppCommand::OpenUsage),
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
            | AppCommand::Watch { .. } => {}
            AppCommand::Send { text } => self.send_message(text)?,
            AppCommand::Cancel { task } => match task {
                Some(id) => self.cancel_chat_task(id)?,
                None => self.stop_stream()?,
            },
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
            AppCommand::Incognito { on } => {
                if on != self.incognito {
                    self.toggle_incognito()?;
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
