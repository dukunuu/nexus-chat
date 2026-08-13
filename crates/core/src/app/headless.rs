//! Headless one-shot runs (`nexus ask`, `nexus chat`, `nexus research`,
//! `nexus watch run`): the same streaming pipelines the TUI event loop
//! drives, minus the terminal. Answers stream to stdout as they arrive,
//! tool status goes to stderr, and every conversation is persisted as a
//! normal session in the active space — reopen it in the TUI and it's just
//! another chat.

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::db::{Watch, stage_content};
use crate::provider::{StreamEvent, Usage};

use super::{App, AppCommand, AppEvent};

/// Per-turn behavior switches for the headless drivers.
#[derive(Default, Clone, Copy)]
pub struct TurnOpts {
    /// Stream answer tokens to stdout as they arrive (default true).
    pub stream: bool,
    /// Suppress stderr chatter (tool status, thinking, token summary).
    pub quiet: bool,
}

/// What a finished `nexus ask` leaves behind — the answer already went to
/// stdout unless `--json` asked for it structured.
pub struct AskOutcome {
    pub answer: String,
    pub session_id: String,
    pub session_title: String,
    pub usage: Option<Usage>,
}

/// A finished headless research run (`nexus research` / a watch's run).
pub struct ResearchOutcome {
    pub report: String,
    pub session_id: String,
    pub session_title: String,
}

/// Stream one answer chunk to stdout; exit quietly if the reader closed the
/// pipe (`nexus ask | head -c 50`) — coreutils behavior, not a panic.
fn stream(s: &str) {
    let mut stdout = std::io::stdout();
    if stdout.write_all(s.as_bytes()).is_err() {
        std::process::exit(0);
    }
}

/// Print a line to stderr unless quiet mode is on.
fn note(quiet: bool, line: impl std::fmt::Display) {
    if !quiet {
        eprintln!("{line}");
    }
}

