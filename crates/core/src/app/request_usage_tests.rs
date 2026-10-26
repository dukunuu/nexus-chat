use super::*;
use crate::app::usage::SessionUsageTotals;
use crate::config::ModelDecider;
use crate::provider::PromptConvention;
use crate::provider::openrouter::decider_mock;
use std::time::Duration;

struct Fixture {
    app: App,
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("nexus-request-usage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root: root.clone() };
        let db = Db::open(&space.db_path()).unwrap();
        let mut app = App::new(db, None, space);
        app.current_model = Some("mock/model".into());
        let session = app
            .db
            .create_session("usage", "mock/model", &app.active_space.id, "chat")
            .unwrap();
        app.db
            .add_user_message(&session.id, "a private question")
            .unwrap();
        app.db
            .add_assistant_message(
                &session.id,
                "a private answer",
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        app.messages = app.db.load_messages(&session.id).unwrap();
        app.session = Some(session);
        Self { app, root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn current_version_dbs_gain_the_usage_purpose_columns() {
    // A db already at SCHEMA_VERSION skips the gated legacy column adds, so
    // a db created before `kind`/`tools` existed must still get them.
    let root = std::env::temp_dir().join(format!("nexus-usage-columns-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    drop(Db::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE usage_log DROP COLUMN kind; ALTER TABLE usage_log DROP COLUMN tools;
         INSERT INTO usage_log(sync_id,created_at,session_id,backend,model,prompt_tokens,completion_tokens)
         VALUES ('u1','2026-10-26T08:00:00Z','s','Codex','gpt-6-luna',10,2);",
    )
    .unwrap();
    drop(conn);
    let db = Db::open(&path).unwrap();
    let rows = db.usage_for_session("s").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].kind.is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn legacy_usage_migration_order_and_session_totals() {
    let root = std::env::temp_dir().join(format!("nexus-usage-migration-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE usage_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL,
        session_id TEXT, space_id TEXT, backend TEXT NOT NULL, model TEXT NOT NULL,
        prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
        cost REAL);
        INSERT INTO usage_log(created_at,session_id,backend,model,prompt_tokens,completion_tokens)
        VALUES ('2020-01-01T12:00:00Z','old','Codex','unknown',10,2);").unwrap();
    drop(conn);
    let db = Db::open(&path).unwrap();
    let rows = db.usage_for_session("old").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].kind.is_none());
    assert!(rows[0].tools.is_none());
    let log = crate::db::UsageLog {
        path: path.clone(),
        session_id: Some("old".into()),
        space_id: db.default_space_id().unwrap(),
    };
    let id = db
        .record_usage(
            "OpenRouter",
            "priced",
            Usage {
                prompt_tokens: 20,
                completion_tokens: 3,
                cache_read_tokens: 5,
                cost: Some(0.25),
                ..Usage::default()
            },
            &log,
            "title",
        )
        .unwrap();
    db.conn()
        .execute(
            "UPDATE usage_log SET created_at = '2019-01-01T12:00:00Z' WHERE id = ?1",
            [id],
        )
        .unwrap();
    let rows = db.usage_for_session("old").unwrap();
    assert_eq!(rows[0].kind.as_deref(), Some("title"));
    assert!(rows[1].kind.is_none());
    let total = SessionUsageTotals::from_rows(&rows);
    assert_eq!(
        (
            total.requests,
            total.prompt_tokens,
            total.cache_read_tokens,
            total.completion_tokens
        ),
        (2, 30, 5, 5)
    );
    assert_eq!(total.cost, Some(0.25));
    assert_eq!(total.unknown_cost_requests, 1);
    assert!(db.usage_for_session("other").unwrap().is_empty());
    drop(db);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.usage_for_session("old").unwrap().len(), 2);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_roundtrips_usage_metadata_and_convention_without_content() {
    let fixture = Fixture::new();
    let app = &fixture.app;
    let log = app.usage_log().unwrap();
    let id = app
        .db
        .record_usage(
            "OpenRouter",
            "mock/model",
            Usage {
                prompt_tokens: 50,
                completion_tokens: 4,
                cache_read_tokens: 20,
                prompt_convention: PromptConvention::Inclusive,
                cost: Some(0.1),
                ..Usage::default()
            },
            &log,
            "chat",
        )
        .unwrap();
    app.db.add_usage_tool(id, "fs").unwrap();
    app.db.add_usage_tool(id, "search").unwrap();
    let changes = crate::sync::build_changeset(&app.db, None, "source").unwrap();
    let row = changes
        .rows
        .iter()
        .find(|r| r.table == "usage_log")
        .unwrap();
    assert_eq!(row.row["kind"], "chat");
    assert_eq!(row.row["tools"], "fs,search");
    assert_eq!(row.row["prompt_convention"], "inclusive");
    assert!(!row.row.to_string().contains("private"));
    let target = Db::open_in_memory().unwrap();
    crate::sync::apply_changeset(&target, &app.space, &changes, None).unwrap();
    crate::sync::apply_changeset(&target, &app.space, &changes, None).unwrap();
    let rows = target
        .usage_for_session(log.session_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind.as_deref(), Some("chat"));
    assert_eq!(rows[0].tools.as_deref(), Some("fs,search"));
    assert_eq!(rows[0].usage.prompt_convention, PromptConvention::Inclusive);
}

#[tokio::test]
async fn background_calls_log_their_kind_and_captured_session_except_incognito() {
    for incognito in [false, true] {
        for kind in ["title", "compaction", "memory", "judge", "research:topic"] {
            let mut fixture = Fixture::new();
            let app = &mut fixture.app;
            let session = app.session.as_ref().unwrap().id.clone();
            let text = r#"{"topic":"A title","id":"a-title","choice":"done","confidence":0.9,"reason":"ok"}"#;
            let body = serde_json::json!({
                "choices":[{"message":{"content":text}}],
                "usage":{"prompt_tokens":100,"completion_tokens":10,"cost":0.012}
            });
            let (provider, server) =
                decider_mock::server(200, body.to_string(), Duration::ZERO, 1).await;
            app.backends.openrouter = Some(provider);
            app.incognito = incognito;
            match kind {
                "title" => {
                    app.maybe_generate_title();
                    let result = tokio::time::timeout(
                        Duration::from_secs(5),
                        app.title_rx.as_mut().unwrap().recv(),
                    )
                    .await
                    .unwrap();
                    assert!(result.is_some());
                }
                "compaction" => {
                    app.force_compact();
                    let result = tokio::time::timeout(
                        Duration::from_secs(5),
                        app.compact_rx.as_mut().unwrap().recv(),
                    )
                    .await
                    .unwrap();
                    assert!(result.is_some());
                }
                "memory" => {
                    app.memory_extraction = true;
                    app.maybe_extract_memory();
                    let result = tokio::time::timeout(
                        Duration::from_secs(5),
                        app.memory_rx.as_mut().unwrap().recv(),
                    )
                    .await
                    .unwrap();
                    assert!(result.is_some());
                }
                "judge" => {
                    app.saved.agent.decider = Some(ModelDecider::Llm);
                    let result = crate::jobs::judge(
                        app,
                        &crate::jobs::JudgeState {
                            session_id: session.clone(),
                            task: "task".into(),
                            iteration: 1,
                            last_answer: "answer".into(),
                            files_changed: String::new(),
                            last_check_output: String::new(),
                        },
                    )
                    .await;
                    assert_eq!(result.by, "llm");
                }
                "research:topic" => {
                    app.start_research_from_chat();
                    let result = tokio::time::timeout(
                        Duration::from_secs(5),
                        app.research_topic_rx.as_mut().unwrap().recv(),
                    )
                    .await
                    .unwrap();
                    assert!(result.unwrap().is_ok());
                }
                _ => unreachable!(),
            }
            server.await.unwrap();
            let rows = app.db.usage_for_session(&session).unwrap();
            assert_eq!(
                rows.len(),
                usize::from(!incognito),
                "{kind}, incognito={incognito}"
            );
            if !incognito {
                assert_eq!(rows[0].kind.as_deref(), Some(kind));
                assert!(rows[0].tools.is_none());
                assert_eq!(rows[0].usage.prompt_tokens, 100);
                assert_eq!(rows[0].usage.cost, Some(0.012));
                let (space, exact): (String, bool) = app
                    .db
                    .conn()
                    .query_row(
                        "SELECT space_id, cost_is_provider FROM usage_log",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(space, app.active_space.id);
                assert!(exact);
            }
            assert_eq!(
                app.db.usage_totals(None).unwrap().requests,
                u64::from(!incognito)
            );
        }
    }
}

#[tokio::test]
async fn chat_usage_records_only_called_tool_names_and_merges_cost_event() {
    for incognito in [false, true] {
        let mut fixture = Fixture::new();
        let app = &mut fixture.app;
        let session = app.session.as_ref().unwrap().id.clone();
        app.incognito = incognito;
        app.chat_tasks.insert(
            1,
            ChatTask {
                id: 1,
                session_id: session.clone(),
                session_title: "usage".into(),
                space_id: app.active_space.id.clone(),
                model: "mock/model".into(),
                model_id: "mock/model".into(),
                backend: BackendTag::OpenRouter,
                incognito,
                buffer: String::new(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                usage_row_id: None,
                started: std::time::Instant::now(),
                thinking_idx: 0,
                spinner_color: SpinnerColor::Green,
                abort: tokio::spawn(async {}).abort_handle(),
            },
        );
        app.on_chat_event(
            1,
            StreamEvent::Usage(Usage {
                prompt_tokens: 100,
                completion_tokens: 5,
                ..Usage::default()
            }),
        )
        .unwrap();
        for name in ["fs", "search"] {
            app.on_chat_event(
                1,
                StreamEvent::ToolCall {
                    id: name.into(),
                    name: name.into(),
                    arguments: "SECRET_ARGUMENT".into(),
                    result: "SECRET_RESULT".into(),
                    reasoning: None,
                    assistant_content: None,
                },
            )
            .unwrap();
        }
        app.on_chat_event(
            1,
            StreamEvent::Usage(Usage {
                cost: Some(0.02),
                ..Usage::default()
            }),
        )
        .unwrap();
        app.on_chat_event(
            1,
            StreamEvent::Usage(Usage {
                prompt_tokens: 150,
                completion_tokens: 10,
                ..Usage::default()
            }),
        )
        .unwrap();
        let rows = app.db.usage_for_session(&session).unwrap();
        if incognito {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].kind.as_deref(), Some("chat"));
            assert_eq!(rows[0].tools.as_deref(), Some("fs,search"));
            assert_eq!(rows[0].usage.cost, Some(0.02));
            assert!(rows[1].tools.is_none());
            assert!(!serde_json::to_string(&rows).unwrap().contains("SECRET"));
        }
    }
}

async fn workflow_server(
    replies: Vec<(&'static str, bool)>,
) -> (OpenRouter, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let provider =
        OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        for (text, streaming) in replies {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(socket.read_line(&mut line).await.unwrap() > 0);
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
            socket.read_exact(&mut body).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["stream"], streaming);
            let usage = serde_json::json!({"prompt_tokens":100,"completion_tokens":10,"cost":0.01});
            let (mime, body) = if streaming {
                let value = serde_json::json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}],"usage":usage});
                (
                    "text/event-stream",
                    format!("data: {value}\n\ndata: [DONE]\n\n"),
                )
            } else {
                (
                    "application/json",
                    serde_json::json!({"choices":[{"message":{"content":text}}],"usage":usage})
                        .to_string(),
                )
            };
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .get_mut()
                .write_all(headers.as_bytes())
                .await
                .unwrap();
            socket.get_mut().write_all(body.as_bytes()).await.unwrap();
        }
    });
    (provider, task)
}

