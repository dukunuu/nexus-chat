use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::*;
use crate::app::{App, AppCommand, Backends};
use crate::db::Db;
use crate::provider::openrouter::OpenRouter;
use crate::space::Space;

struct Fixture {
    dir: PathBuf,
    app: App,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("nexus-jobs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("data/spaces")).unwrap();
        fs::create_dir(dir.join("workspace")).unwrap();
        let app = App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space {
                root: dir.join("data"),
            },
        );
        Self { dir, app }
    }

    fn root(&self) -> PathBuf {
        self.dir.join("workspace")
    }

    fn job(&mut self, trigger: &str) -> Job {
        self.app.current_model = Some("local:fake".into());
        self.app
            .execute(AppCommand::CreateJob {
                spec: JobSpec {
                    name: "fix tests".into(),
                    prompt: "fix the tests".into(),
                    root: self.root().display().to_string(),
                    trigger_kind: trigger.into(),
                    run_at: Some("2026-01-01T18:00:00Z".into()),
                    ..JobSpec::default()
                },
            })
            .unwrap();
        self.app.db.list_jobs().unwrap().pop().unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn repository(&self) {
        self.git(&["init", "-b", "main"]);
        self.git(&["config", "user.name", "Job Test"]);
        self.git(&["config", "user.email", "job@example.invalid"]);
        fs::write(self.root().join("original"), "original\n").unwrap();
        self.git(&["add", "original"]);
        self.git(&["commit", "-m", "initial"]);
    }

    async fn model(&mut self, responses: Vec<Value>) -> tokio::task::JoinHandle<Vec<Value>> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        self.app.backends = Backends {
            local: Some(
                OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap()))
                    .unwrap(),
            ),
            ..Backends::default()
        };
        self.app.memory_model.clear();
        self.app.saved.agent.allow = vec!["fs write **".into()];
        self.app.saved.agent.decider = Some(crate::config::ModelDecider::Rules);
        tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                requests.push(serde_json::from_slice(&body).unwrap());
                let body = format!("data: {response}\n\ndata: [DONE]\n\n");
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(headers.as_bytes())
                    .await
                    .unwrap();
                // A parked run may close the response as soon as it has the denied call.
                let _ = stream.get_mut().write_all(body.as_bytes()).await;
            }
            requests
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(cost: f64) -> Value {
    json!({"choices":[{"delta":{"content":"iteration complete"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"cost":cost}})
}

fn write_file(path: &str) -> Value {
    json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-write","type":"function","function":{"name":"fs","arguments":json!({"action":"write","path":path,"content":"fixed\n"}).to_string()}}]},"finish_reason":"tool_calls"}]})
}

#[test]
fn scheduling_interval_once_disabled_foreign_and_never_run() {
    let mut fixture = Fixture::new();
    let mut job = fixture.job("interval");
    let now = parse_time("2026-01-02T18:00:00Z").unwrap();
    let device = job.device_id.clone();
    assert_eq!(due(&[job.clone()], now, &device).len(), 1);
    job.last_run_at = Some("2026-01-02T17:59:00Z".into());
    assert!(due(&[job.clone()], now, &device).is_empty());
    job.last_run_at = Some("2026-01-01T18:00:00Z".into());
    assert_eq!(due(&[job.clone()], now, &device).len(), 1);
    job.enabled = 0;
    assert!(due(&[job.clone()], now, &device).is_empty());
    job.enabled = 1;
    assert!(due(&[job.clone()], now, "other-device").is_empty());
    job.spec.trigger_kind = "once".into();
    job.last_run_at = None;
    assert_eq!(due(&[job.clone()], now, &device).len(), 1);
    job.spec.run_at = Some("2026-01-03T18:00:00Z".into());
    assert!(due(&[job.clone()], now, &device).is_empty());
    job.spec.run_at = Some("2026-01-01T18:00:00Z".into());
    job.last_run_at = Some("2026-01-01T19:00:00Z".into());
    assert!(due(&[job], now, &device).is_empty());
}

#[tokio::test]
async fn worktree_commits_agent_edits_without_touching_users_branch_or_dirty_files() {
    let mut fixture = Fixture::new();
    fixture.repository();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    fs::write(fixture.root().join("original"), "user edits\n").unwrap();
    fs::write(fixture.root().join("untracked"), "keep me").unwrap();
    let status = fixture.git(&["status", "--porcelain"]);
    let mut job = fixture.job("once");
    job.spec.mode = "auto".into();
    let server = fixture.model(vec![write_file("fixed"), text(0.01)]).await;
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "done", "{}", run.summary);
    let branch = run.branch.as_deref().unwrap();
    assert_eq!(fixture.git(&["show", &format!("{branch}:fixed")]), "fixed");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.git(&["branch", "--show-current"]), "main");
    assert_eq!(fixture.git(&["status", "--porcelain"]), status);
    let tree = fixture.app.space.root.join("job-worktrees").join(&run.id);
    assert!(!tree.exists());
    assert_eq!(fixture.app.db.list_job_runs().unwrap()[0].id, run.id);
    let session = fixture.app.db.get_session(&run.id).unwrap().unwrap();
    assert!(session.title.starts_with("job: fix tests · "));
    let messages = fixture.app.db.load_messages(&run.id).unwrap();
    assert!(messages.iter().any(|message| message.role == "tool_call"));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn non_git_copy_retains_edits_and_a_review_diff() {
    let mut fixture = Fixture::new();
    fs::write(fixture.root().join("original"), "original\n").unwrap();
    let mut job = fixture.job("once");
    job.spec.mode = "auto".into();
    let server = fixture.model(vec![write_file("fixed"), text(0.01)]).await;
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "done", "{}", run.summary);
    assert!(run.branch.is_none());
    assert!(run.summary.contains("1 files changed"));
    assert!(run.summary.contains("+++ b/fixed"));
    assert!(run.summary.contains("+fixed"));
    let tree = fixture.app.space.root.join("job-worktrees").join(&run.id);
    assert!(tree.join("fixed").is_file());
    assert!(!fixture.root().join("fixed").exists());
    server.await.unwrap();
}

