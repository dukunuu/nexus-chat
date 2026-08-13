//! The command seam: one enum for every user intent, parsed from the
//! `/`-command line (or synthesized by the TUI keys, the CLI, or the Phase 4
//! host). `run_command` stays the string front; everything that mutates the
//! app goes through `App::execute`.

use anyhow::Result;

use super::{App, FilesTab};

/// One user intent, in the seam's own words. Parsed from the `/`-command
/// line (`App::parse_command`) or synthesized by the TUI keys, the CLI, or
/// the Phase 4 host; `App::execute` is the only mutation path.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let canonical = crate::input::COMMANDS
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

    /// Run one parsed command — the single mutation path for user intents.
    /// The TUI/CLI/host all funnel through this; `run_command` is the
    /// `/`-string parse front.
    pub fn execute(&mut self, cmd: AppCommand) -> Result<()> {
        match cmd {
            AppCommand::Quit => self.should_quit = true,
            AppCommand::Send { text } => self.send_message(text)?,
            AppCommand::Cancel { task } => match task {
                Some(id) => self.cancel_chat_task(id)?,
                None => self.stop_stream()?,
            },
            AppCommand::Steer { text } => self.steer_research(&text),
            AppCommand::AnswerGate { text } => self.reply_to_survey_gate(&text),
            AppCommand::NewSession => self.new_session(),
            AppCommand::Compact => self.force_compact(),
            AppCommand::OpenSessionPicker => {
                self.open_session_picker()?;
            }
            AppCommand::OpenSpacePicker => {
                self.open_space_picker()?;
            }
            AppCommand::OpenModelPicker => self.open_model_picker(),
            AppCommand::OpenLogin => self.open_login_popup(),
            AppCommand::OpenSwarm => self.open_swarm_popup(),
            AppCommand::OpenSettings => self.open_settings(),
            AppCommand::OpenCopyMenu => self.open_copy_menu(),
            AppCommand::OpenSkills => self.open_skills_popup(),
            AppCommand::OpenFiles { tab } => {
                if self.incognito {
                    self.push_status("not available in incognito mode");
                } else {
                    self.open_files_popup(tab);
                }
            }
            AppCommand::OpenApps => {
                if self.incognito {
                    self.push_status("apps not available in incognito mode");
                } else {
                    self.open_apps_popup();
                }
            }
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
            AppCommand::Watch { topic } => {
                if !self.is_research_session() {
                    self.push_status(
                        "watch is only available in research sessions — use /research first",
                    );
                } else if let Some(t) = topic {
                    self.create_watch(&t);
                } else {
                    self.open_watch_picker()?;
                }
            }
            AppCommand::OpenUsage => self.open_usage_popup(),
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
