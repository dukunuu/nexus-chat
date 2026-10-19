use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use tokio::sync::watch;

use super::{Job, JobApprover, JobRun, PARKED, due, tree::Tree};
use crate::app::{App, AppCommand, WorkMode, headless::TurnOpts};

/// An OS-held lock: process exit releases it, without stale-PID races.
pub struct DaemonLock {
    file: File,
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl DaemonLock {
    pub fn acquire(data: &Path) -> Result<Self> {
        std::fs::create_dir_all(data)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data.join("jobs.lock"))?;
        file.try_lock()
            .context("another jobs runner holds jobs.lock")?;
        Ok(Self { file })
    }
}

/// Boot exactly the ordinary CLI app, then run this device's job.
pub async fn run_once(job: &Job) -> Result<JobRun> {
    let saved = crate::config::load_all_providers().await?;
    let mut app = crate::boot(saved).await?;
    let _lock = DaemonLock::acquire(&app.space.root)?;
    run_with_app(&mut app, job).await
}

/// Run with an already bootstrapped app (also permits hermetic fake providers).
pub async fn run_with_app(app: &mut App, job: &Job) -> Result<JobRun> {
    let (_sender, mut cancelled) = watch::channel(false);
    run(app, job, &mut cancelled).await
}

async fn run(app: &mut App, job: &Job, cancelled: &mut watch::Receiver<bool>) -> Result<JobRun> {
    ensure!(
        job.device_id == app.db.device_id()?,
        "workspace belongs to another device; this job is read-only"
    );
    job.spec.validate()?;
    if let Some(space) = app
        .db
        .list_spaces()?
        .into_iter()
        .find(|space| space.id == job.space_id)
    {
        app.set_active_space(space);
    } else {
        anyhow::bail!("job space no longer exists");
    }
    if let Some(session) = app.db.get_session(&job.session_id)? {
        app.current_model = Some(session.model);
    }
    let model = app.current_model.clone().unwrap_or_default();
    let now = Utc::now().to_rfc3339();
    let title = format!("job: {} · {}", job.spec.name, Utc::now().format("%Y-%m-%d"));
    let mut session = app
        .db
        .create_session(&title, &model, &job.space_id, "chat")?;
    app.db
        .set_session_title(&session.id, &title, Some(&format!("job-{}", session.id)))?;
    session.slug = Some(format!("job-{}", session.id));
    let mut run = JobRun {
        id: session.id.clone(),
        job_id: job.id.clone(),
        device_id: job.device_id.clone(),
        started_at: now.clone(),
        finished_at: None,
        status: "running".into(),
        iters: 0,
        cost_usd: 0.0,
        branch: None,
        summary: String::new(),
        updated_at: now.clone(),
    };
    app.db.save_job_run(&run)?;
    app.db.conn().execute(
        "UPDATE jobs SET session_id = ?2, last_run_at = ?3, updated_at = ?3 WHERE id = ?1",
        (&job.id, &run.id, &now),
    )?;
    app.session = Some(session);
    app.messages.clear();
    let result = execute(app, job, &mut run, cancelled).await;
    if let Err(error) = result {
        run.status = "failed".into();
        run.summary = format!("{}\n{error:#}", run.summary);
    }
    run.cost_usd = app.db.conn().query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM usage_log WHERE session_id = ?1",
        [&run.id],
        |row| row.get(0),
    )?;
    run.finished_at = Some(Utc::now().to_rfc3339());
    run.updated_at = run.finished_at.clone().unwrap_or_default();
    app.db.save_job_run(&run)?;
    if app.interactive_ui {
        app.collect_job_notifications()?;
    }
    Ok(run)
}