#[tokio::test]
async fn swarm_personas_roster_moderation_and_synthesis_each_log_a_request() {
    for incognito in [false, true] {
        let mut fixture = Fixture::new();
        let app = &mut fixture.app;
        let session = app.session.as_ref().unwrap().id.clone();
        let (provider, server) = workflow_server(vec![
            (r#"[{"name":"Analyst","blurb":"check evidence"}]"#, false),
            ("Persona response", true),
            ("CONVERGED", false),
            ("Final synthesis", false),
        ])
        .await;
        app.backends.openrouter = Some(provider);
        app.incognito = incognito;
        app.start_swarm_turn();
        app.session = None;
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.swarm_rx.as_mut().unwrap().recv().await.is_some() {}
        })
        .await
        .unwrap();
        server.await.unwrap();
        let rows = app.db.usage_for_session(&session).unwrap();
        assert_eq!(rows.len(), if incognito { 0 } else { 4 });
        for row in rows {
            assert_eq!(row.kind.as_deref(), Some("swarm"));
            assert_eq!(row.usage.cost, Some(0.01));
            assert!(row.tools.is_none());
        }
    }
}

#[tokio::test]
async fn research_pipeline_logs_every_stage_and_respects_incognito() {
    for incognito in [false, true] {
        let fixture = Fixture::new();
        let app = &fixture.app;
        let session = app.session.as_ref().unwrap().id.clone();
        let (provider, server) = workflow_server(vec![
            ("Landscape findings", true),
            ("1. What evidence is available?", false),
            ("Searcher findings", true),
            ("Draft", false),
            ("SATISFIED", false),
            ("Verified draft", true),
            ("Final report", false),
        ])
        .await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_steer_tx, steer_rx) = mpsc::unbounded_channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            research::run_research(research::ResearchOptions {
                research_provider: provider.clone(),
                research_model: "mock/model".into(),
                embedding_provider: provider,
                embedding_model: String::new(),
                db_path: app.space.db_path(),
                incognito,
                topic: "Evidence".into(),
                reply_rx: None,
                steer_rx,
                toolbox: app.toolbox.clone(),
                tx,
                session_id: session.clone(),
                prompt_cache_key: "research-test".into(),
                cache_ttl_1h: false,
                space_id: app.active_space.id.clone(),
                space_name: app.active_space.name.clone(),
            }),
        )
        .await
        .unwrap();
        server.await.unwrap();
        let rows = app.db.usage_for_session(&session).unwrap();
        let kinds: Vec<_> = rows.iter().map(|r| r.kind.as_deref().unwrap()).collect();
        let expected = if incognito {
            vec![]
        } else {
            vec![
                "research:web survey",
                "research:planner",
                "research:searcher",
                "research:synthesizer",
                "research:critic",
                "research:verifier",
                "research:writer",
            ]
        };
        assert_eq!(kinds, expected);
        assert!(
            rows.iter()
                .all(|r| r.usage.cost == Some(0.01) && r.tools.is_none())
        );
    }
}

