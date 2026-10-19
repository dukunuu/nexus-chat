use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{AppCommand, Popup, clamp_cursor};
use nexus_core::jobs::{Job, JobRun, JobSpec};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;

use super::chrome;

const LABELS: [&str; 12] = [
    "name",
    "prompt",
    "root",
    "mode (agent|auto)",
    "trigger (interval|once|until_done)",
    "interval minutes",
    "run-at (RFC 3339)",
    "stop command",
    "max iterations",
    "max cost USD",
    "max wall minutes",
    "notify (always|on_failure|on_change)",
];

#[derive(Default)]
pub struct JobsPopup {
    pub jobs: Vec<Job>,
    pub runs: Vec<JobRun>,
    pub selected: usize,
    pub form: Option<JobForm>,
}

pub struct JobForm {
    pub values: [String; 12],
    pub field: usize,
    pub error: String,
}

impl JobForm {
    fn new(root: String) -> Self {
        Self {
            values: [
                String::new(),
                String::new(),
                root,
                "agent".into(),
                "until_done".into(),
                "1440".into(),
                String::new(),
                String::new(),
                "5".into(),
                "1.00".into(),
                "30".into(),
                "always".into(),
            ],
            field: 0,
            error: String::new(),
        }
    }

    fn spec(&self) -> Result<JobSpec> {
        let value = &self.values;
        let spec = JobSpec {
            name: value[0].clone(),
            prompt: value[1].clone(),
            root: value[2].clone(),
            mode: value[3].clone(),
            trigger_kind: value[4].clone(),
            interval_minutes: value[5].parse().context("interval must be an integer")?,
            run_at: (!value[6].is_empty()).then(|| value[6].clone()),
            stop_command: (!value[7].is_empty()).then(|| value[7].clone()),
            max_iters: value[8].parse().context("iterations must be an integer")?,
            max_cost_usd: value[9].parse().context("cost must be a number")?,
            max_wall_minutes: value[10]
                .parse()
                .context("wall minutes must be an integer")?,
            notify: value[11].clone(),
            ..JobSpec::default()
        };
        spec.validate()?;
        Ok(spec)
    }
}

pub fn open(app: &mut AppView, create: bool) -> Result<()> {
    app.collect_job_notifications()?;
    refresh(app)?;
    app.jobs_popup.form = if create {
        Some(JobForm::new(
            app.workspace
                .as_ref()
                .map(|workspace| workspace.root().display().to_string())
                .unwrap_or_default(),
        ))
    } else {
        None
    };
    app.popup = Popup::Jobs;
    Ok(())
}

fn refresh(app: &mut AppView) -> Result<()> {
    app.jobs_popup.jobs = app.db.list_jobs()?;
    app.jobs_popup.runs = app.db.list_job_runs()?;
    app.jobs_popup.selected = app
        .jobs_popup
        .selected
        .min(app.jobs_popup.jobs.len().saturating_sub(1));
    Ok(())
}

