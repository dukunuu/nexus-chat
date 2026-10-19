use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::*;
use crate::app::{App, AppCommand};
use crate::config::ModelDecider;
use crate::db::Db;
use crate::provider::openrouter::{OpenRouter, decider_mock::server};
use crate::space::Space;

struct Fixture {
    root: PathBuf,
    app: App,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-job-judge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("data/spaces")).unwrap();
        std::fs::create_dir(root.join("work")).unwrap();
        let mut app = App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space {
                root: root.join("data"),
            },
        );
        app.memory_model.clear();
        app.current_model = Some("local:fake".into());
        app.saved.agent.decider = Some(ModelDecider::Rules);
        Self { root, app }
    }

    fn job(&mut self, trigger: &str) -> Job {
        self.app
            .execute(AppCommand::CreateJob {
                spec: JobSpec {
                    name: "judge test".into(),
                    prompt: "finish the work".into(),
                    root: self.root.join("work").display().to_string(),
                    trigger_kind: trigger.into(),
                    max_iters: 2,
                    ..JobSpec::default()
                },
            })
            .unwrap();
        self.app.db.list_jobs().unwrap().pop().unwrap()
    }

    fn jev(&mut self, provider: OpenRouter) {
        self.app.saved.openrouter_key = Some("mock-decider-key".into());
        self.app.saved.agent.decider = Some(ModelDecider::Jev);
        self.app.backends.openrouter = Some(provider);
    }

    async fn chat(&mut self, answers: &[&str]) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        self.app.backends.local = Some(
            OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap(),
        );
        let answers: Vec<String> = answers.iter().map(ToString::to_string).collect();
        tokio::spawn(async move {
            for answer in answers {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let response =
                    json!({"choices":[{"delta":{"content":answer},"finish_reason":"stop"}]});
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
                stream.get_mut().write_all(body.as_bytes()).await.unwrap();
            }
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn state() -> JudgeState {
    JudgeState {
        session_id: "session-judge".into(),
        task: "finish the work".into(),
        iteration: 2,
        last_answer: "implemented the change".into(),
        files_changed: "src/lib.rs | 1 +".into(),
        last_check_output: "no configured check".into(),
    }
}

fn response(choice: &str, confidence: f64) -> String {
    json!({"answers":{"job":{"choice":choice,"confidence":confidence,"reason":"Please choose the target."}},"usage":{"cost":0.002}}).to_string()
}

#[tokio::test]
async fn judge_sends_one_choice_with_compact_state_and_records_the_answer() {
    let mut fixture = Fixture::new();
    let (provider, requests) = server(200, response("done", 0.9), Duration::ZERO, 1).await;
    fixture.jev(provider);
    let judgement = judge(&mut fixture.app, &state()).await;
    assert_eq!(judgement.verdict, Verdict::Done);
    assert_eq!(judgement.by, "jev");
    let requests = requests.await.unwrap();
    let body = &requests[0].1;
    assert_eq!(body["model"], "typesafe/jev-1.14");
    assert_eq!(body["session_id"], "session-judge");
    assert_eq!(body["state"]["task"], "finish the work");
    assert_eq!(body["state"]["iteration"], 2);
    assert_eq!(body["state"]["files_changed"], "src/lib.rs | 1 +");
    assert_eq!(body["state"]["last_check_output"], "no configured check");
    assert_eq!(body["questions"].as_object().unwrap().len(), 1);
    let question = &body["questions"]["job"];
    assert_eq!(question["type"], "choice");
    for choice in ["continue", "done", "ask_user", "abort"] {
        assert!(!question["criteria"][choice].as_str().unwrap().is_empty());
    }
    let rows = fixture.app.db.decisions("session-judge").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tool, "job");
    assert_eq!(rows[0].action, "judge");
    assert_eq!(rows[0].verdict, "done");
    assert_eq!(rows[0].cost, Some(0.002));
}

#[tokio::test]
async fn judgement_controls_real_runs_and_low_confidence_continues_to_budget() {
    for (choice, confidence, expected, iters) in [
        ("done", 0.6, "done", 1),
        ("ask_user", 0.9, "parked", 1),
        ("abort", 0.9, "failed", 1),
        ("done", 0.59, "budget", 2),
        ("continue", 0.99, "budget", 2),
    ] {
        let mut fixture = Fixture::new();
        let job = fixture.job("until_done");
        let chat = fixture.chat(&vec!["working"; iters]).await;
        let (provider, requests) =
            server(200, response(choice, confidence), Duration::ZERO, iters).await;
        fixture.jev(provider);
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        assert_eq!(run.status, expected, "{}", run.summary);
        assert_eq!(usize::try_from(run.iters).unwrap(), iters);
        assert_eq!(run.summary.matches("[judge]").count(), iters);
        if matches!(choice, "ask_user" | "abort") {
            assert!(run.summary.contains("Please choose the target."));
        }
        assert_eq!(fixture.app.db.decisions(&run.id).unwrap().len(), iters);
        chat.await.unwrap();
        requests.await.unwrap();
    }
}

#[tokio::test]
async fn rules_and_network_errors_end_after_one_iteration() {
    // Without a working judge or a stop command, nothing can say the job is
    // finished, so it stops after one iteration instead of spending the
    // whole budget blind.
    for rules in [true, false] {
        let mut fixture = Fixture::new();
        let job = fixture.job("until_done");
        let chat = fixture.chat(&["working"]).await;
        let (provider, requests) = server(
            503,
            "unavailable".into(),
            Duration::ZERO,
            usize::from(!rules),
        )
        .await;
        if !rules {
            fixture.jev(provider);
        }
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        assert_eq!(run.status, "done", "{}", run.summary);
        assert_eq!(run.iters, 1);
        assert_eq!(fixture.app.db.decisions(&run.id).unwrap().len(), 1);
        chat.await.unwrap();
        requests.await.unwrap();
    }
}

#[tokio::test]
async fn no_openrouter_key_uses_the_json_llm_fallback() {
    let mut fixture = Fixture::new();
    let body = json!({"choices":[{"message":{"content":"{\"choice\":\"ask_user\",\"confidence\":0.8,\"reason\":\"Which file?\"}"}}]}).to_string();
    let (provider, requests) = server(200, body, Duration::ZERO, 1).await;
    fixture.app.backends.local = Some(provider);
    fixture.app.saved.agent.decider = Some(ModelDecider::Jev);
    let judgement = judge(&mut fixture.app, &state()).await;
    assert_eq!(judgement.by, "llm");
    assert_eq!(judgement.verdict, Verdict::AskUser);
    assert_eq!(judgement.reason, "Which file?");
    assert!(
        requests.await.unwrap()[0].1["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("only JSON")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn hard_stop_commands_override_both_soft_directions_without_querying_a_model() {
    for (command, choice, expected, iters) in [
        ("true", "continue", "done", 1),
        ("false", "done", "budget", 2),
    ] {
        let mut fixture = Fixture::new();
        let mut job = fixture.job("until_done");
        job.spec.stop_command = Some(command.into());
        let chat = fixture.chat(&vec!["working"; iters]).await;
        let (provider, requests) = server(200, response(choice, 1.0), Duration::ZERO, 0).await;
        fixture.jev(provider);
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        assert_eq!(run.status, expected, "{}", run.summary);
        assert!(fixture.app.db.decisions(&run.id).unwrap().is_empty());
        assert!(requests.await.unwrap().is_empty());
        chat.await.unwrap();
    }
}

#[tokio::test]
async fn on_change_uses_meaningfulness_not_text_noise() {
    for (probability, expected) in [(0.49, 0), (0.5, 1)] {
        let mut fixture = Fixture::new();
        let mut job = fixture.job("interval");
        job.spec.notify = "on_change".into();
        // Persist the policy before collecting notifications.
        fixture
            .app
            .db
            .conn()
            .execute(
                "UPDATE jobs SET notify = 'on_change' WHERE id = ?1",
                [&job.id],
            )
            .unwrap();
        let chat = fixture.chat(&["report"]).await;
        let (provider, requests) = server(
            200,
            json!({"answers":{"job":{"noul":probability}}}).to_string(),
            Duration::ZERO,
            1,
        )
        .await;
        fixture.jev(provider);
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        fixture.app.collect_job_notifications().unwrap();
        assert_eq!(fixture.app.notifications.len(), expected);
        assert_eq!(fixture.app.unread.contains(&run.id), expected == 1);
        let requests = requests.await.unwrap();
        assert_eq!(requests[0].1["questions"]["job"]["type"], "noul");
        assert!(requests[0].1["state"].get("previous_summary").is_some());
        assert!(
            requests[0].1["state"]["current_summary"]
                .as_str()
                .unwrap()
                .contains("report")
        );
        assert!(requests[0].1["state"].get("diff_stat").is_some());
        chat.await.unwrap();
    }
}

#[tokio::test]
async fn text_diff_fallback_ignores_run_paths_and_notifies_when_the_answer_changes() {
    let mut fixture = Fixture::new();
    let mut job = fixture.job("interval");
    job.spec.notify = "on_change".into();
    fixture
        .app
        .db
        .conn()
        .execute(
            "UPDATE jobs SET notify = 'on_change' WHERE id = ?1",
            [&job.id],
        )
        .unwrap();
    let chat = fixture
        .chat(&["same report", "same report", "new finding"])
        .await;
    for (index, expected) in [1, 1, 2].into_iter().enumerate() {
        let run = run_with_app(&mut fixture.app, &job).await.unwrap();
        assert_eq!(run.status, "done");
        fixture.app.collect_job_notifications().unwrap();
        assert_eq!(fixture.app.notifications.len(), expected, "run {index}");
    }
    chat.await.unwrap();
}

#[test]
fn legacy_watch_migration_is_guarded_idempotent_and_survives_cache_loss() {
    let fixture = Fixture::new();
    let path = fixture.root.join("legacy.db");
    let db = Db::open(&path).unwrap();
    db.conn().execute_batch("ALTER TABLE jobs DROP COLUMN kind; ALTER TABLE jobs DROP COLUMN topic;
        INSERT INTO watches VALUES ('watch-1', 'default', 'topic', 12, 'session', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');").unwrap();
    drop(db);
    for _ in 0..2 {
        let db = Db::open(&path).unwrap();
        let jobs = db.list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "watch-1");
        assert_eq!(jobs[0].spec.kind, "research");
        assert_eq!(jobs[0].spec.topic.as_deref(), Some("topic"));
        assert_eq!(jobs[0].spec.interval_minutes, 720);
        assert_eq!(jobs[0].session_id, "session");
        assert_eq!(jobs[0].last_run_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert!(jobs[0].spec.root.is_empty());
        assert_eq!(
            due(
                &jobs,
                parse_time("2026-01-02T00:00:00Z").unwrap(),
                &db.device_id().unwrap()
            )
            .len(),
            1
        );
        assert_eq!(db.list_all_watches().unwrap().len(), 1);
        db.conn()
            .execute("DELETE FROM cache.watch_jobs", [])
            .unwrap();
    }
}

#[test]
fn research_kind_and_topic_sync_and_deleted_migrations_stay_deleted() {
    let fixture = Fixture::new();
    let session = fixture
        .app
        .db
        .create_session("watch", "local:fake", "default", "research")
        .unwrap();
    let id = fixture
        .app
        .db
        .create_watch("default", "topic", 24, &session.id)
        .unwrap();
    let changes = crate::sync::build_changeset(&fixture.app.db, None, "owner").unwrap();
    let row = changes.rows.iter().find(|row| row.table == "jobs").unwrap();
    assert_eq!(row.row["kind"], "research");
    assert_eq!(row.row["topic"], "topic");
    let peer = Fixture::new();
    crate::sync::apply_changeset(&peer.app.db, &peer.app.space, &changes, None).unwrap();
    assert_eq!(peer.app.db.list_jobs().unwrap().len(), 1);
    fixture
        .app
        .db
        .conn()
        .execute("UPDATE jobs SET enabled = -1 WHERE id = ?1", [&id])
        .unwrap();
    assert!(fixture.app.db.list_jobs().unwrap().is_empty());
}

#[tokio::test]
async fn slash_watch_creates_research_job_and_direct_watch_runner_remains_compatible() {
    let mut fixture = Fixture::new();
    // A bound mock backend permits synchronous startup; abort before yielding to
    // its spawned research task, so this test never starts a provider request.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    fixture.app.backends.local =
        Some(OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap());
    fixture.app.create_watch("topic");
    let job = fixture.app.db.list_jobs().unwrap().pop().unwrap();
    assert_eq!(job.spec.kind, "research");
    assert_eq!(job.spec.topic.as_deref(), Some("topic"));
    assert_eq!(job.spec.interval_minutes, 1440);
    fixture.app.stop_research();
    let watch = fixture.app.db.list_all_watches().unwrap().pop().unwrap();
    assert!(fixture.app.run_one_watch(&watch));
    let updated = fixture.app.db.list_all_watches().unwrap().pop().unwrap();
    assert_ne!(updated.session_id, watch.session_id);
    assert_eq!(fixture.app.db.list_job_runs().unwrap().len(), 2);
    fixture.app.stop_research();
}