async fn execute(
    app: &mut App,
    job: &Job,
    run: &mut JobRun,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<()> {
    app.validate_job_root(&job.spec.root)?;
    let started = Instant::now();
    let limit = Duration::from_secs(u64::from(job.spec.max_wall_minutes) * 60);
    let tree = Tree::prepare(
        Path::new(&job.spec.root),
        &app.space.root,
        &job.spec.name,
        &run.id,
    )?;
    run.branch.clone_from(&tree.branch);
    app.db.save_job_run(run)?;
    let result = iterate(app, job, run, &tree.path, started + limit, cancelled).await;
    let _ = app.execute(AppCommand::Cancel { task: None });
    std::mem::take(&mut app.procs).shutdown().await;
    let review = tree.finish(&job.spec.name, &run.id);
    if !run.summary.is_empty() {
        run.summary.push('\n');
    }
    match &review {
        Ok(review) => run.summary.push_str(review),
        Err(_) => write!(run.summary, "retained tree: {}", tree.path.display())?,
    }
    result?;
    review?;
    Ok(())
}

async fn iterate(
    app: &mut App,
    job: &Job,
    run: &mut JobRun,
    root: &Path,
    deadline: Instant,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<()> {
    let approver = Arc::new(JobApprover::default());
    app.enter_job_workspace(
        root,
        if job.spec.mode == "auto" {
            WorkMode::AgentAuto
        } else {
            WorkMode::Agent
        },
        Arc::clone(&approver),
    )?;
    let mut prompt = job.spec.prompt.clone();
    loop {
        if *cancelled.borrow() {
            run.status = "cancelled".into();
            break;
        }
        if run.iters >= job.spec.max_iters
            || run.cost_usd >= job.spec.max_cost_usd
            || Instant::now() >= deadline
        {
            run.status = "budget".into();
            break;
        }
        run.iters += 1;
        let result = tokio::select! {
            result = app.ask_headless(prompt, TurnOpts { stream: false, quiet: true }) => Some(result),
            () = tokio::time::sleep_until(deadline.into()) => None,
            _ = cancelled.changed() => {
                run.status = "cancelled".into();
                None
            }
        };
        let Some(result) = result else {
            if run.status != "cancelled" {
                run.status = "budget".into();
            }
            break;
        };
        if let Some(summary) = approver.summary() {
            run.status = "parked".into();
            run.summary = summary;
            if let Ok(outcome) = result {
                run.cost_usd += outcome.usage.and_then(|usage| usage.cost).unwrap_or(0.0);
            }
            break;
        }
        let outcome = result?;
        run.cost_usd += outcome.usage.and_then(|usage| usage.cost).unwrap_or(0.0);
        run.summary = outcome.answer;
        app.db.save_job_run(run)?;
        if job.spec.trigger_kind != "until_done"
            || job.spec.stop_command.as_deref().is_none_or(str::is_empty)
        {
            run.status = "done".into();
            break;
        }
        let command = job.spec.stop_command.as_deref().expect("stop command");
        if !crate::workspace::shell::write_confinement_available() {
            run.status = "parked".into();
            run.summary = format!("{PARKED}\nstop check needs shell write confinement: {command}");
            break;
        }
        let args = serde_json::json!({"cmd": command, "timeout_s": 600}).to_string();
        let db_path = app.space.db_path();
        let check = tokio::select! {
            output = crate::workspace::shell::action(app.workspace.as_deref().expect("job workspace"), Some(&db_path), Some(root), "shell_run", &args, false) => Some(output),
            () = tokio::time::sleep_until(deadline.into()) => None,
            _ = cancelled.changed() => {
                run.status = "cancelled".into();
                None
            }
        };
        let Some(check) = check else {
            if run.status != "cancelled" {
                run.status = "budget".into();
            }
            break;
        };
        if check.starts_with("exit 0 ·") {
            run.status = "done".into();
            break;
        }
        let exit = check
            .lines()
            .next()
            .unwrap_or("unknown exit")
            .split(" ·")
            .next()
            .unwrap_or("unknown exit");
        let mut tail: Vec<_> = check.lines().rev().take(60).collect();
        tail.reverse();
        prompt = format!(
            "the stop check `{command}` failed ({exit}):\n{}\nKeep working until it passes.",
            tail.join("\n")
        );
    }
    Ok(())
}

/// Queue an immediate run for the lock-owning daemon (or the next run-due pass).
/// IDs come from persisted jobs, never from workspace paths.
pub fn request_run(data: &Path, job: &Job) -> Result<()> {
    ensure!(uuid::Uuid::parse_str(&job.id).is_ok(), "invalid job id");
    let queue = data.join("jobs-now");
    std::fs::create_dir_all(&queue)?;
    std::fs::write(queue.join(&job.id), [])?;
    Ok(())
}

async fn pass(app: &mut App, cancelled: &mut watch::Receiver<bool>) -> Result<Vec<JobRun>> {
    let jobs = app.db.list_jobs()?;
    let device = app.db.device_id()?;
    let mut targets = due(&jobs, Utc::now(), &device);
    for job in jobs {
        if job.device_id == device
            && uuid::Uuid::parse_str(&job.id).is_ok()
            && app.space.root.join("jobs-now").join(&job.id).is_file()
            && !targets.iter().any(|target| target.id == job.id)
        {
            targets.push(job);
        }
    }
    let mut results = Vec::new();
    for job in targets {
        if *cancelled.borrow() {
            break;
        }
        let request = app.space.root.join("jobs-now").join(&job.id);
        if uuid::Uuid::parse_str(&job.id).is_ok() && request.is_file() {
            std::fs::remove_file(request)?;
        }
        results.push(run(app, &job, cancelled).await?);
    }
    Ok(results)
}

/// Run one scheduling pass and return, including when nothing is due.
pub async fn run_due(app: &mut App) -> Result<Vec<JobRun>> {
    let _lock = DaemonLock::acquire(&app.space.root)?;
    let (_sender, mut cancelled) = watch::channel(false);
    pass(app, &mut cancelled).await
}

/// Serial scheduler with an OS lock and graceful SIGINT/SIGTERM cancellation.
pub async fn daemon(app: &mut App) -> Result<()> {
    let _lock = DaemonLock::acquire(&app.space.root)?;
    let (sender, mut cancelled) = watch::channel(false);
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result?,
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        let _ = sender.send(true);
        Ok::<_, std::io::Error>(())
    });
    let result = async {
        loop {
            pass(app, &mut cancelled).await?;
            if *cancelled.borrow() {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(60)) => {},
                _ = cancelled.changed() => break,
            }
        }
        Ok(())
    }
    .await;
    signals.abort();
    result
}