pub fn render(frame: &mut Frame, app: &AppView) {
    let area = crate::ui::centered(frame.area(), chrome::WIDE.0, chrome::WIDE.1);
    let creating = app.jobs_popup.form.is_some();
    let inner = chrome::render_hinted(
        frame,
        area,
        chrome::popup_title(app, if creating { "new job" } else { "jobs" }),
        if creating {
            "tab/↑↓ field · enter next · ctrl+s save · esc cancel"
        } else {
            "↑↓ select · n new · space enable · r run · d delete · enter logs · esc close"
        },
        app,
        true,
        chrome::Tone::Normal,
    );
    let dim = Style::default().fg(app.theme.fg_dim);
    if let Some(form) = &app.jobs_popup.form {
        let mut lines: Vec<Line<'_>> = LABELS
            .iter()
            .enumerate()
            .map(|(index, label)| {
                Line::styled(
                    format!(
                        "{} {label}: {}",
                        if index == form.field { "▸" } else { " " },
                        form.values[index].replace('\n', " ")
                    ),
                    if index == form.field {
                        Style::default().fg(app.theme.accent)
                    } else {
                        dim
                    },
                )
            })
            .collect();
        lines.push(Line::styled(
            &form.error,
            Style::default().fg(app.theme.accent),
        ));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
    let device = app.db.device_id().unwrap_or_default();
    let items: Vec<_> = if app.jobs_popup.jobs.is_empty() {
        vec![chrome::empty_placeholder(
            "no jobs · n to create",
            &app.theme,
        )]
    } else {
        app.jobs_popup
            .jobs
            .iter()
            .map(|job| {
                let run = app.jobs_popup.runs.iter().find(|run| run.job_id == job.id);
                let status = run.map_or("never run", |run| run.status.as_str());
                ListItem::new(Line::from(format!(
                    "{} · {} · {} · {} · {status}{}",
                    job.spec.name,
                    job.spec.kind,
                    job.spec.trigger_kind,
                    if job.enabled == 1 {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    if job.device_id == device {
                        ""
                    } else {
                        " · other device (read-only)"
                    }
                )))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !app.jobs_popup.jobs.is_empty() {
        state.select(Some(app.jobs_popup.selected));
    }
    chrome::render_list(
        frame,
        chrome::standard_list(items, &app.theme),
        &mut state,
        parts[0],
        app.jobs_popup.jobs.len(),
        1,
        app,
    );
    if let Some(job) = app.jobs_popup.jobs.get(app.jobs_popup.selected)
        && let Some(run) = app.jobs_popup.runs.iter().find(|run| run.job_id == job.id)
    {
        frame.render_widget(Paragraph::new(run_details(job, run)).style(dim), parts[1]);
    }
}

fn run_details(job: &Job, run: &JobRun) -> String {
    let location = if job.spec.kind == "research" {
        "research"
    } else {
        run.branch.as_deref().unwrap_or("non-git copy")
    };
    let judgement = (job.spec.kind == "agent")
        .then(|| {
            run.summary
                .lines()
                .rev()
                .find(|line| line.starts_with("[judge] "))
        })
        .flatten();
    if let Some(judgement) = judgement {
        format!("{location}\n{judgement}\n{}", run.summary)
    } else {
        format!("{location}\n{}", run.summary)
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    if app.jobs_popup.form.is_some() {
        return form_key(app, key);
    }
    let job = app.jobs_popup.jobs.get(app.jobs_popup.selected).cloned();
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => {
            app.jobs_popup.selected =
                clamp_cursor(app.jobs_popup.selected, app.jobs_popup.jobs.len(), -1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.jobs_popup.selected =
                clamp_cursor(app.jobs_popup.selected, app.jobs_popup.jobs.len(), 1);
        }
        KeyCode::Char('n') => return open(app, true),
        KeyCode::Char(' ' | 'r' | 'd') => {
            if let Some(job) = job {
                let cmd = match key.code {
                    KeyCode::Char(' ') => AppCommand::SetJobEnabled {
                        id: job.id,
                        on: job.enabled != 1,
                    },
                    KeyCode::Char('r') => AppCommand::RunJobNow { id: job.id },
                    _ => AppCommand::DeleteJob { id: job.id },
                };
                app.core.execute(cmd)?;
                refresh(app)?;
            }
        }
        KeyCode::Enter => {
            if let Some(job) = job
                && let Some(run) = app.jobs_popup.runs.iter().find(|run| run.job_id == job.id)
            {
                let id = run.id.clone();
                app.core.switch_to_session_by_id(&id)?;
                app.popup = Popup::None;
            }
        }
        _ => {}
    }
    Ok(())
}

fn form_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let form = app.jobs_popup.form.as_mut().expect("job form");
    let save = key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL)
        || key.code == KeyCode::Enter && form.field == LABELS.len() - 1;
    if save {
        let result = form
            .spec()
            .and_then(|spec| app.core.execute(AppCommand::CreateJob { spec }));
        if let Err(error) = result {
            app.jobs_popup.form.as_mut().expect("job form").error = error.to_string();
        } else {
            open(app, false)?;
        }
        return Ok(());
    }
    match key.code {
        KeyCode::Esc => {
            app.jobs_popup.form = None;
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
            form.field = (form.field + 1) % LABELS.len();
        }
        KeyCode::BackTab | KeyCode::Up => {
            form.field = (form.field + LABELS.len() - 1) % LABELS.len();
        }
        KeyCode::Backspace => {
            form.values[form.field].pop();
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            form.values[form.field].push(ch);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_new_defaults_to_agent_workspace_and_keeps_invalid_form_open() {
        use nexus_core::app::{App, WorkMode};
        use nexus_core::db::Db;
        use nexus_core::space::Space;

        let dir = std::env::temp_dir().join(format!("nexus-job-form-{}", uuid::Uuid::new_v4()));
        let root = dir.join("workspace");
        let data = dir.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(data.join("spaces")).unwrap();
        let mut app = AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space { root: data },
        ));
        app.execute(AppCommand::SetWorkMode {
            mode: WorkMode::Agent,
            root: Some(root.display().to_string()),
        })
        .unwrap();
        app.run_command("job new").unwrap();
        assert_eq!(app.popup, Popup::Jobs);
        let save = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        handle_key(&mut app, save).unwrap();
        let form = app.jobs_popup.form.as_mut().unwrap();
        assert!(form.error.contains("name"));
        assert_eq!(
            std::path::Path::new(&form.values[2]),
            root.canonicalize().unwrap()
        );
        form.values[0] = "fix tests".into();
        form.values[1] = "fix the test suite".into();
        form.values[9] = "0".into();
        handle_key(&mut app, save).unwrap();
        assert!(app.jobs_popup.form.as_ref().unwrap().error.contains("cost"));
        assert!(app.db.list_jobs().unwrap().is_empty());
        app.jobs_popup.form.as_mut().unwrap().values[9] = "1".into();
        handle_key(&mut app, save).unwrap();
        assert!(app.jobs_popup.form.is_none());
        assert_eq!(app.jobs_popup.jobs.len(), 1);
        assert_eq!(app.jobs_popup.jobs[0].spec.prompt, "fix the test suite");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn form_validates_required_fields_trigger_and_budgets() {
        let mut form = JobForm::new("/workspace".into());
        assert!(form.spec().unwrap_err().to_string().contains("name"));
        form.values[0] = "fix tests".into();
        assert!(form.spec().unwrap_err().to_string().contains("prompt"));
        form.values[1] = "fix the failing tests".into();
        assert!(form.spec().is_ok());
        form.values[4] = "once".into();
        assert!(form.spec().is_err());
        form.values[6] = "2026-01-01T18:00:00Z".into();
        assert!(form.spec().is_ok());
        form.values[9] = "NaN".into();
        assert!(form.spec().is_err());
    }
}