#[tokio::test]
async fn oversized_copy_fails_clearly_before_provider_or_copy() {
    let mut fixture = Fixture::new();
    fs::File::create(fixture.root().join("large"))
        .unwrap()
        .set_len(201 * 1024 * 1024)
        .unwrap();
    let job = fixture.job("until_done");
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "failed");
    assert!(run.summary.contains("200 MiB"));
    let tree = fixture.app.space.root.join("job-worktrees").join(&run.id);
    assert!(!tree.exists());
}

#[tokio::test]
async fn gated_agent_call_parks_and_records_summary_without_changing_files() {
    let mut fixture = Fixture::new();
    let job = fixture.job("until_done");
    let server = fixture.model(vec![write_file("fixed")]).await;
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "parked", "{}", run.summary);
    assert!(run.summary.contains(PARKED));
    assert!(run.summary.contains("fixed"));
    assert!(!fixture.root().join("fixed").exists());
    let tree = fixture.app.space.root.join("job-worktrees").join(&run.id);
    assert!(!tree.join("fixed").exists());
    let messages = fixture.app.db.load_messages(&run.id).unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains(PARKED))
    );
    fixture.app.collect_job_notifications().unwrap();
    assert!(fixture.app.unread.contains(&run.id));
    assert_eq!(fixture.app.notifications.len(), 1);
    fixture.app.collect_job_notifications().unwrap();
    assert_eq!(fixture.app.notifications.len(), 1);
    server.await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn until_done_fixes_file_on_second_iteration_and_retains_failed_check_context() {
    let mut fixture = Fixture::new();
    let mut job = fixture.job("until_done");
    job.spec.mode = "auto".into();
    job.spec.stop_command = Some("test -f fixed".into());
    let server = fixture
        .model(vec![text(0.01), write_file("fixed"), text(0.02)])
        .await;
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "done", "{}", run.summary);
    assert_eq!(run.iters, 2);
    assert!((run.cost_usd - 0.03).abs() < f64::EPSILON);
    let requests = server.await.unwrap();
    let history = requests[1]["messages"].to_string();
    assert!(history.contains("test -f fixed"));
    assert!(history.contains("exit 1"));
    assert!(history.contains("Keep working until it passes"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn iteration_and_cost_budgets_stop_until_done_runs() {
    for (max_iters, cost, expected_iters) in [(2, 0.01, 2), (5, 1.01, 1)] {
        let mut fixture = Fixture::new();
        let mut job = fixture.job("until_done");
        job.spec.max_iters = max_iters;
        job.spec.stop_command = Some("test -f fixed".into());
        let server = fixture
            .model((0..expected_iters).map(|_| text(cost)).collect())
            .await;
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        assert_eq!(run.status, "budget", "{}", run.summary);
        assert_eq!(run.iters, expected_iters);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn run_due_is_one_pass_and_lock_excludes_second_runner() {
    let mut fixture = Fixture::new();
    let job = fixture.job("until_done");
    let lock = DaemonLock::acquire(&fixture.app.space.root).unwrap();
    assert!(DaemonLock::acquire(&fixture.app.space.root).is_err());
    assert!(run_due(&mut fixture.app).await.is_err());
    drop(lock);
    let server = fixture.model(vec![text(0.01)]).await;
    let runs = tokio::time::timeout(Duration::from_secs(5), run_due(&mut fixture.app))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "done");
    assert!(run_due(&mut fixture.app).await.unwrap().is_empty());
    assert!(fixture.app.db.list_jobs().unwrap()[0].last_run_at.is_some());
    assert_eq!(fixture.app.db.list_jobs().unwrap()[0].id, job.id);
    server.await.unwrap();
}

#[tokio::test]
async fn sync_keeps_both_tables_and_foreign_jobs_read_only() {
    let mut owner = Fixture::new();
    let job = owner.job("until_done");
    let server = owner.model(vec![text(0.01)]).await;
    run_with_app(&mut owner.app, &job).await.unwrap();
    let mut peer = Fixture::new();
    let changes = crate::sync::build_changeset(&owner.app.db, None, "owner").unwrap();
    for table in ["jobs", "job_runs"] {
        assert!(changes.rows.iter().any(|row| row.table == table));
    }
    crate::sync::apply_changeset(&peer.app.db, &peer.app.space, &changes, None).unwrap();
    let listed = peer.app.db.list_jobs().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(peer.app.db.list_job_runs().unwrap().len(), 1);
    assert!(due(&listed, Utc::now(), &peer.app.db.device_id().unwrap()).is_empty());
    assert!(run_with_app(&mut peer.app, &listed[0]).await.is_err());
    assert!(
        peer.app
            .execute(AppCommand::SetJobEnabled {
                id: job.id.clone(),
                on: false
            })
            .is_err()
    );
    assert!(
        peer.app
            .execute(AppCommand::DeleteJob { id: job.id })
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn queued_run_bypasses_disabled_schedule_and_is_consumed_once() {
    let mut fixture = Fixture::new();
    let mut job = fixture.job("until_done");
    job.enabled = 0;
    fixture.app.db.save_job(&job).unwrap();
    request_run(&fixture.app.space.root, &job).unwrap();
    let server = fixture.model(vec![text(0.01)]).await;
    let runs = run_due(&mut fixture.app).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "done");
    assert!(run_due(&mut fixture.app).await.unwrap().is_empty());
    assert_eq!(fixture.app.db.list_jobs().unwrap()[0].enabled, 0);
    server.await.unwrap();
}

#[test]
fn creation_rejects_data_roots_and_deletion_replicates() {
    let mut fixture = Fixture::new();
    let invalid = JobSpec {
        name: "invalid".into(),
        prompt: "fix".into(),
        root: fixture.dir.display().to_string(),
        ..JobSpec::default()
    };
    assert!(
        fixture
            .app
            .execute(AppCommand::CreateJob { spec: invalid })
            .is_err()
    );
    assert!(fixture.app.db.list_jobs().unwrap().is_empty());
    let job = fixture.job("until_done");
    let peer = Fixture::new();
    let changes = crate::sync::build_changeset(&fixture.app.db, None, "owner").unwrap();
    crate::sync::apply_changeset(&peer.app.db, &peer.app.space, &changes, None).unwrap();
    assert_eq!(peer.app.db.list_jobs().unwrap().len(), 1);
    fixture
        .app
        .execute(AppCommand::DeleteJob { id: job.id })
        .unwrap();
    let changes = crate::sync::build_changeset(&fixture.app.db, None, "owner").unwrap();
    crate::sync::apply_changeset(&peer.app.db, &peer.app.space, &changes, None).unwrap();
    assert!(fixture.app.db.list_jobs().unwrap().is_empty());
    assert!(peer.app.db.list_jobs().unwrap().is_empty());
}

#[tokio::test]
async fn agent_batch_parks_before_unapproved_writes() {
    let mut fixture = Fixture::new();
    let job = fixture.job("until_done");
    let response = json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"batch-call","type":"function","function":{
            "name":"batch","arguments":json!({"calls":[
                {"tool":"fs","arguments":{"action":"write","path":"fixed","content":"fixed"}},
                {"tool":"shell","arguments":{"action":"run","cmd":"echo needs approval"}}
            ]}).to_string()
        }
    }]},"finish_reason":"tool_calls"}]});
    let server = fixture.model(vec![response]).await;
    let run = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(run.status, "parked", "{}", run.summary);
    let tree = fixture.app.space.root.join("job-worktrees").join(&run.id);
    assert!(!tree.join("fixed").exists());
    server.await.unwrap();
}