impl App {
    /// Switch the active space by name (`--space`). Bails on an unknown
    /// name; no-op when it's already the active space.
    pub fn switch_space_cli(&mut self, name: &str) -> Result<()> {
        let row = self
            .db
            .list_spaces()
            .context("listing spaces")?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("no space named {name:?} — `nexus spaces` lists them"))?;
        if row.id != self.active_space.id {
            self.set_active_space(row);
        }
        Ok(())
    }

    /// Drive one non-interactive turn: send `prompt`, drain stream events
    /// until the response finishes, and (unless `opts.stream`) collect the
    /// answer. Tool status lines and the token summary go to stderr unless
    /// `opts.quiet`. Returns the final answer text and merged usage.
    pub async fn run_turn(
        &mut self,
        prompt: String,
        opts: TurnOpts,
    ) -> Result<(String, Option<Usage>)> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        // Drive through the seam, like the host API will: boot → command →
        // event drain. The guard checks above already validated the preconditions.
        self.execute(AppCommand::Send { text: prompt })?;

        let mut usage: Option<Usage> = None;
        let mut last_status = String::new();
        let mut streamed_any = false;
        let mut thought = false;
        loop {
            match self.next_event().await {
                AppEvent::Stream(Some((task_id, ev))) => {
                    match &ev {
                        StreamEvent::Token(t) => {
                            streamed_any = true;
                            if opts.stream {
                                stream(t);
                            }
                        }
                        StreamEvent::Reasoning(_) if !thought => {
                            thought = true;
                            note(opts.quiet, "…thinking…");
                        }
                        StreamEvent::Status(s) if *s != last_status => {
                            note(opts.quiet, s);
                            last_status.clone_from(s);
                        }
                        _ => {}
                    }
                    self.on_chat_event(task_id, ev)?;
                    if self.chat_tasks.is_empty() {
                        break;
                    }
                    if let Some(u) = self.chat_tasks.get(&task_id).and_then(|t| t.usage) {
                        usage = Some(u);
                    }
                }
                // The event channel closed without a Done — nothing more is
                // coming; fall through to the summary below.
                AppEvent::Stream(None) => break,
                AppEvent::Title(t) => self.on_title_result(t),
                _ => {} // models/memory/compact/etc: nothing pending for a fresh turn
            }
        }
        if streamed_any && opts.stream {
            stream("\n");
        }

        // The reply is already persisted (the turn's session is active, so
        // finish_chat_task pushed it to self.messages too). Prefer the
        // assistant row; surface an error row if the stream failed.
        let mut answer = None;
        for m in self.messages.iter().rev() {
            match m.role.as_str() {
                "assistant" if !m.content.is_empty() => {
                    answer = Some(m.content.clone());
                    break;
                }
                "error" => bail!("{}", m.content),
                _ => {}
            }
        }
        let Some(answer) = answer else {
            bail!("response finished without text");
        };
        if let Some(u) = usage {
            let cost = u.cost.map(|c| format!(" · ${c:.4}")).unwrap_or_default();
            note(
                opts.quiet,
                format!(
                    "tokens: {} → {} ({} cached){}",
                    u.prompt_tokens, u.completion_tokens, u.cache_read_tokens, cost
                ),
            );
        }
        Ok((answer, usage))
    }

    /// `nexus ask`: one turn, then a short wait for the model-generated
    /// session title so `nexus sessions` shows a real name instead of the
    /// prompt prefix. A slow title must never hold the ask hostage — capped.
    pub async fn ask_headless(&mut self, prompt: String, opts: TurnOpts) -> Result<AskOutcome> {
        let (answer, usage) = self.run_turn(prompt, opts).await?;

        if self.title_rx.is_some() {
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(15));
            tokio::pin!(timeout);
            loop {
                tokio::select! {
                    () = &mut timeout => break,
                    ev = self.next_event() => {
                        if let AppEvent::Title(t) = ev {
                            self.on_title_result(t);
                            break;
                        }
                    }
                }
            }
        }

        let session = self.session.clone().context("session vanished after ask")?;
        Ok(AskOutcome {
            answer,
            session_id: session.id,
            session_title: session.title,
            usage,
        })
    }

    /// `nexus chat`: a bare REPL — prompt on stderr, one turn per line,
    /// all turns in the one session the first turn creates.
    pub async fn chat_headless(&mut self, quiet: bool) -> Result<()> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        loop {
            eprint!("> ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                eprintln!();
                break;
            }
            let text = line.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if matches!(text.as_str(), "/quit" | "/exit" | "/q") {
                break;
            }
            self.run_turn(
                text,
                TurnOpts {
                    stream: true,
                    quiet,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// `nexus research <topic>`: run the full deep-research pipeline headless.
    ///
    /// Gate policy: with `approve` the pipeline runs ungated (survey and
    /// plan-approval are skipped entirely, like `/research!`). Without it,
    /// a gated run parks at each SurveyReady/PlanReady: when stdin is a
    /// terminal the prompt is printed and a reply is read from the line
    /// (empty reply skips the survey round; `approve` approves the plan);
    /// otherwise the run bails rather than hang.
    pub async fn research_headless(
        &mut self,
        topic: String,
        approve: bool,
        opts: TurnOpts,
    ) -> Result<ResearchOutcome> {
        if !self.backends.any() {
            bail!(
                "no API key configured — set one with /login in the TUI, or export \
                 $OPENROUTER_API_KEY / $OPENAI_API_KEY / $OPENCODE_API_KEY"
            );
        }
        if self.current_model.is_none() {
            bail!("no model selected — pass --model, or pick one in the TUI first");
        }
        let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
        self.execute(AppCommand::RunResearch {
            topic,
            gated: !approve,
        })?;
        // The execute above may have refused with a status line (already
        // running, no model, …) — surface the last one if no job started.
        let mut refusal = String::new();
        while let Some(ev) = self.pop_pending_event() {
            if let AppEvent::Status(s) = ev {
                refusal = s;
            }
        }
        if self.research_rx.is_none() {
            bail!("research didn't start: {refusal}");
        }

        let mut report: Option<String> = None;
        let mut error: Option<String> = None;
        loop {
            match self.next_event().await {
                AppEvent::Research(Some((session_id, space_id, space_name, update))) => {
                    if let super::research::ResearchUpdate::Stage { label, detail } = &update {
                        note(opts.quiet, stage_content(label, detail));
                    }
                    match &update {
                        super::research::ResearchUpdate::Done(Ok(text)) => {
                            report = Some(text.clone());
                        }
                        super::research::ResearchUpdate::Done(Err(e)) => error = Some(e.clone()),
                        _ => {}
                    }
                    self.on_research_done(Some((session_id, space_id, space_name, update)));
                    // A gate armed (or re-armed) by the handler above: answer
                    // it before draining anything else — the pipeline is
                    // parked and will not move until it gets a reply.
                    if !approve && let Some(gate) = self.survey_gate.as_ref() {
                        if !interactive {
                            bail!(
                                "research needs your input at a gate ({}), but stdin isn't a \
                                 terminal — re-run with --approve to skip the gates",
                                match &gate.phase {
                                    super::SurveyPhase::Clarify { .. } => "survey questions",
                                    super::SurveyPhase::Approve { .. } => "plan approval",
                                }
                            );
                        }
                        stream(&format!("\n{}\n", gate.prompt_content));
                        eprint!("> ");
                        let _ = std::io::stderr().flush();
                        let mut reply = String::new();
                        if std::io::stdin().read_line(&mut reply)? == 0 {
                            bail!("research needs your input — stdin closed");
                        }
                        self.execute(AppCommand::AnswerGate { text: reply })?;
                    }
                }
                AppEvent::Research(None) => break,
                _ => {} // unrelated background events; keep draining
            }
        }

        let session = self.session.clone().context("research session vanished")?;
        let Some(report) = report else {
            bail!(
                "{}",
                error.unwrap_or_else(|| "research finished without a report".to_string())
            );
        };
        Ok(ResearchOutcome {
            report,
            session_id: session.id,
            session_title: session.title,
        })
    }

    /// `nexus watch run`: run one watch (by id or topic prefix), all watches
    /// (`--all`), or the due ones (default). Each run drives its research
    /// job to completion before the next starts. Returns the reports, keyed
    /// by watch topic, for the caller to print.
    pub async fn watch_run_headless(
        &mut self,
        watch_ref: Option<&str>,
        all: bool,
        quiet: bool,
    ) -> Result<Vec<(String, ResearchOutcome)>> {
        let watches = self.db.list_all_watches().context("listing watches")?;
        let targets: Vec<Watch> = match (watch_ref, all) {
            (Some(r), _) => {
                let w = watches
                    .iter()
                    .find(|w| w.id.starts_with(r) || w.topic.contains(r))
                    .ok_or_else(|| {
                        anyhow!("no watch matching {r:?} — `nexus watch list` shows them")
                    })?;
                vec![w.clone()]
            }
            (None, true) => watches,
            (None, false) => crate::app::watches::due_watches(&watches, chrono::Utc::now()),
        };
        if targets.is_empty() {
            bail!("no watches to run — `nexus watch list` shows them");
        }
        let mut ran = Vec::new();
        for w in targets {
            note(quiet, format!("watch: {} …", w.topic));
            if !self.run_one_watch(&w) {
                note(
                    quiet,
                    "  could not start (no session to run from?) — skipped",
                );
                continue;
            }
            // Drain this watch's job to completion; its session is active
            // during the run, so `research_headless`-style gate handling is
            // not needed (watches are always ungated).
            let mut report: Option<String> = None;
            let mut error: Option<String> = None;
            loop {
                match self.next_event().await {
                    AppEvent::Research(Some((session_id, space_id, space_name, update))) => {
                        if let super::research::ResearchUpdate::Stage { label, detail } = &update {
                            note(quiet, stage_content(label, detail));
                        }
                        match &update {
                            super::research::ResearchUpdate::Done(Ok(text)) => {
                                report = Some(text.clone());
                            }
                            super::research::ResearchUpdate::Done(Err(e)) => {
                                error = Some(e.clone());
                            }
                            _ => {}
                        }
                        self.on_research_done(Some((session_id, space_id, space_name, update)));
                    }
                    AppEvent::Research(None) => break,
                    _ => {}
                }
            }
            let Some(report) = report else {
                bail!(
                    "{}",
                    error.unwrap_or_else(|| "watch finished without a report".to_string())
                );
            };
            // The watch row was repointed at its fresh session during
            // `run_one_watch` — re-read it for the closing line.
            let session = self
                .db
                .list_all_watches()
                .ok()
                .and_then(|ws| ws.into_iter().find(|x| x.id == w.id))
                .and_then(|x| self.db.get_session(&x.session_id).ok().flatten())
                .context("watch session vanished")?;
            ran.push((
                w.topic,
                ResearchOutcome {
                    report,
                    session_id: session.id,
                    session_title: session.title,
                },
            ));
        }
        Ok(ran)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!("nexus-cli-{}", uuid::Uuid::new_v4()));
        App::new(Db::open_in_memory().unwrap(), Some("k"), Space { root })
    }

    #[test]
    fn switch_space_by_name_switches_and_is_idempotent() {
        let mut app = test_app();
        let row = app.db.create_space("research").unwrap();
        let active_before = app.active_space.id.clone();
        assert_ne!(row.id, active_before);

        app.switch_space_cli("research").unwrap();
        assert_eq!(app.active_space.id, row.id);
        // Switching again (same space) is a no-op, not an error.
        app.switch_space_cli("research").unwrap();
        assert_eq!(app.active_space.id, row.id);
    }

    #[test]
    fn switch_space_unknown_name_bails() {
        let mut app = test_app();
        assert!(app.switch_space_cli("nope").is_err());
    }
}