#[tokio::test]
async fn vision_and_embedding_requests_retain_usage_but_never_content() {
    for incognito in [false, true] {
        for kind in ["describe image", "ocr", "embed", "generate image"] {
            let mut fixture = Fixture::new();
            let app = &mut fixture.app;
            app.incognito = incognito;
            let body = serde_json::json!({
                "choices":[{"message":{"content":"PRIVATE_RESPONSE"}}],
                "data":[{"index":0,"embedding":[0.1,0.2],"b64_json":"aGVsbG8=","media_type":"image/png"}],
                "usage":{"prompt_tokens":25,"completion_tokens":5,"cost":0.001}
            });
            let (provider, server) =
                decider_mock::server(200, body.to_string(), Duration::ZERO, 1).await;
            let provider = app.usage_provider(provider, "helper");
            match kind {
                "describe image" => {
                    provider
                        .describe_image("vision", "PRIVATE_IMAGE")
                        .await
                        .unwrap();
                }
                "ocr" => {
                    provider.ocr_page("vision", "PRIVATE_IMAGE").await.unwrap();
                }
                "embed" => {
                    provider
                        .embed("embedding", vec!["PRIVATE_INPUT".into()])
                        .await
                        .unwrap();
                }
                "generate image" => {
                    provider
                        .generate_image("image", "PRIVATE_INPUT", "1024x1024", None)
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            server.await.unwrap();
            let rows = app
                .db
                .usage_for_session(&app.session.as_ref().unwrap().id)
                .unwrap();
            assert_eq!(rows.len(), usize::from(!incognito));
            if let Some(row) = rows.first() {
                assert_eq!(row.kind.as_deref(), Some(kind));
                assert_eq!(row.usage.prompt_tokens, 25);
                assert_eq!(row.usage.cost, Some(0.001));
                assert!(!serde_json::to_string(row).unwrap().contains("PRIVATE"));
            }
        }
    }
}

#[tokio::test]
async fn local_ollama_embeddings_do_not_write_usage_rows() {
    use crate::provider::embedding::{EmbedInput, Embedder, tests::MockOllama};
    let fixture = Fixture::new();
    let app = &fixture.app;
    let server = MockOllama::start(false).await;
    let embedder = Embedder::ollama(&server.endpoint, "embeddinggemma-2:270m".into())
        .unwrap()
        .logged(app.usage_log());
    embedder
        .embed(vec![EmbedInput::Text("private local text".into())])
        .await
        .unwrap();
    assert_eq!(app.db.usage_totals(None).unwrap().requests, 0);
}

#[test]
fn chat_subagents_leave_logging_to_the_chat_task() {
    // The chat task logs every usage event on the turn's channel, including
    // the ones sub-agents forward, so a logged sub-agent client would count
    // each sub-agent request twice.
    let fixture = Fixture::new();
    let app = &fixture.app;
    let provider = crate::provider::openrouter::OpenRouter::from_key_auto("sk-or-test".into());
    assert_eq!(
        app.usage_provider(provider.clone(), "chat")
            .subagent_client()
            .usage_label(),
        None
    );
    assert_eq!(
        app.usage_provider(provider, "swarm")
            .subagent_client()
            .usage_label(),
        Some("subagent")
    );
}

#[tokio::test]
async fn subagent_rounds_use_the_inherited_session_usage_sink() {
    let fixture = Fixture::new();
    let app = &fixture.app;
    let (provider, server) = workflow_server(vec![("Subagent answer", true)]).await;
    let provider = app.usage_provider(provider, "chat").usage_kind("subagent");
    let answer = provider
        .subagent_round(crate::subagent::Request {
            model: "mock/model",
            messages: &[crate::provider::ChatMessage::text("user", "PRIVATE_TASK")],
            params: &crate::provider::ChatParams::default(),
            tools: &[],
            final_round: true,
        })
        .await
        .unwrap();
    assert_eq!(answer.content, "Subagent answer");
    server.await.unwrap();
    let rows = app
        .db
        .usage_for_session(&app.session.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind.as_deref(), Some("subagent"));
    assert_eq!(rows[0].usage.cost, Some(0.01));
    assert!(!serde_json::to_string(&rows).unwrap().contains("PRIVATE"));
}

#[test]
fn shared_usage_sink_estimates_cache_aware_costs_without_claiming_provider_provenance() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.db
        .upsert_model_prices(&[(
            "mock/model".into(),
            "OpenRouter".into(),
            crate::provider::ModelPricing {
                prompt: 2.0,
                completion: 4.0,
                cache_read: Some(0.2),
                cache_write: Some(3.0),
            },
        )])
        .unwrap();
    app.usage_log().unwrap().record(
        "OpenRouter",
        "mock/model",
        Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            cache_read_tokens: 60,
            cache_creation_tokens: 10,
            ..Usage::default()
        },
        "compaction",
    );
    let rows = app
        .db
        .usage_for_session(&app.session.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!((rows[0].usage.cost.unwrap() - 0.000182).abs() < 1e-12);
    let exact: bool = app
        .db
        .conn()
        .query_row("SELECT cost_is_provider FROM usage_log", [], |r| r.get(0))
        .unwrap();
    assert!(!exact);
}

#[tokio::test]
async fn both_model_deciders_log_the_live_session_without_persisting_state() {
    use crate::decider::{
        Class, Decider as _, DecisionRequest, JevDecider, LlmDecider, ModelContext,
    };
    for incognito in [false, true] {
        for jev in [false, true] {
            let mut fixture = Fixture::new();
            let app = &mut fixture.app;
            let session = app.session.take().unwrap().id;
            app.incognito = incognito;
            let body = serde_json::json!({
                "choices":[{"message":{"content":r#"{"calls":[{"id":"c0","in_scope":0.99,"risk":0.0}]}"#}}],
                "answers":{"c0_scope":{"noul":0.99},"c0_risk":{"score":0.0,"confidence":0.99}},
                "usage":{"prompt_tokens":20,"completion_tokens":5,"cost":0.001}
            });
            let (provider, server) =
                decider_mock::server(200, body.to_string(), Duration::ZERO, 1).await;
            let provider = app.usage_provider(provider, "decider");
            let context = ModelContext {
                session: Arc::new(std::sync::Mutex::new((session.clone(), String::new()))),
                recent_steps: Arc::default(),
                scrubber: crate::trace::Scrubber::new(Vec::new()),
                sandbox_available: true,
                config: app.saved.agent.clone(),
                availability: app.decider_availability.clone(),
            };
            let request = DecisionRequest {
                tool: "fs".into(),
                action: "write".into(),
                summary: "PRIVATE_SUMMARY".into(),
                task: "PRIVATE_TASK".into(),
                root: fixture.root.clone(),
                effect: crate::tools::Effect::default(),
                class: Class::WriteInRoot,
            };
            let answers = if jev {
                JevDecider {
                    provider: Some(provider),
                    context,
                }
                .decide(&[request])
                .await
            } else {
                LlmDecider {
                    backend: Some((provider, "mock/model".into())),
                    context,
                }
                .decide(&[request])
                .await
            };
            assert_eq!(answers[0].by, if jev { "jev" } else { "llm" });
            server.await.unwrap();
            let rows = app.db.usage_for_session(&session).unwrap();
            assert_eq!(rows.len(), usize::from(!incognito));
            if let Some(row) = rows.first() {
                assert_eq!(row.kind.as_deref(), Some("decider"));
                assert_eq!(row.usage.cost, Some(0.001));
                assert!(!serde_json::to_string(row).unwrap().contains("PRIVATE"));
            }
            assert!(app.db.usage_for_session("").unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn new_session_rebinds_tool_requests_before_the_first_turn() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.session = None;
    app.messages.clear();
    let (provider, server) = decider_mock::server(200, String::new(), Duration::ZERO, 0).await;
    app.backends.openrouter = Some(provider);
    app.send_message("a new question".into()).unwrap();
    app.stop_stream().unwrap();
    let session = app.session.as_ref().unwrap().id.clone();
    let _ = app.toolbox.run("skills", r#"{"action":"list"}"#).await;
    let recorded: String = app
        .db
        .conn()
        .query_row(
            "SELECT session_id FROM cache.tool_events LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, session);
    server.await.unwrap();
}

#[test]
fn tools_bucket_estimates_the_exact_next_request_definitions() {
    let mut fixture = Fixture::new();
    for model in ["mock/model", "local:small"] {
        fixture.app.current_model = Some(model.into());
        let defs = fixture.app.chat_tool_defs();
        assert!(!defs.is_empty());
        let expected =
            u64::try_from(serde_json::to_string(&defs).unwrap().chars().count() / 4).unwrap();
        assert_eq!(fixture.app.context_breakdown().tools_tokens, expected);
    }
}