#[tokio::test]
async fn on_failure_suppresses_success_and_notification_cursor_is_local() {
    let mut fixture = Fixture::new();
    let spec = JobSpec {
        name: "morning".into(),
        prompt: "report".into(),
        root: fixture.root().display().to_string(),
        notify: "on_failure".into(),
        ..JobSpec::default()
    };
    fixture.app.execute(AppCommand::CreateJob { spec }).unwrap();
    fixture.app.current_model = Some("local:fake".into());
    let job = fixture.app.db.list_jobs().unwrap().pop().unwrap();
    let server = fixture.model(vec![text(0.01)]).await;
    let done = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(done.status, "done");
    assert!(fixture.app.db.setting("jobs_seen").is_none());
    fixture.app.collect_job_notifications().unwrap();
    assert!(fixture.app.notifications.is_empty());
    assert!(!fixture.app.unread.contains(&done.id));
    fs::File::create(fixture.root().join("large"))
        .unwrap()
        .set_len(201 * 1024 * 1024)
        .unwrap();
    let failed = run_with_app(&mut fixture.app, &job).await.unwrap();
    assert_eq!(failed.status, "failed");
    fixture.app.collect_job_notifications().unwrap();
    assert_eq!(fixture.app.notifications.len(), 1);
    assert!(fixture.app.unread.contains(&failed.id));
    let changes = crate::sync::build_changeset(&fixture.app.db, None, "owner").unwrap();
    assert!(
        !changes
            .rows
            .iter()
            .any(|row| { row.table == "app_settings" && row.row["key"] == "jobs_seen" })
    );
    server.await.unwrap();
}

#[test]
fn service_definition_is_hermetic_and_runs_jobs_not_host() {
    let fixture = Fixture::new();
    let path = service::install_service_at(
        &fixture.dir,
        &fixture.dir.join("config"),
        Path::new("/opt/Nexus & 100%/nexus"),
    )
    .unwrap();
    let contents = fs::read_to_string(path).unwrap();
    assert!(contents.contains("jobs"));
    assert!(contents.contains("daemon"));
    assert!(!contents.contains(" host "));
    if cfg!(target_os = "macos") {
        assert!(contents.contains("chat.nexus.jobs"));
        assert!(contents.contains("Nexus &amp; 100%"));
    } else {
        assert!(contents.contains("100%%"));
    }
}
