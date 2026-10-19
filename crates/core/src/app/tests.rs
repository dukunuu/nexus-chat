use super::*;

fn approval_test_app() -> (App, std::path::PathBuf) {
    let mut app = app_with_key();
    let workspace = test_space().root;
    std::fs::create_dir_all(&workspace).unwrap();
    app.interactive_approvals = true;
    app.run_command(&format!("agent {}", workspace.display()))
        .unwrap();
    while app.pop_pending_event().is_some() {}
    (app, workspace)
}

#[tokio::test]
async fn installed_approver_round_trip_writes_only_after_the_approval_command() {
    let (mut app, workspace) = approval_test_app();
    let root = app.space.root.clone();
    let path = workspace.join("approved");
    let toolbox = Arc::clone(&app.toolbox);
    let call = tokio::spawn(async move {
        toolbox
            .run(
                "fs",
                r#"{"action":"write","path":"approved","content":"allowed"}"#,
            )
            .await
    });
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
        .await
        .unwrap();
    assert!(matches!(event, AppEvent::Status(status) if status == "approval needed · fs write"));
    assert_eq!(
        app.pending_approval_request.as_ref().unwrap(),
        &ApprovalRequest {
            tool: "fs".into(),
            action: "write".into(),
            summary: "write approved".into(),
        }
    );
    assert!(app.pending_approval.is_some());
    assert!(!path.exists());
    app.execute(AppCommand::Approve {
        decision: Decision::Allow,
    })
    .unwrap();
    assert!(app.pending_approval.is_none());
    assert!(app.pending_approval_request.is_none());
    let (result, status) = call.await.unwrap();
    assert!(result.starts_with("wrote approved"), "{result}");
    assert_ne!(status, "blocked");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "allowed");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn installed_session_grants_reset_on_refresh_and_mode_switches() {
    let (mut app, workspace) = approval_test_app();
    let root = app.space.root.clone();
    for reset_mode in [false, true] {
        let toolbox = Arc::clone(&app.toolbox);
        let call = tokio::spawn(async move {
            toolbox
                .run(
                    "fs",
                    r#"{"action":"write","path":"first","content":"allowed"}"#,
                )
                .await
        });
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
            .await
            .unwrap();
        assert!(
            matches!(event, AppEvent::Status(status) if status == "approval needed · fs write")
        );
        app.execute(AppCommand::Approve {
            decision: Decision::AllowForSession,
        })
        .unwrap();
        assert_ne!(call.await.unwrap().1, "blocked");
        let (result, status) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.toolbox.run(
                "fs",
                r#"{"action":"write","path":"second","content":"allowed"}"#,
            ),
        )
        .await
        .unwrap();
        assert!(result.starts_with("wrote second"), "{result}");
        assert_ne!(status, "blocked");
        let toolbox = Arc::clone(&app.toolbox);
        let delete = tokio::spawn(async move {
            toolbox
                .run("fs", r#"{"action":"delete","path":"first"}"#)
                .await
        });
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
            .await
            .unwrap();
        assert!(
            matches!(event, AppEvent::Status(status) if status == "approval needed · fs delete")
        );
        app.execute(AppCommand::Approve {
            decision: Decision::Deny,
        })
        .unwrap();
        assert_eq!(delete.await.unwrap().1, "blocked");
        assert!(workspace.join("first").exists());
        if reset_mode {
            app.run_command("chat").unwrap();
            assert!(app.approval_rx.is_some());
            app.run_command(&format!("agent {}", workspace.display()))
                .unwrap();
            while app.pop_pending_event().is_some() {}
            let toolbox = Arc::clone(&app.toolbox);
            let regrant = tokio::spawn(async move {
                toolbox
                    .run(
                        "fs",
                        r#"{"action":"write","path":"regrant","content":"denied"}"#,
                    )
                    .await
            });
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
                .await
                .unwrap();
            assert!(
                matches!(event, AppEvent::Status(status) if status == "approval needed · fs write"),
                "a grant made before /chat must not carry over"
            );
            app.execute(AppCommand::Approve {
                decision: Decision::Deny,
            })
            .unwrap();
            assert_eq!(regrant.await.unwrap().1, "blocked");
            assert!(!workspace.join("regrant").exists());
        } else {
            app.refresh_toolbox();
        }
        while app.pop_pending_event().is_some() {}
    }
    let toolbox = Arc::clone(&app.toolbox);
    let call = tokio::spawn(async move {
        toolbox
            .run(
                "fs",
                r#"{"action":"write","path":"third","content":"denied"}"#,
            )
            .await
    });
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
        .await
        .unwrap();
    assert!(matches!(event, AppEvent::Status(status) if status == "approval needed · fs write"));
    app.execute(AppCommand::Approve {
        decision: Decision::Deny,
    })
    .unwrap();
    assert_eq!(call.await.unwrap().1, "blocked");
    assert!(!workspace.join("third").exists());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn dropped_tool_call_clears_the_pending_approval_in_next_event() {
    let (mut app, workspace) = approval_test_app();
    let root = app.space.root.clone();
    let toolbox = Arc::clone(&app.toolbox);
    let call = tokio::spawn(async move {
        toolbox
            .run(
                "fs",
                r#"{"action":"write","path":"cancelled","content":"no"}"#,
            )
            .await
    });
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
        .await
        .unwrap();
    assert!(matches!(event, AppEvent::Status(status) if status == "approval needed · fs write"));
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
        .await
        .unwrap();
    assert!(matches!(event, AppEvent::Status(status) if status == "approval cancelled"));
    assert!(app.pending_approval.is_none());
    assert!(app.pending_approval_request.is_none());
    assert!(!workspace.join("cancelled").exists());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn finishing_or_cancelling_chat_clears_its_pending_approval() {
    for cancel in [false, true] {
        let (mut app, workspace) = approval_test_app();
        let root = app.space.root.clone();
        let session = app
            .db
            .create_session("approval", "a/one", &app.active_space.id, "chat")
            .unwrap();
        app.session = Some(session.clone());
        let toolbox = Arc::clone(&app.toolbox);
        let call = tokio::spawn(async move {
            toolbox
                .run(
                    "fs",
                    r#"{"action":"write","path":"cancelled","content":"no"}"#,
                )
                .await
        });
        app.chat_tasks.insert(
            1,
            ChatTask {
                id: 1,
                session_id: session.id,
                session_title: session.title,
                space_id: app.active_space.id.clone(),
                model: "a/one".into(),
                model_id: "a/one".into(),
                backend: BackendTag::OpenRouter,
                incognito: false,
                buffer: String::new(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                usage_row_id: None,
                started: std::time::Instant::now(),
                thinking_idx: 0,
                spinner_color: SpinnerColor::Green,
                abort: call.abort_handle(),
            },
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
            .await
            .unwrap();
        assert!(
            matches!(event, AppEvent::Status(status) if status == "approval needed · fs write")
        );
        if cancel {
            app.cancel_chat_task(1).unwrap();
            assert!(call.await.unwrap_err().is_cancelled());
        } else {
            app.on_chat_event(1, StreamEvent::Done).unwrap();
            assert_eq!(call.await.unwrap().1, "blocked");
        }
        assert!(app.pending_approval.is_none());
        assert!(app.pending_approval_request.is_none());
        assert!(!workspace.join("cancelled").exists());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(workspace).unwrap();
    }
}

#[tokio::test]
async fn channel_approver_remembers_only_the_requested_action_for_its_session() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let approver = ChannelApprover::new(sender);
    let request = ApprovalRequest {
        tool: "fs".into(),
        action: "edit".into(),
        summary: "edit file".into(),
    };
    let mut calls = 0;
    let (decision, ()) = tokio::join!(approver.request(request.clone()), async {
        let (shown, reply) = receiver.recv().await.unwrap();
        calls += 1;
        assert_eq!(shown, request);
        reply.send(Decision::AllowForSession).unwrap();
    });
    assert_eq!(decision, Decision::AllowForSession);
    assert_eq!(approver.request(request.clone()).await, Decision::Allow);
    assert!(receiver.try_recv().is_err());
    let different = ApprovalRequest {
        action: "delete".into(),
        ..request.clone()
    };
    let (decision, ()) = tokio::join!(approver.request(different.clone()), async {
        let (shown, reply) = receiver.recv().await.unwrap();
        calls += 1;
        assert_eq!(shown, different);
        reply.send(Decision::Deny).unwrap();
    });
    assert_eq!(decision, Decision::Deny);
    assert_eq!(calls, 2);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let fresh = ChannelApprover::new(sender);
    let (decision, ()) = tokio::join!(fresh.request(request.clone()), async {
        let (shown, reply) = receiver.recv().await.unwrap();
        assert_eq!(shown, request);
        reply.send(Decision::Deny).unwrap();
    });
    assert_eq!(decision, Decision::Deny);
}

#[tokio::test]
async fn approval_command_answers_a_pending_channel_request() {
    let mut app = app_with_key();
    let root = app.space.root.clone();
    assert!(!app.interactive_approvals);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let approver = ChannelApprover::new(sender);
    let request = ApprovalRequest {
        tool: "shell".into(),
        action: "run".into(),
        summary: "$ true".into(),
    };
    for answer in [Decision::Allow, Decision::Allow, Decision::Deny] {
        let (decision, ()) = tokio::join!(approver.request(request.clone()), async {
            let (shown, reply) = receiver.recv().await.unwrap();
            assert_eq!(shown, request);
            app.pending_approval = Some(reply);
            app.execute(AppCommand::Approve { decision: answer })
                .unwrap();
            assert!(app.pending_approval.is_none());
        });
        assert_eq!(decision, answer);
    }
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn channel_approver_denies_closed_channels_and_cancels_with_the_caller() {
    let request = ApprovalRequest {
        tool: "fs".into(),
        action: "write".into(),
        summary: "write file".into(),
    };
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let approver = Arc::new(ChannelApprover::new(sender));
    let caller = tokio::spawn({
        let approver = Arc::clone(&approver);
        let request = request.clone();
        async move { approver.request(request).await }
    });
    let (_, mut reply) = receiver.recv().await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    reply.closed().await;
    assert!(reply.is_closed());
    drop(receiver);
    assert_eq!(approver.request(request).await, Decision::Deny);
}

#[test]
fn model_profile_caps_local_tool_replay_without_changing_hosted_or_stored_results() {
    let result = format!("HEAD:{}:TAIL", "界".repeat(7_990));
    for backend in [BackendTag::OpenRouter, BackendTag::Local] {
        let mut app = app_with_key();
        let root = app.space.root.clone();
        app.models[0].backend = backend;
        app.models[0].context_length = Some(8_000);
        let model = composite_id(&app.models[0]);
        app.current_model = Some(model.clone());
        app.saved.local = Some(toml::from_str("provider = 'ollama'").unwrap());
        let session = app
            .db
            .create_session("profile", &model, &app.active_space.id, "chat")
            .unwrap();
        for id in ["first", "duplicate"] {
            app.db
                .add_tool_call_message(
                    &session.id,
                    &serde_json::json!({
                        "id": id, "name": "test", "arguments": "{}", "result": result,
                    })
                    .to_string(),
                )
                .unwrap();
        }
        app.messages = app.db.load_messages(&session.id).unwrap();
        app.session = Some(session.clone());
        let history = app.build_history();
        let tools: Vec<_> = history
            .iter()
            .filter(|message| message.role == "tool")
            .collect();
        assert_eq!(tools.len(), 2);
        if backend == BackendTag::OpenRouter {
            assert_eq!(tools[0].content, result);
        } else {
            assert!(tools[0].content.starts_with("HEAD:"));
            assert!(tools[0].content.ends_with(":TAIL"));
            assert!(tools[0].content.contains(" characters omitted] …\n"));
            assert!(tools[0].content.chars().count() <= 4_000);
            assert_eq!(
                tools[0].content,
                crate::tools::cap_head_tail(&result, 4_000)
            );
        }
        assert_eq!(
            tools[1].content,
            crate::tools::tool_result_unchanged_note("test", "{}")
        );
        let seeded = crate::provider::seed_tool_result_dedup(&history);
        assert_eq!(seeded[&("test".into(), "{}".into())], tools[0].content);
        for message in app.db.load_messages(&session.id).unwrap() {
            let row: serde_json::Value = serde_json::from_str(&message.content).unwrap();
            assert_eq!(row["result"], result);
        }
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A throwaway space dir under the OS temp dir, unique per test.
fn test_space() -> Space {
    Space {
        root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())),
    }
}

#[test]
fn training_is_off_by_default_and_never_in_incognito() {
    let mut app = App::new(Db::open_in_memory().unwrap(), None, test_space());
    let root = app.space.root.clone();
    let key = format!("training:{}", app.active_space.id);
    assert!(!app.training_enabled());
    app.db.set_setting(&key, "on").unwrap();
    assert!(app.training_enabled());
    app.incognito = true;
    assert!(!app.training_enabled());
    app.run_command("training off").unwrap();
    assert!(app.last_status().contains("incognito"));
    assert!(
        app.db
            .load_settings()
            .unwrap()
            .contains(&(key, "on".into()))
    );
    app.incognito = false;
    assert!(app.training_enabled());
    app.active_space = app.db.create_space("other").unwrap();
    assert!(!app.training_enabled());
    assert!(!app.space.traces_dir().exists());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn training_command_toggles_and_persists_locally() {
    let space = test_space();
    std::fs::create_dir_all(&space.root).unwrap();
    let root = space.root.clone();
    let db_path = space.db_path();
    let mut app = App::new(Db::open(&db_path).unwrap(), None, space);
    assert_eq!(
        app.parse_command("training").unwrap(),
        AppCommand::SetTraining { on: None }
    );
    assert_eq!(
        app.parse_command("training on").unwrap(),
        AppCommand::SetTraining { on: Some(true) }
    );
    assert_eq!(
        app.parse_command("training off").unwrap(),
        AppCommand::SetTraining { on: Some(false) }
    );
    assert!(app.parse_command("training invalid").is_err());
    app.run_command("training").unwrap();
    assert!(app.training_enabled());
    let status = app.last_status();
    assert!(status.contains(&app.space.traces_dir().display().to_string()));
    assert!(status.contains("secrets are scrubbed"));
    let key = format!("training:{}", app.active_space.id);
    assert!(Db::setting_is_local(&key));
    let scope: String = app
        .db
        .conn_for_test()
        .query_row(
            "SELECT scope FROM app_settings WHERE key = ?1",
            [&key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(scope, "local");
    assert!(!app.space.traces_dir().exists());
    drop(app);
    let mut app = App::new(
        Db::open(&db_path).unwrap(),
        None,
        Space { root: root.clone() },
    );
    assert!(app.training_enabled());
    app.run_command("training").unwrap();
    assert!(!app.training_enabled());
    assert!(
        app.db
            .load_settings()
            .unwrap()
            .contains(&(key, "off".into()))
    );
    app.run_command("training on").unwrap();
    assert!(app.training_enabled());
    app.run_command("training off").unwrap();
    assert!(!app.training_enabled());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn start_stream_sets_trace_context_only_when_training_is_on() {
    let mut app = app_with_key();
    let root = app.space.root.clone();
    let session = app
        .db
        .create_session("chat", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session.clone());
    app.settings.temperature = Some(0.5);
    let params = app.chat_params(Some("low".into()), BackendTag::OpenRouter);
    assert!(params.trace.is_none());
    assert!(app.trace_store.is_none());
    assert!(app.last_turn.is_none());
    app.run_command("training on").unwrap();
    let params = app.chat_params(Some("low".into()), BackendTag::OpenRouter);
    assert_eq!(params.temperature, Some(0.5));
    assert_eq!(params.reasoning_effort.as_deref(), Some("low"));
    assert!(params.prompt_cache_key.is_some());
    let context = params.trace.unwrap();
    assert_eq!(context.session_id, session.id);
    assert_eq!(context.space_id, app.active_space.id);
    assert_eq!(context.backend, "OpenRouter");
    assert_eq!(
        uuid::Uuid::parse_str(&context.turn_id)
            .unwrap()
            .get_version_num(),
        4
    );
    assert_eq!(app.last_turn, Some((session.id, context.turn_id.clone())));
    assert!(std::sync::Arc::ptr_eq(
        &context.store,
        app.trace_store.as_ref().unwrap()
    ));
    let next = app.chat_params(None, BackendTag::Local).trace.unwrap();
    assert_ne!(context.turn_id, next.turn_id);
    assert_eq!(next.backend, "Local");
    assert!(std::sync::Arc::ptr_eq(&context.store, &next.store));
    app.incognito = true;
    assert!(
        app.chat_params(None, BackendTag::OpenRouter)
            .trace
            .is_none()
    );
    app.incognito = false;
    app.run_command("training off").unwrap();
    assert!(
        app.chat_params(None, BackendTag::OpenRouter)
            .trace
            .is_none()
    );
    drop(next);
    drop(context);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn local_backend_discovers_models_and_uses_local_utility_fallback() {
    let mut app = App::new(Db::open_in_memory().unwrap(), None, test_space());
    app.saved.local = Some(crate::provider::local::LocalConfig {
        provider: crate::provider::local::LocalRuntime::Mlx,
        endpoint: None,
        list_command: Some(vec![
            "/usr/bin/printf".into(),
            "mlx-community/test-model\\n".into(),
        ]),
        memory_budget_mb: None,
        context_length: None,
        kv_cache_q8: false,
        parallel_tool_calls: None,
        tier: None,
    });
    app.rebuild_all_backends();
    assert!(app.backends.configured(BackendTag::Local));
    assert!(!app.backends.configured(BackendTag::OpenAi));
    app.fetch_models();
    let result = app.models_rx.as_mut().unwrap().recv().await;
    app.on_models_result(result);
    assert_eq!(app.models.len(), 1);
    assert_eq!(
        composite_id(&app.models[0]),
        "local:mlx-community/test-model"
    );
    app.current_model = Some(composite_id(&app.models[0]));
    let (provider, model) = app.resolve_utility_model_backend("gpt-4.1-mini").unwrap();
    assert_eq!(provider.backend_tag(), BackendTag::Local);
    assert_eq!(model, "mlx-community/test-model");
}

#[test]
fn bare_local_command_reports_the_current_runtime() {
    // The status path is the headless half of `/local`; applying a spec
    // writes the user's real config file, so it stays out of the suite.
    let mut app = App::new(Db::open_in_memory().unwrap(), None, test_space());
    let last_status = |app: &mut App| {
        app.pending_events
            .drain(..)
            .filter_map(|event| match event {
                AppEvent::Status(s) => Some(s),
                _ => None,
            })
            .next_back()
            .unwrap()
    };
    app.execute(AppCommand::ConfigureLocal {
        spec: String::new(),
    })
    .unwrap();
    assert!(last_status(&mut app).contains("local inference is off"));
    app.saved.local = Some(crate::provider::local::LocalConfig {
        provider: crate::provider::local::LocalRuntime::Lmstudio,
        endpoint: None,
        list_command: None,
        memory_budget_mb: None,
        context_length: None,
        kv_cache_q8: false,
        parallel_tool_calls: None,
        tier: None,
    });
    app.configure_local("  ");
    let status = last_status(&mut app);
    assert!(
        status.contains("LM Studio") && status.contains("http://localhost:1234/v1"),
        "{status}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_prompt_budget_warning_is_advisory_once_per_epoch_and_never_hosted() {
    // Never yield: abort each request before the single-threaded runtime can
    // poll its provider task, so this exercises start_stream without network.
    for (backend, context_length, warns) in [
        (BackendTag::Local, Some(2048), true),
        (BackendTag::OpenRouter, Some(2048), false),
        (BackendTag::Local, Some(1_000_000), false),
        (BackendTag::Local, None, false),
    ] {
        let mut app = app_with_key();
        app.models[0].backend = backend;
        app.models[0].context_length = context_length;
        let model = composite_id(&app.models[0]);
        app.current_model = Some(model.clone());
        if backend == BackendTag::Local {
            app.backends.set(
                BackendTag::Local,
                crate::provider::openrouter::OpenRouter::local("http://127.0.0.1:9/v1").unwrap(),
            );
        }
        let session = app
            .db
            .create_session("budget", &model, &app.active_space.id, "chat")
            .unwrap();
        let session_id = session.id.clone();
        app.session = Some(session);
        app.pending_events.clear();
        for (attempt, new_epoch) in [false, false, true].into_iter().enumerate() {
            if new_epoch {
                app.bump_cache_epoch();
            }
            app.start_stream().unwrap();
            assert!(app.is_streaming(), "the warning must not block the request");
            app.discard_chat_task(&session_id);
            let warnings: Vec<_> = app
                .pending_events
                .drain(..)
                .filter_map(|event| match event {
                    AppEvent::Status(text)
                        if text.starts_with("prompt + tool definitions use ~") =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                warnings.len(),
                usize::from(warns && attempt != 1),
                "{backend:?} {context_length:?}, attempt {attempt}"
            );
            for warning in warnings {
                let tokens: u64 = warning
                    .strip_prefix("prompt + tool definitions use ~")
                    .unwrap()
                    .strip_suffix(" of 2048 tokens — raise [local] context_length")
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(tokens > 1024);
            }
        }
    }
}

#[test]
fn local_survey_reports_liveness_and_warns_without_stopping_anything() {
    use crate::provider::local::LocalRuntime;
    use crate::provider::serve::RuntimeStatus;

    let mut app = App::new(Db::open_in_memory().unwrap(), None, test_space());
    let statuses = |over: bool| {
        vec![
            RuntimeStatus {
                runtime: LocalRuntime::Edge0,
                endpoint: "http://localhost:8000/v1".into(),
                port: Some(8000),
                running: true,
                managed: true,
                rss_kb: Some(4300 * 1024),
                over_budget: over,
                budget_kb: Some(3000 * 1024),
            },
            RuntimeStatus {
                runtime: LocalRuntime::Ollama,
                endpoint: "http://localhost:11434/v1".into(),
                port: Some(11434),
                running: false,
                managed: false,
                rss_kb: None,
                over_budget: false,
                budget_kb: Some(3000 * 1024),
            },
        ]
    };
    let drain = |app: &mut App| {
        app.pending_events
            .drain(..)
            .filter_map(|event| match event {
                AppEvent::Status(s) => Some(s),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // A survey the user asked for reports a line per runtime, and warns about
    // the one over budget. (Boot queues a "no API key" line first.)
    let _ = drain(&mut app);
    app.local_status_announce = true;
    app.on_local_status(Some(statuses(true)));
    let lines = drain(&mut app);
    assert!(lines[0].contains("edge0 ●4.2 GB ⚠"), "{lines:?}");
    assert!(lines[0].contains("Ollama ○"), "{lines:?}");
    let warning = lines.last().unwrap();
    assert!(warning.contains("over the 2.9 GB budget"), "{warning}");
    assert!(warning.contains("/local stop edge0"), "{warning}");
    // Advisory only: warning about a server does not stop it.
    assert!(!app.local_servers.manages(LocalRuntime::Edge0));
    assert_eq!(app.local_status.len(), 2);

    // A background refresh (the picker opening) caches without announcing;
    // nothing is over budget, so it says nothing at all.
    app.on_local_status(Some(statuses(false)));
    assert!(drain(&mut app).is_empty());
    assert!(app.local_status_rx.is_none());
}

#[test]
fn stopping_a_server_nexus_did_not_start_is_refused() {
    use crate::provider::local::LocalRuntime;

    let mut app = App::new(Db::open_in_memory().unwrap(), None, test_space());
    let last_status = |app: &mut App| {
        app.pending_events
            .drain(..)
            .filter_map(|event| match event {
                AppEvent::Status(s) => Some(s),
                _ => None,
            })
            .next_back()
            .unwrap()
    };
    app.manage_local_server("stop", "edge0");
    let status = last_status(&mut app);
    assert!(status.contains("did not start"), "{status}");
    // A runtime that is off and unnamed cannot be started by guesswork.
    app.manage_local_server("start", "");
    let status = last_status(&mut app);
    assert!(status.contains("local inference is off"), "{status}");
    assert!(!app.local_servers.manages(LocalRuntime::Edge0));
}

#[test]
fn parse_topic_extracts_and_slugifies() {
    let (t, s) = parse_topic(r#"{"topic": "Rust Async Runtimes", "id": "rust async!"}"#).unwrap();
    assert_eq!(t, "Rust Async Runtimes");
    assert_eq!(s, "rust-async");
    // Tolerates surrounding prose / fences.
    let (t, s) =
        parse_topic("sure:\n```json\n{\"topic\":\"Hi There\",\"id\":\"hi\"}\n```").unwrap();
    assert_eq!(t, "Hi There");
    assert_eq!(s, "hi");
    assert!(parse_topic("no json here").is_none());
}

#[test]
fn code_blocks_split_by_fence_with_lang() {
    let md = "intro\n```rust\nfn a() {}\n```\ntext\n```\nplain\n```";
    let blocks = code_blocks(md);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].0.as_deref(), Some("rust"));
    assert_eq!(blocks[0].1, "fn a() {}\n");
    assert_eq!(blocks[1].0, None);
    assert_eq!(blocks[1].1, "plain\n");
}

#[test]
fn work_mode_commands_parse_through_the_command_seam() {
    let app = app_with_key();
    for (text, mode, root) in [
        ("agent", WorkMode::Agent, None),
        ("agent ./sub", WorkMode::Agent, Some("./sub")),
        ("agent /a directory", WorkMode::Agent, Some("/a directory")),
        ("chat", WorkMode::Chat, None),
    ] {
        let command = AppCommand::SetWorkMode {
            mode,
            root: root.map(str::to_string),
        };
        assert_eq!(app.parse_command(text).unwrap(), command);
        let wire = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<AppCommand>(&wire).unwrap(), command);
    }
}

#[test]
fn agent_defaults_to_cwd_and_resolves_relative_and_home_roots() {
    let mut app = app_with_key();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    for command in ["agent", "agent ./"] {
        app.run_command(command).unwrap();
        assert_eq!(app.work_mode, WorkMode::Agent);
        assert_eq!(app.workspace.as_ref().unwrap().root(), cwd);
    }
    if let Some(home) = std::env::var_os("HOME") {
        app.run_command("agent ~").unwrap();
        assert_eq!(
            app.workspace.as_ref().unwrap().root(),
            std::path::PathBuf::from(home).canonicalize().unwrap()
        );
    }
}

#[test]
fn invalid_agent_root_preserves_the_mode_binding_and_cache_epoch() {
    let mut app = app_with_key();
    app.run_command("agent .").unwrap();
    let root = app.workspace.as_ref().unwrap().root().to_path_buf();
    let epoch = app.cache_epoch;
    let session = app
        .db
        .create_session("bound", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.switch_to_session_by_id(&session.id).unwrap();
    app.run_command("agent .").unwrap();
    let missing = app.space.root.join("missing");
    let bound_epoch = app.cache_epoch;
    app.run_command(&format!("agent {}", missing.display()))
        .unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    assert_eq!(app.workspace.as_ref().unwrap().root(), root);
    assert_eq!(app.db.session_workspace(&session.id), Some(root));
    assert_eq!(app.cache_epoch, bound_epoch);
    assert!(bound_epoch > epoch);
    assert!(app.last_status().contains("workspace error"));
    app.run_command("chat").unwrap();
    app.run_command(&format!("agent {}", missing.display()))
        .unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.last_status().contains("workspace error"));
}

#[test]
fn switching_space_leaves_agent_mode() {
    let mut app = app_with_key();
    app.run_command("agent .").unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    let space = app.active_space.clone();
    app.set_active_space(space);
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    assert!(!app.toolbox.defs().iter().any(|tool| tool.name == "fs"));
}

#[test]
fn agent_refuses_data_roots_their_ancestors_and_their_contents() {
    let mut app = app_with_key();
    let data = app.space.root.clone();
    for root in [
        data.clone(),
        data.parent().unwrap().to_path_buf(),
        data.join("spaces"),
    ] {
        app.run_command(&format!("agent {}", root.display()))
            .unwrap();
        assert_eq!(app.work_mode, WorkMode::Chat);
        assert!(app.workspace.is_none());
        assert!(app.last_status().contains("data root"));
    }
}

#[tokio::test]
async fn pre_session_agent_mode_binds_on_send_and_new_sessions_start_in_chat() {
    let mut app = app_with_key();
    app.current_model = Some("a/one".into());
    let epoch = app.cache_epoch;
    app.run_command("agent .").unwrap();
    assert!(app.session.is_none());
    assert!(app.cache_epoch > epoch);
    let root = app.workspace.as_ref().unwrap().root().to_path_buf();
    app.send_message("hello".into()).unwrap();
    let id = app.session.as_ref().unwrap().id.clone();
    assert_eq!(app.db.session_workspace(&id), Some(root.clone()));
    app.stop_stream().unwrap();
    app.new_session();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    assert!(!app.toolbox.defs().iter().any(|tool| tool.name == "fs"));
    app.switch_to_session_by_id(&id).unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    assert_eq!(app.workspace.as_ref().unwrap().root(), root);
    let epoch = app.cache_epoch;
    app.run_command("chat").unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert_eq!(app.session.as_ref().unwrap().id, id);
    assert!(!app.messages.is_empty());
    assert_eq!(app.db.session_workspace(&id), None);
    assert!(app.cache_epoch > epoch);
    app.switch_to_session_by_id(&id).unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
}

#[test]
fn session_workspace_restores_after_restart_and_reports_a_deleted_directory() {
    let space = test_space();
    std::fs::create_dir_all(&space.root).unwrap();
    let data = space.root.clone();
    let workspace = test_space().root;
    std::fs::create_dir_all(&workspace).unwrap();
    let mut app = App::new(Db::open(&space.db_path()).unwrap(), None, space);
    let session = app
        .db
        .create_session("bound", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.switch_to_session_by_id(&session.id).unwrap();
    app.run_command(&format!("agent {}", workspace.display()))
        .unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    let canonical = workspace.canonicalize().unwrap();
    assert_eq!(
        app.db.session_workspace(&session.id),
        Some(canonical.clone())
    );
    drop(app);

    let space = Space { root: data.clone() };
    let mut app = App::new(Db::open(&space.db_path()).unwrap(), None, space);
    app.switch_to_session_by_id(&session.id).unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    assert_eq!(app.workspace.as_ref().unwrap().root(), canonical);
    std::fs::remove_dir_all(&workspace).unwrap();
    app.new_session();
    app.switch_to_session_by_id(&session.id).unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    assert_eq!(
        app.last_status(),
        format!(
            "workspace {} is not available on this device · /agent <path> to bind another",
            canonical.display()
        )
    );
    assert!(!app.toolbox.defs().iter().any(|tool| tool.name == "fs"));
    drop(app);
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn agent_is_refused_in_incognito_and_toggling_privacy_drops_workspace_tools() {
    let mut app = app_with_key();
    app.run_command("agent .").unwrap();
    assert_eq!(app.work_mode, WorkMode::Agent);
    app.toggle_incognito().unwrap();
    assert!(app.incognito);
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    let epoch = app.cache_epoch;
    app.run_command("agent .").unwrap();
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert_eq!(app.cache_epoch, epoch);
    assert!(app.last_status().contains("not available in incognito"));
    for name in ["fs", "shell"] {
        assert!(!app.toolbox.defs().iter().any(|tool| tool.name == name));
    }
}

#[tokio::test]
async fn agent_toolbox_reads_but_blocks_mutations_and_chat_hides_workspace_tools() {
    let mut app = app_with_key();
    let root = test_space().root;
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file"), "original\n").unwrap();
    for name in ["fs", "shell"] {
        assert!(!app.toolbox.defs().iter().any(|tool| tool.name == name));
    }
    app.run_command(&format!("agent {}", root.display()))
        .unwrap();
    for name in ["fs", "shell"] {
        assert!(app.toolbox.defs().iter().any(|tool| tool.name == name));
    }
    let (read, status) = app
        .toolbox
        .run("fs", r#"{"action":"read","path":"file"}"#)
        .await;
    assert!(read.contains("original"), "{read}");
    assert_ne!(status, "blocked");
    for (tool, args) in [
        (
            "fs",
            r#"{"action":"write","path":"file","content":"changed"}"#,
        ),
        ("shell", r#"{"action":"run","cmd":"touch created"}"#),
    ] {
        let (result, status) = app.toolbox.run(tool, args).await;
        assert_eq!(
            result,
            "needs approval · not available here (headless); describe the change and let the user apply it"
        );
        assert_eq!(status, "blocked");
    }
    assert_eq!(
        std::fs::read_to_string(root.join("file")).unwrap(),
        "original\n"
    );
    assert!(!root.join("created").exists());
    app.run_command("chat").unwrap();
    for name in ["fs", "shell"] {
        assert!(!app.toolbox.defs().iter().any(|tool| tool.name == name));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_prompt_is_after_environment_and_included_in_context_counts() {
    let mut app = app_with_key();
    app.system_prompt_overlay = "user additions".into();
    let before = app.context_breakdown().system_tokens;
    let environment_len = app.environment_section().chars().count();
    app.run_command("agent .").unwrap();
    let prompt = app.system_prompt();
    let environment = prompt.find("## Environment").unwrap();
    let agent = prompt.find("\n\n## Agent mode\n").unwrap();
    let overlay = prompt.find("\n\nuser additions").unwrap();
    assert!(environment < agent && agent < overlay);
    assert!(prompt.contains("- Mode: agent · workspace "));
    let extra_chars = app.environment_section().chars().count() - environment_len
        + crate::prompts::render(crate::prompts::AGENT, &[])
            .chars()
            .count();
    let delta = app.context_breakdown().system_tokens - before;
    let expected = u64::try_from(extra_chars / 4).unwrap();
    assert!((expected..=expected + 1).contains(&delta));
    app.run_command("chat").unwrap();
    assert!(!app.system_prompt().contains("## Agent mode"));
    assert!(!app.system_prompt().contains("- Mode:"));
    assert_eq!(app.context_breakdown().system_tokens, before);
}

pub fn app_with_key() -> App {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("sk-or-test-key"), test_space());
    a.models = vec![
        Model {
            id: "a/one".into(),
            name: "One".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
        Model {
            id: "b/two".into(),
            name: "Two".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
    ];
    a
}

#[test]
fn web_mode_clause_instructs_search_first_and_cites_inline() {
    let c = chat::web_mode_clause("2026-07-06");
    assert!(c.contains("2026-07-06"));
    assert!(c.contains("search with mode=web"));
    assert!(c.contains("[n]"));
    assert!(c.contains("Sources:"));
    assert!(c.contains("Do not fabricate"));
}

#[tokio::test]
async fn web_mode_toggles_persists_and_shows_in_system_prompt() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    assert!(!a.web_mode);
    a.toggle_web_mode();
    assert!(a.web_mode);
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("web mode on"));

    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    let sid = a.session.as_ref().unwrap().id.clone();
    let sessions = a.db.list_sessions(&a.active_space.id).unwrap();
    assert!(sessions.iter().find(|s| s.id == sid).unwrap().web_mode);
    assert!(a.system_prompt().contains("Web answer mode is ON"));

    a.toggle_web_mode(); // off, same session
    assert!(!a.system_prompt().contains("Web answer mode is ON"));
    let sessions = a.db.list_sessions(&a.active_space.id).unwrap();
    assert!(!sessions.iter().find(|s| s.id == sid).unwrap().web_mode);
}

#[test]
fn no_key_rejects_message_and_points_to_login_cmd() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, None, test_space());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    assert!(a.session.is_none());
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("/login"));
}

#[test]
fn message_without_model_is_rejected() {
    let mut a = app_with_key();
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    assert!(a.session.is_none());
    // The composer restore is an event — the view re-applies it.
    let (sets, status) = a.drain_ui_events();
    assert_eq!(sets, vec!["hello".to_string()]);
    assert!(status.contains("pick a model"));
}

#[tokio::test]
async fn message_with_model_creates_session_and_streams() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello world".to_string(),
    })
    .unwrap();
    assert!(a.session.is_some());
    assert!(a.is_streaming());
    assert_eq!(a.messages.len(), 1);
    assert_eq!(a.messages[0].role, "user");
}

fn app_with_reply() -> App {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let session =
        a.db.create_session("chat", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.db.add_user_message(&session.id, "hello").unwrap();
    a.db.add_assistant_message(&session.id, "old reply", None, None, None, None, None, None)
        .unwrap();
    a.messages = a.db.load_messages(&session.id).unwrap();
    a.session = Some(session);
    a
}

#[test]
fn retry_and_regen_parse_to_the_retry_command() {
    let a = app_with_key();
    assert_eq!(a.parse_command("retry").unwrap(), AppCommand::Retry);
    assert_eq!(a.parse_command("regen").unwrap(), AppCommand::Retry);
}

#[tokio::test]
async fn retry_removes_the_last_reply_and_keeps_the_user_message() {
    let mut a = app_with_reply();
    let sid = a.session.as_ref().unwrap().id.clone();
    let replaced = a.db.reply_ids_after_last_user(&sid).unwrap();
    a.run_command("retry").unwrap();

    assert_eq!(a.messages.len(), 1);
    assert_eq!(a.messages[0].role, "user");
    assert_eq!(a.messages[0].content, "hello");
    let stored = a.db.load_messages(&sid).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].role, "user");
    assert_eq!(stored[0].content, "hello");
    let mut stmt =
        a.db.conn_for_test()
            .prepare("SELECT row_id FROM sync_tombstones WHERE table_name = 'messages'")
            .unwrap();
    let tombstones = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(tombstones, replaced);
    drop(stmt);
    assert!(a.is_streaming());
    assert!(
        a.pending_events
            .iter()
            .any(|event| matches!(event, AppEvent::HistoryInvalidated))
    );
    assert_eq!(a.build_history().last().unwrap().content, "hello");
    a.stop_stream().unwrap();
}

#[tokio::test]
async fn retry_keeps_a_skill_row() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let session =
        a.db.create_session("chat", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.db.add_skill_message(&session.id, "web-search\nsearch instructions")
        .unwrap();
    a.db.add_user_message(&session.id, "rust news").unwrap();
    a.db.add_assistant_message(&session.id, "old news", None, None, None, None, None, None)
        .unwrap();
    a.messages = a.db.load_messages(&session.id).unwrap();
    a.session = Some(session.clone());
    a.run_command("regen").unwrap();

    let stored = a.db.load_messages(&session.id).unwrap();
    for messages in [&a.messages, &stored] {
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "skill");
        assert_eq!(messages[0].content, "web-search\nsearch instructions");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "rust news");
    }
    let history = a.build_history();
    assert_eq!(
        history[history.len() - 2].content,
        "The user invoked the skill 'web-search'. Follow these instructions for the rest of this conversation:\nsearch instructions"
    );
    assert_eq!(history.last().unwrap().content, "rust news");
    assert!(a.is_streaming());
    a.stop_stream().unwrap();
}

fn agent_app_with_reply() -> (App, std::path::PathBuf) {
    let mut a = app_with_reply();
    let workspace = test_space().root;
    std::fs::create_dir_all(&workspace).unwrap();
    a.run_command(&format!("agent {}", workspace.display()))
        .unwrap();
    while a.pop_pending_event().is_some() {}
    (a, workspace.canonicalize().unwrap())
}

#[tokio::test]
async fn undo_refuses_outside_agent_mode_and_while_streaming() {
    let mut a = app_with_reply();
    assert_eq!(a.parse_command("undo").unwrap(), AppCommand::Undo);
    a.run_command("undo").unwrap();
    assert!(a.last_status().contains("works in agent mode"));
    let (mut a, workspace) = agent_app_with_reply();
    let root = a.space.root.clone();
    a.run_command("undo").unwrap();
    assert!(a.last_status().contains("nothing to undo"));
    a.start_stream().unwrap();
    a.run_command("undo").unwrap();
    assert!(a.last_status().contains("still streaming"));
    a.stop_stream().unwrap();
    drop(a);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn undo_restores_the_last_agent_turn_and_retry_points_to_it() {
    let (mut a, workspace) = agent_app_with_reply();
    let root = a.space.root.clone();
    let file = workspace.join("notes");
    std::fs::write(&file, "original").unwrap();
    let mut toolbox = crate::tools::ToolBox::new(
        root.join("skills"),
        None,
        None,
        "auto".to_string(),
        Vec::new(),
        None,
        None,
        None,
    );
    toolbox.workspace.clone_from(&a.workspace);
    let session_id = a.session.as_ref().unwrap().id.clone();
    for content in ["turn one", "turn two"] {
        a.start_stream().unwrap();
        a.stop_stream().unwrap();
        toolbox
            .run("fs", r#"{"action":"read","path":"notes"}"#)
            .await;
        let args = serde_json::json!({"action": "write", "path": "notes", "content": content});
        let (result, _) = toolbox.run("fs", &args.to_string()).await;
        assert!(result.starts_with("wrote notes"), "{result}");
    }
    toolbox
        .run(
            "fs",
            r#"{"action":"write","path":"created","content":"new"}"#,
        )
        .await;
    assert!(
        root.join("checkpoints")
            .join(&session_id)
            .join("2")
            .exists()
    );

    while a.pop_pending_event().is_some() {}
    a.run_command("retry").unwrap();
    assert!(a.last_status().contains("/undo"));
    a.stop_stream().unwrap();
    assert!(
        !root
            .join("checkpoints")
            .join(&session_id)
            .join("3")
            .exists()
    );

    a.run_command("undo").unwrap();
    assert_eq!(
        a.last_status(),
        "undid 2 file changes from the last agent turn · 0 skipped (changed since) · shell commands are not undone"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "turn one");
    assert!(!workspace.join("created").exists());
    assert!(a.workspace.as_ref().unwrap().last_read(&file).is_none());
    std::fs::write(&file, "user edit").unwrap();
    a.run_command("undo").unwrap();
    assert_eq!(
        a.last_status(),
        "undid 0 file changes from the last agent turn · 1 skipped (changed since: notes) · shell commands are not undone"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "user edit");
    a.run_command("undo").unwrap();
    assert!(a.last_status().contains("nothing to undo"));
    drop(a);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn undo_waits_for_the_workspace_root_its_turn_changed() {
    let (mut a, root_a) = agent_app_with_reply();
    let root = a.space.root.clone();
    let root_b = test_space().root;
    std::fs::create_dir_all(&root_b).unwrap();
    let root_b = root_b.canonicalize().unwrap();
    let file = root_a.join("notes");
    std::fs::write(&file, "original").unwrap();
    let mut toolbox = crate::tools::ToolBox::new(
        root.join("skills"),
        None,
        None,
        "auto".to_string(),
        Vec::new(),
        None,
        None,
        None,
    );
    toolbox.workspace.clone_from(&a.workspace);
    a.start_stream().unwrap();
    a.stop_stream().unwrap();
    toolbox
        .run("fs", r#"{"action":"read","path":"notes"}"#)
        .await;
    let (result, _) = toolbox
        .run(
            "fs",
            r#"{"action":"write","path":"notes","content":"agent"}"#,
        )
        .await;
    assert!(result.starts_with("wrote notes"), "{result}");
    let checkpoint = root
        .join("checkpoints")
        .join(&a.session.as_ref().unwrap().id)
        .join("1");

    a.run_command(&format!("agent {}", root_b.display()))
        .unwrap();
    while a.pop_pending_event().is_some() {}
    a.run_command("undo").unwrap();
    assert_eq!(
        a.last_status(),
        format!(
            "the last agent turn changed files in {root} · switch back with /agent {root} to undo it",
            root = root_a.display()
        )
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "agent");
    assert!(checkpoint.exists());

    a.run_command(&format!("agent {}", root_a.display()))
        .unwrap();
    while a.pop_pending_event().is_some() {}
    a.run_command("undo").unwrap();
    assert!(a.last_status().starts_with("undid 1 file changes"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
    assert!(!checkpoint.exists());
    drop(a);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(root_a).unwrap();
    std::fs::remove_dir_all(root_b).unwrap();
}

#[test]
fn retry_refuses_when_nothing_to_retry() {
    let mut a = app_with_key();
    a.run_command("retry").unwrap();
    assert!(a.last_status().contains("no active session"));
    assert!(a.session.is_none());
    assert!(a.messages.is_empty());
    let session =
        a.db.create_session("chat", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(session.clone());
    for content in [None, Some("no user yet"), Some("no reply yet")] {
        match content {
            Some("no user yet") => {
                a.db.add_error_message(&session.id, "no user yet").unwrap();
            }
            Some(text) => {
                a.db.add_user_message(&session.id, text).unwrap();
            }
            None => {}
        }
        a.messages = a.db.load_messages(&session.id).unwrap();
        let changes = a.db.conn_for_test().total_changes();
        let count = a.messages.len();
        a.run_command("retry").unwrap();
        assert_eq!(a.messages.len(), count);
        assert_eq!(a.db.conn_for_test().total_changes(), changes);
        assert!(!a.is_streaming());
        assert!(
            a.pending_events
                .iter()
                .all(|event| matches!(event, AppEvent::Status(_)))
        );
        assert!(a.last_status().contains("nothing to retry"));
    }
}

#[tokio::test]
async fn retry_refuses_while_streaming() {
    let mut a = app_with_reply();
    a.start_stream().unwrap();
    let task = a.active_chat_task().unwrap().id;
    let changes = a.db.conn_for_test().total_changes();
    a.run_command("retry").unwrap();

    assert!(a.last_status().contains("still streaming"));
    assert_eq!(a.chat_task_count(), 1);
    assert_eq!(a.active_chat_task().unwrap().id, task);
    assert_eq!(a.messages.len(), 2);
    assert_eq!(a.messages.last().unwrap().content, "old reply");
    assert_eq!(a.db.conn_for_test().total_changes(), changes);
    a.stop_stream().unwrap();
}

#[test]
fn retry_refuses_swarm_and_research_sessions() {
    for (swarm_mode, kind) in [(true, "chat"), (false, "research")] {
        let mut a = app_with_reply();
        let session = a.session.as_mut().unwrap();
        session.swarm_mode = swarm_mode;
        session.kind = kind.into();
        let changes = a.db.conn_for_test().total_changes();
        a.run_command("retry").unwrap();

        assert!(a.last_status().contains("swarm or research"));
        assert!(!a.is_streaming());
        assert_eq!(a.messages.len(), 2);
        assert_eq!(a.messages.last().unwrap().content, "old reply");
        assert_eq!(a.db.conn_for_test().total_changes(), changes);
    }
}

#[tokio::test]
async fn retry_labels_the_replaced_turn_rejected() {
    let mut a = app_with_reply();
    let root = a.space.root.clone();
    a.run_command("training on").unwrap();
    let sid = a.session.as_ref().unwrap().id.clone();
    let store = std::sync::Arc::new(
        crate::trace::TraceStore::open(
            &a.space.traces_dir(),
            crate::trace::Scrubber::new(Vec::new()),
        )
        .unwrap(),
    );
    let mut step = crate::trace::TraceStep {
        session_id: sid.clone(),
        space_id: a.active_space.id.clone(),
        turn_id: "t1".into(),
        round: 0,
        model: "a/one".into(),
        backend: "OpenRouter".into(),
        system: "Be helpful.".into(),
        tools: Vec::new(),
        messages: vec![crate::provider::ChatMessage::text("user", "hello")],
        params: serde_json::json!({}),
        response: crate::trace::TraceResponse {
            content: "old reply".into(),
            reasoning: None,
            tool_calls: Vec::new(),
            finish: "stop".into(),
        },
        usage: None,
        latency_ms: 1,
        tool_results: Vec::new(),
    };
    let first = store.record_step(&step).unwrap();
    step.turn_id = "t2".into();
    let other = store.record_step(&step).unwrap();
    step.turn_id = "t1".into();
    step.round = 1;
    let second = store.record_step(&step).unwrap();
    a.trace_store = Some(store.clone());
    a.last_turn = Some((sid.clone(), "t1".into()));
    a.run_command("retry").unwrap();

    assert_eq!(
        store.steps_for_turn("t1").unwrap(),
        vec![first.clone(), second.clone()]
    );
    for id in [first, second] {
        let loaded = store.load_step(&id).unwrap();
        assert_eq!(loaded["response"]["content"], "old reply");
        assert_eq!(loaded["labels"].as_array().unwrap().len(), 1);
        assert_eq!(loaded["labels"][0]["kind"], "rejected");
        assert_eq!(loaded["labels"][0]["value"], "retry");
    }
    assert_eq!(
        store.load_step(&other).unwrap()["labels"],
        serde_json::json!([])
    );
    let (session, turn) = a.last_turn.as_ref().unwrap();
    assert_eq!(session, &sid);
    assert_ne!(turn, "t1");
    assert!(a.is_streaming());
    a.stop_stream().unwrap();
    drop(a);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn retry_in_incognito_touches_no_rows() {
    let mut a = app_with_reply();
    let sid = a.session.as_ref().unwrap().id.clone();
    a.incognito = true;
    a.messages.last_mut().unwrap().content = "private reply".into();
    let changes = a.db.conn_for_test().total_changes();
    a.run_command("retry").unwrap();

    assert_eq!(a.db.conn_for_test().total_changes(), changes);
    let stored = a.db.load_messages(&sid).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored.last().unwrap().content, "old reply");
    assert_eq!(a.messages.len(), 1);
    assert_eq!(a.messages[0].role, "user");
    assert_eq!(a.messages[0].content, "hello");
    assert!(a.is_streaming());
    assert!(a.active_chat_task().unwrap().incognito);
    a.stop_stream().unwrap();
}

#[test]
fn ocr_settings_defaults_gate_and_cycle() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("sk-or-test"), test_space());
    assert_eq!(a.ocr_model, "google/gemini-2.5-flash-lite");
    assert_eq!(a.ocr_engine, "auto");
    assert_eq!(a.embedding_model, "openai/text-embedding-3-small");
    assert!(a.vlm_ocr_enabled()); // auto + model configured
    a.cycle_ocr_engine().unwrap(); // auto → tesseract
    assert_eq!(a.ocr_engine, "tesseract");
    assert!(!a.vlm_ocr_enabled());
    a.cycle_ocr_engine().unwrap(); // tesseract → vlm
    assert!(a.vlm_ocr_enabled());
    a.cycle_ocr_engine().unwrap(); // vlm → local
    assert_eq!(a.ocr_engine, "local");
    assert!(!a.vlm_ocr_enabled()); // local routes to ollama, not OpenRouter
    a.cycle_ocr_engine().unwrap(); // local → auto
    assert_eq!(a.ocr_engine, "auto");
    a.clear_ocr_model().unwrap();
    assert!(!a.vlm_ocr_enabled()); // no model, auto can't use vlm
}

#[tokio::test]
async fn stream_is_tagged_with_its_origin_session() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    let sid = a.session.as_ref().unwrap().id.clone();
    assert_eq!(
        a.active_chat_task().map(|task| task.session_id.clone()),
        Some(sid)
    );
    assert!(a.viewing_stream());

    // Switch to a blank chat: still streaming, but not viewing it.
    a.new_session();
    assert!(a.is_streaming());
    assert!(!a.viewing_stream());
}

#[tokio::test]
async fn background_finish_lands_in_origin_session_and_notifies() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();

    a.new_session(); // switch away mid-stream
    a.on_stream_event(crate::provider::StreamEvent::Token("late answer".into()))
        .unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Done)
        .unwrap();

    // Landed in the origin session, not the (blank) active one.
    assert!(a.messages.is_empty());
    let stored = a.db.load_messages(&origin).unwrap();
    assert_eq!(stored.last().unwrap().role, "assistant");
    assert_eq!(stored.last().unwrap().content, "late answer");
    assert!(a.unread.contains(&origin));
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("response ready in"));
    assert!(a.chat_tasks.is_empty());
}

#[tokio::test]
async fn background_tool_call_persists_to_origin_session_only() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();

    a.new_session();
    a.on_stream_event(crate::provider::StreamEvent::ToolCall {
        id: "call_0".into(),
        reasoning: None,
        assistant_content: None,
        name: "web_search".into(),
        arguments: "{}".into(),
        result: "ok".into(),
    })
    .unwrap();

    assert!(a.messages.is_empty()); // active transcript untouched
    let stored = a.db.load_messages(&origin).unwrap();
    assert_eq!(stored.last().unwrap().role, "tool_call");
}

#[tokio::test]
async fn send_in_another_session_is_allowed_while_one_streams() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    a.new_session();
    a.execute(AppCommand::Send {
        text: "second message".to_string(),
    })
    .unwrap();
    assert_eq!(a.chat_task_count(), 2);
}

#[tokio::test]
async fn second_send_in_the_same_session_is_rejected() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "first".to_string(),
    })
    .unwrap();
    a.execute(AppCommand::Send {
        text: "second".to_string(),
    })
    .unwrap();

    assert_eq!(a.chat_task_count(), 1);
    let (sets, status) = a.drain_ui_events();
    assert_eq!(sets, vec!["second".to_string()]);
    assert!(status.contains("still streaming in"));
}

#[tokio::test]
async fn update_check_notifies_only_for_newer_versions() {
    let mut a = app_with_key();
    // Newer published version -> status + notification.
    a.on_update_check(Some("99.0.0".into()));
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("update available"));
    assert_eq!(a.notifications.len(), 1);
    // Same/older version or a failed check -> silent.
    a.on_update_check(Some("0.0.1".into()));
    a.on_update_check(None);
    assert_eq!(a.notifications.len(), 1, "only newer versions notify");
}

#[tokio::test]
async fn update_check_due_is_daily_throttled() {
    let db = Db::open_in_memory().unwrap();
    assert!(db.update_check_due(), "first check of the day is due");
    assert!(!db.update_check_due(), "second check same day is throttled");
}

#[tokio::test]
async fn concurrent_chat_tasks_route_results_to_their_origin_sessions() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "first".to_string(),
    })
    .unwrap();
    let first_session = a.session.as_ref().unwrap().id.clone();
    let first_task = a
        .chat_task_for_session(&first_session)
        .map(|task| task.id)
        .unwrap();

    a.new_session();
    a.current_model = Some("b/two".into());
    a.execute(AppCommand::Send {
        text: "second".to_string(),
    })
    .unwrap();
    let second_session = a.session.as_ref().unwrap().id.clone();
    let second_task = a
        .chat_task_for_session(&second_session)
        .map(|task| task.id)
        .unwrap();
    assert_ne!(first_session, second_session);
    assert_ne!(first_task, second_task);

    a.on_chat_event(first_task, StreamEvent::Token("answer one".into()))
        .unwrap();
    a.on_chat_event(second_task, StreamEvent::Token("answer two".into()))
        .unwrap();
    a.on_chat_event(first_task, StreamEvent::Done).unwrap();
    a.on_chat_event(second_task, StreamEvent::Done).unwrap();

    let first = a.db.load_messages(&first_session).unwrap();
    assert_eq!(first.last().unwrap().content, "answer one");
    assert_eq!(first.last().unwrap().model.as_deref(), Some("a/one"));
    assert_eq!(a.messages.last().unwrap().content, "answer two");
    assert_eq!(a.messages.last().unwrap().model.as_deref(), Some("b/two"));
    assert_eq!(a.notifications.len(), 1);
    assert!(a.unread.contains(&first_session));
}

#[tokio::test]
async fn opencode_split_usage_merges_cost_into_one_row() {
    // OpenCode Zen sends accounting as two events: the finish chunk carries
    // real usage, then a trailing chunk carries only the provider cost.
    // Both must merge into a single usage_log row — not two.
    let mut a = app_with_key();
    a.current_model = Some("go:deepseek-v4-pro".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    let session = a.session.as_ref().unwrap().id.clone();
    let task = a.chat_task_for_session(&session).map(|t| t.id).unwrap();

    a.on_chat_event(
        task,
        StreamEvent::Usage(crate::provider::Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            cache_read_tokens: 60,
            cache_creation_tokens: 0,
            prompt_convention: Default::default(),
            cost: None,
        }),
    )
    .unwrap();
    a.on_chat_event(
        task,
        StreamEvent::Usage(crate::provider::Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            prompt_convention: Default::default(),
            cost: Some(0.0042),
        }),
    )
    .unwrap();

    let rows = a.db.usage_recent(10, None).unwrap();
    assert_eq!(rows.len(), 1, "split accounting must log exactly one row");
    assert_eq!(rows[0].prompt_tokens, 100);
    assert_eq!(rows[0].completion_tokens, 20);
    assert_eq!(rows[0].cache_read_tokens, 60);
    assert_eq!(rows[0].cost, Some(0.0042));
    assert!(a.last_cache_rate.is_some());
}

#[tokio::test]
async fn each_real_api_usage_event_gets_its_own_row() {
    let mut a = app_with_key();
    a.current_model = Some("go:deepseek-v4-pro".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    let session = a.session.as_ref().unwrap().id.clone();
    let task = a.chat_task_for_session(&session).map(|t| t.id).unwrap();

    for prompt_tokens in [100, 140] {
        a.on_chat_event(
            task,
            StreamEvent::Usage(crate::provider::Usage {
                prompt_tokens,
                completion_tokens: 20,
                total_tokens: prompt_tokens + 20,
                cache_read_tokens: prompt_tokens.saturating_sub(20),
                cache_creation_tokens: 0,
                prompt_convention: Default::default(),
                cost: None,
            }),
        )
        .unwrap();
    }

    let rows = a.db.usage_recent(10, None).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].prompt_tokens, 140);
    assert_eq!(rows[1].prompt_tokens, 100);
}

#[tokio::test]
async fn cost_only_usage_event_is_logged_with_zero_tokens() {
    // OpenCode Zen free models never report usage — the trailing cost chunk
    // is the only accounting. It must still land in the log (provider cost
    // beats the catalog's unknown price) and not clobber the cache rate.
    let mut a = app_with_key();
    a.current_model = Some("deepseek-v4-flash-free".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    let session = a.session.as_ref().unwrap().id.clone();
    let task = a.chat_task_for_session(&session).map(|t| t.id).unwrap();
    a.last_cache_rate = Some(0.5);

    a.on_chat_event(
        task,
        StreamEvent::Usage(crate::provider::Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            prompt_convention: Default::default(),
            cost: Some(0.0),
        }),
    )
    .unwrap();

    let rows = a.db.usage_recent(10, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prompt_tokens, 0);
    assert_eq!(rows[0].cost, Some(0.0));
    assert_eq!(
        a.last_cache_rate,
        Some(0.5),
        "cost-only events must not clear the cache rate"
    );
}

#[tokio::test]
async fn chat_task_limit_rejects_the_eleventh_task() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    for index in 0..MAX_CHAT_TASKS {
        if index > 0 {
            a.new_session();
        }
        let message = format!("message {index}");
        a.execute(AppCommand::Send {
            text: message.to_string(),
        })
        .unwrap();
    }
    assert_eq!(a.chat_task_count(), MAX_CHAT_TASKS);

    a.new_session();
    a.execute(AppCommand::Send {
        text: "rejected".to_string(),
    })
    .unwrap();
    assert_eq!(a.chat_task_count(), MAX_CHAT_TASKS);
    assert!(a.session.is_none());
    let (sets, status) = a.drain_ui_events();
    assert_eq!(sets, vec!["rejected".to_string()]);
    assert!(status.contains("task limit reached"));
}

#[tokio::test]
async fn clicking_a_completion_notification_opens_its_session() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "first".to_string(),
    })
    .unwrap();
    let first_session = a.session.as_ref().unwrap().id.clone();
    let first_task = a
        .chat_task_for_session(&first_session)
        .map(|task| task.id)
        .unwrap();
    a.new_session();
    a.on_chat_event(first_task, StreamEvent::Token("done".into()))
        .unwrap();
    a.on_chat_event(first_task, StreamEvent::Done).unwrap();

    assert_eq!(a.notifications.len(), 1);
    a.activate_notification(0).unwrap();
    assert_eq!(a.session.as_ref().unwrap().id, first_session);
    assert!(a.notifications.is_empty());
    assert!(!a.unread.contains(&first_session));
}

#[tokio::test]
async fn opening_a_session_clears_its_unread_marker() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();
    a.new_session();
    a.on_stream_event(crate::provider::StreamEvent::Token("done".into()))
        .unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Done)
        .unwrap();
    assert!(a.unread.contains(&origin));

    a.switch_to_session_by_id(&origin).unwrap();
    assert_eq!(a.session.as_ref().unwrap().id, origin);
    assert!(!a.unread.contains(&origin));
    assert_eq!(a.messages.last().unwrap().content, "done"); // reloaded from db
}

#[tokio::test]
async fn welcome_screen_shows_while_a_stream_runs_elsewhere() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    assert!(!a.is_welcome()); // viewing the streaming session
    a.new_session();
    assert!(a.is_welcome()); // blank chat, stream backgrounded
}

#[tokio::test]
async fn esc_stop_keeps_partial_response() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hello".to_string(),
    })
    .unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Token("partial answer".into()))
        .unwrap();
    a.stop_stream().unwrap();
    assert!(!a.is_streaming());
    assert!(a.chat_tasks.is_empty());
    assert_eq!(a.messages.last().unwrap().content, "partial answer");
    assert_eq!(a.last_status(), "response stopped");
    // No-op when nothing streams.
    a.stop_stream().unwrap();
}

#[test]
fn effort_accepted_follows_the_models_own_list() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k"), test_space());
    a.models = vec![
        Model {
            id: "claude".into(),
            name: "C".into(),
            reasoning_efforts: crate::provider::ReasoningEffort::WITH_MINIMAL.to_vec(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
        Model {
            id: "dumb".into(),
            name: "D".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
    ];
    // Claude's own list: minimal/low/medium/high.
    assert!(a.effort_accepted("claude", "minimal"));
    assert!(a.effort_accepted("claude", "high"));
    assert!(!a.effort_accepted("claude", "max")); // not in the accepted set
    // No reasoning mode → nothing is accepted.
    assert!(!a.effort_accepted("dumb", "low"));
    // Unknown model → accept anything, so a stored value is never silently
    // dropped just because the catalog isn't loaded.
    assert!(a.effort_accepted("not-in-catalog", "high"));
}

#[tokio::test]
async fn starting_a_request_clears_an_unsupported_reasoning_preference() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".to_string());
    let session =
        a.db.create_session("t", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(session);
    a.reasoning.insert("a/one".to_string(), "high".to_string());
    a.db.set_reasoning("a/one", Some("high")).unwrap();

    a.start_stream().unwrap();

    assert!(!a.reasoning.contains_key("a/one"));
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("cleared unsupported reasoning"));
    assert!(
        a.db.load_model_prefs()
            .unwrap()
            .iter()
            .find(|pref| pref.id == "a/one")
            .is_some_and(|pref| pref.reasoning.is_none())
    );
    for task in a.chat_tasks.values() {
        task.abort.abort();
    }
}

#[test]
fn settings_groups_cover_every_field_exactly_once() {
    let grouped: Vec<&str> = SETTINGS_GROUPS
        .iter()
        .flat_map(|g| g.fields.iter().map(|f| f.label()))
        .collect();
    let unique: std::collections::HashSet<&str> = grouped.iter().copied().collect();
    assert_eq!(
        grouped.len(),
        unique.len(),
        "a field appears in more than one group"
    );
    for f in SettingsField::ALL {
        assert!(
            grouped.contains(&f.label()),
            "field missing from SETTINGS_GROUPS: {}",
            f.label()
        );
    }
    assert_eq!(grouped.len(), SettingsField::ALL.len());
}

#[test]
fn base_system_prompt_is_always_present_and_resolves_verbosity() {
    let a = app_with_key();
    assert!(!a.base_system_prompt.is_empty());
    assert!(a.base_system_prompt.contains("{{verbosity}}")); // placeholder present in the raw file
    let resolved = a.system_prompt();
    assert!(!resolved.contains("{{verbosity}}")); // placeholder gets swapped
    assert!(resolved.contains(verbosity_clause("concise"))); // default level
}

#[test]
fn base_prompt_has_no_volatile_or_stale_text() {
    for text in ["{{datetime}}", "OpenRouter", "web-search"] {
        assert!(!config::BASE_SYSTEM_PROMPT.contains(text), "{text}");
    }
    assert!(config::BASE_SYSTEM_PROMPT.contains("{{verbosity}}"));
}

#[test]
fn system_prompt_is_static_first() {
    let mut a = app_with_key();
    let instructions = "Space instructions: prefer concrete examples.";
    std::fs::write(
        a.space.instructions_path(&a.active_space.name),
        instructions,
    )
    .unwrap();
    let overlay = "User additions: answer in French.";
    a.system_prompt_overlay = overlay.into();
    install_test_skill(
        &mut a,
        "prompt-order",
        "Check prompt order",
        "Use examples.",
    );
    a.memory_snapshot = "The user prefers Rust.".into();
    a.web_mode = true;
    a.interactive_ui = true;

    let prompt = a.system_prompt();
    let positions = [
        config::BASE_SYSTEM_PROMPT.lines().next().unwrap(),
        "## Rendering",
        crate::interactive::INSTRUCTIONS,
        "## Environment",
        instructions,
        overlay,
        "## Skills",
        "## Memory",
        "## Now",
        "Web answer mode is ON",
    ]
    .map(|text| prompt.find(text).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn two_epochs_share_the_whole_prefix_before_now() {
    let mut a = app_with_key();
    a.interactive_ui = true;
    a.web_mode = true;
    a.prompt_datetime = "2026-07-06 12:00 UTC".into();
    let first = a.system_prompt();
    a.prompt_datetime = "2026-07-07 13:00 UTC".into();
    let second = a.system_prompt();

    let (first_prefix, first_tail) = first.split_once("## Now").unwrap();
    let (second_prefix, second_tail) = second.split_once("## Now").unwrap();
    assert_eq!(first_prefix, second_prefix);
    assert_ne!(first_tail, second_tail);
    assert!(first_tail.contains("2026-07-06 12:00 UTC"));
    assert!(second_tail.contains("2026-07-07 13:00 UTC"));
}

#[test]
fn environment_names_backend_model_and_os() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let section = a.environment_section();
    assert!(section.contains("Backend: OpenRouter · model a/one"));
    assert!(section.contains(&format!(
        "- OS: {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )));

    a.current_model = None;
    let section = a.environment_section();
    assert!(!section.contains("Backend:"));
    assert!(section.contains(std::env::consts::OS));

    a.current_model = Some("openai:unconfigured".into());
    let section = a.environment_section();
    assert!(!section.contains("Backend:"));
    assert!(section.contains(std::env::consts::OS));
}

#[test]
fn prompt_mentions_only_tools_that_exist() {
    let a = app_with_key();
    let defs = a.toolbox.defs();
    let now = a.now_section();
    for text in [config::BASE_SYSTEM_PROMPT, now.as_str()] {
        for token in text.split('`').skip(1).step_by(2).filter(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }) {
            assert!(
                defs.iter().any(|tool| tool.name == token),
                "prompt advertises unavailable tool `{token}`"
            );
        }
    }
    assert!(now.contains("use `search` with mode=web"));
}

#[test]
fn rendering_section_only_for_capable_clients() {
    let mut a = app_with_key();
    assert!(!a.system_prompt().contains("## Rendering"));
    a.interactive_ui = true;
    let prompt = a.system_prompt();
    assert!(prompt.contains("## Rendering"));
    for kind in ["flowchart", "sequenceDiagram", "stateDiagram-v2"] {
        assert!(prompt.contains(kind), "{kind}");
    }
}

#[test]
fn system_prompt_appends_overlay_after_base() {
    let mut a = app_with_key();
    a.system_prompt_overlay = "Always answer in French. Today is {{datetime}}.".into();
    let prompt = a.system_prompt();
    let base_first_line = config::BASE_SYSTEM_PROMPT.lines().next().unwrap();
    let overlay = format!("Always answer in French. Today is {}.", a.prompt_datetime);
    assert!(prompt.find(base_first_line).unwrap() < prompt.find(&overlay).unwrap());
    assert!(!prompt.contains("{{"));
}

#[test]
fn empty_overlay_adds_nothing() {
    let mut a = app_with_key();
    a.system_prompt_overlay.clear();
    let without_overlay = a.system_prompt();
    for overlay in ["", " \t\n "] {
        a.system_prompt_overlay = overlay.into();
        assert_eq!(a.system_prompt(), without_overlay);
    }
}

#[test]
fn last_used_model_restored_on_startup() {
    let db = Db::open_in_memory().unwrap();
    db.mark_model_used("a/one").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.mark_model_used("b/two").unwrap(); // more recent
    let a = App::new(db, Some("k"), test_space());
    assert_eq!(a.current_model.as_deref(), Some("b/two"));
}

#[test]
fn context_used_and_limit() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from installed skills
    a.base_system_prompt = String::new(); // isolate from the base system prompt
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    assert_eq!(a.context_limit(), Some(1000));
    let prompt_only = a.context_used();
    a.messages.push(Message {
        role: "user".into(),
        content: "x".repeat(40), // ~10 tokens
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    assert_eq!(a.context_used(), prompt_only + 10);
}

#[tokio::test]
async fn streaming_context_estimate_does_not_shrink_below_last_exact_total() {
    let mut a = app_with_key();
    a.base_system_prompt.clear();
    a.skills.clear();
    a.context_total = Some(10_000);
    let task = tokio::spawn(async {});
    let abort = task.abort_handle();
    let _ = task.await;
    a.chat_tasks.insert(
        1,
        ChatTask {
            id: 1,
            session_id: String::new(),
            session_title: String::new(),
            space_id: String::new(),
            model: "a/one".into(),
            model_id: "a/one".into(),
            backend: BackendTag::OpenRouter,
            incognito: false,
            buffer: String::new(),
            thinking: String::new(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            abort,
        },
    );

    assert_eq!(a.context_used(), 10_000);
}

#[test]
fn compaction_narrows_effective_messages_and_context_used() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from installed skills
    a.base_system_prompt = String::new(); // isolate from the base system prompt
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    let prompt_only = a.context_used();
    let space = a.active_space.id.clone();
    let mut s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    for i in 0..4 {
        a.messages.push(Message {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: "x".repeat(40), // ~10 tokens each
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
    }
    s.compact_summary = Some("y".repeat(80)); // ~20 tokens
    s.compact_through = 2; // first two raw messages folded away
    a.session = Some(s);

    assert_eq!(a.effective_messages().len(), 2);
    // 20 (summary) + 2*10 (tail) = 40 tokens on top of the system prompt's
    // own cost; the two compacted-away messages must NOT be counted.
    assert_eq!(a.context_used(), prompt_only + 40);
}

#[test]
fn on_compact_result_persists_and_clears_stale_total() {
    let mut a = app_with_key();
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    let sid = s.id.clone();
    a.session = Some(s);
    a.context_total = Some(999); // stale exact usage from before compaction

    a.on_compact_result(Some((sid.clone(), "digest".into(), 3, 61)));

    assert_eq!(
        a.session.as_ref().unwrap().compact_summary.as_deref(),
        Some("digest")
    );
    assert_eq!(a.session.as_ref().unwrap().compact_through, 3);
    assert!(a.context_total.is_none()); // stale total dropped
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("compacted: 61%"));
    // Persisted to the db too.
    let reloaded = &a.db.list_sessions(&space).unwrap()[0];
    assert_eq!(reloaded.compact_summary.as_deref(), Some("digest"));
}

#[test]
fn maybe_compact_noop_when_disabled_or_under_threshold() {
    let mut a = app_with_key();
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    a.settings.compact_threshold = 0; // disabled
    a.maybe_compact();
    assert!(a.compact_rx.is_none());

    a.settings.compact_threshold = 60;
    // No session / far under threshold — should still no-op.
    a.maybe_compact();
    assert!(a.compact_rx.is_none());
}

#[tokio::test]
async fn force_compact_reports_why_it_no_ops() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());

    // No active session yet.
    a.force_compact();
    assert!(a.compact_rx.is_none());
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("no active session"));

    // Session exists but everything in it is already covered.
    let space = a.active_space.id.clone();
    let mut s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    s.compact_through = 0;
    a.session = Some(s);
    a.force_compact();
    assert!(a.compact_rx.is_none());
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("nothing new"));

    // Now there's an uncompacted message — should actually kick off a job.
    a.messages.push(Message {
        role: "user".into(),
        content: "hi".into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    a.force_compact();
    assert!(a.compact_rx.is_some());
}

#[test]
fn context_breakdown_reports_system_memory_conversation() {
    let mut a = app_with_key();
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    a.messages.push(Message {
        role: "user".into(),
        content: "x".repeat(40),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    let b = a.context_breakdown();
    assert_eq!(b.conversation_tokens, 10);
    assert!(b.system_tokens > 0); // base system prompt is always present
    assert_eq!(b.limit, Some(1000));
    assert!(!b.compacted);
}

#[test]
fn compact_summary_view_and_edit_roundtrip() {
    let mut a = app_with_key();
    assert!(a.compact_summary_path().is_none()); // not compacted yet

    let space = a.active_space.id.clone();
    let mut s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    s.compact_summary = Some("original digest".into());
    s.compact_through = 2;
    a.session = Some(s);

    let path = a.compact_summary_path().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original digest");

    // Simulate a hand-edit in $EDITOR, then reload.
    std::fs::write(&path, "hand-edited digest\n").unwrap();
    a.reload_compact_summary(&path).unwrap();
    assert_eq!(
        a.session.as_ref().unwrap().compact_summary.as_deref(),
        Some("hand-edited digest")
    );
    let reloaded = &a.db.list_sessions(&space).unwrap()[0];
    assert_eq!(
        reloaded.compact_summary.as_deref(),
        Some("hand-edited digest")
    );
    assert_eq!(reloaded.compact_through, 2); // boundary untouched by the edit
}

#[test]
fn system_prompt_lists_files_but_not_their_content() {
    let mut a = app_with_key();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plan.md"), "SECRET-CONTENT-MARKER inside").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.rescan_files();
    });

    let sp = a.system_prompt();
    assert!(sp.contains("## Files"));
    assert!(sp.contains("plan.md"));
    assert!(sp.contains("files(action=search"));
    assert!(!sp.contains("SECRET-CONTENT-MARKER")); // names only, never content

    // No files → no section.
    std::fs::remove_file(dir.join("plan.md")).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.rescan_files();
    });
    assert!(!a.system_prompt().contains("## Files"));
}

#[test]
fn memory_snapshot_stays_stable_until_refresh() {
    let mut a = app_with_key();
    let path = a.space.memory_path(&a.active_space.name);
    std::fs::write(&path, "1. first fact\n").unwrap();
    a.refresh_memory_snapshot();
    let first_epoch = a.cache_epoch;
    assert!(a.system_prompt().contains("first fact"));

    std::fs::write(&path, "1. newer fact\n").unwrap();
    assert!(a.system_prompt().contains("first fact"));
    assert!(!a.system_prompt().contains("newer fact"));
    assert_eq!(a.cache_epoch, first_epoch);

    a.refresh_memory_snapshot();
    assert!(!a.system_prompt().contains("first fact"));
    assert!(a.system_prompt().contains("newer fact"));
    assert!(a.cache_epoch > first_epoch);
}

#[test]
fn clear_memory_model_disables_extraction() {
    let mut a = app_with_key();
    a.memory_model = "some/model".into();
    a.clear_memory_model().unwrap();
    assert!(a.memory_model.is_empty());
    assert_eq!(
        a.db.load_settings()
            .unwrap()
            .iter()
            .find(|(k, _)| k == "memory_model")
            .map(|(_, v)| v.clone()),
        Some(String::new())
    );
}

#[test]
fn transcriber_model_defaults_and_persists() {
    let mut a = app_with_key();
    assert_eq!(a.transcriber_model, "google/gemini-2.5-flash-lite");
    a.transcriber_model = "some/vision-model".to_string();
    a.db.set_setting("transcriber_model", &a.transcriber_model)
        .unwrap();
    let kv = a.db.load_settings().unwrap();
    assert!(
        kv.iter()
            .any(|(k, v)| k == "transcriber_model" && v == "some/vision-model")
    );
}

#[test]
fn utility_defaults_are_prefixed_for_non_openrouter_bootstrap_backend() {
    let db = Db::open_in_memory().unwrap();
    let a = App::new(db, Some("sk-openai-test-key"), test_space());

    assert_eq!(a.memory_model, "openai:gpt-4.1-mini");
    assert_eq!(a.transcriber_model, "openai:gpt-4.1-mini");
    assert_eq!(a.ocr_model, "openai:gpt-4.1-mini");
    assert_eq!(a.embedding_model, "openai:text-embedding-3-small");
}

#[test]
fn utility_model_resolution_falls_back_from_legacy_openrouter_id_on_openai() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("sk-openai-test-key"), test_space());
    a.models = vec![Model {
        id: "gpt-4.1-mini".into(),
        name: "GPT-4.1 mini".into(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: BackendTag::OpenAi,
        pricing: None,
    }];
    a.current_model = Some("openai:gpt-4.1-mini".into());

    let (provider, raw) = a
        .resolve_utility_model_backend("google/gemini-2.5-flash-lite")
        .unwrap();

    assert_eq!(provider.backend_tag(), BackendTag::OpenAi);
    assert_eq!(raw, "gpt-4.1-mini");
}

#[tokio::test]
async fn finish_stream_persists_assistant_message() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    a.on_stream_event(StreamEvent::Token("pong".into()))
        .unwrap();
    a.on_stream_event(StreamEvent::Done).unwrap();
    assert!(!a.is_streaming());
    assert_eq!(a.messages.last().unwrap().content, "pong");
    let sid = a.session.as_ref().unwrap().id.clone();
    assert_eq!(a.db.load_messages(&sid).unwrap().len(), 2);
}

#[tokio::test]
async fn stream_error_is_persisted_in_transcript_and_not_replayed() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    a.on_stream_event(StreamEvent::Token("partial answer".into()))
        .unwrap();
    a.on_stream_event(StreamEvent::Error("backend unavailable".into()))
        .unwrap();

    assert!(!a.is_streaming());
    assert_eq!(a.messages[a.messages.len() - 2].content, "partial answer");
    assert_eq!(a.messages.last().unwrap().role, "error");
    assert_eq!(a.messages.last().unwrap().content, "backend unavailable");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("backend unavailable"));

    let sid = &a.session.as_ref().unwrap().id;
    let stored = a.db.load_messages(sid).unwrap();
    assert_eq!(stored.last().unwrap().role, "error");
    assert_eq!(stored.last().unwrap().content, "backend unavailable");
    assert!(
        a.build_history()
            .iter()
            .all(|message| !message.content.contains("backend unavailable"))
    );
}

#[test]
fn build_history_compresses_duplicate_tool_results_and_keeps_changed_ones() {
    let mut a = app_with_key();
    let s =
        a.db.create_session("t", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(s);
    a.messages.push(Message {
        role: "user".into(),
        content: "read the file".into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    let row = |result: &str| {
        serde_json::json!({
            "name": "read_file",
            "arguments": r#"{"name":"a.txt"}"#,
            "result": result,
        })
        .to_string()
    };
    // read v1 → re-read (identical) → edit → re-read v2 (changed).
    for result in ["v1", "v1", "v2"] {
        a.messages.push(Message {
            role: "tool_call".into(),
            content: row(result),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
    }
    let history = a.build_history();
    let tools: Vec<&str> = history
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(tools.len(), 3, "all three calls still replayed");
    assert_eq!(tools[0], "v1", "first read stays full");
    assert!(
        tools[1].starts_with(crate::tools::TOOL_RESULT_OMITTED_PREFIX),
        "identical re-read compressed, got: {}",
        tools[1]
    );
    assert!(tools[1].contains("read_file a.txt"), "{}", tools[1]);
    assert_eq!(tools[2], "v2", "changed re-read stays full");
}

#[test]
fn history_prefix_survives_repeated_new_requests() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let session =
        a.db.create_session("history", "a/one", &a.active_space.id, "chat")
            .unwrap();
    for i in 0..100 {
        a.db.add_user_message(&session.id, &format!("user turn {i}"))
            .unwrap();
        a.db.add_assistant_message(
            &session.id,
            &format!("assistant turn {i}"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    a.session = Some(session.clone());
    a.messages = a.db.load_messages(&session.id).unwrap();

    let first = a.build_history();
    assert_eq!(
        first
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        100
    );
    assert_eq!(
        first
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        100
    );

    a.db.add_user_message(&session.id, "user turn 100").unwrap();
    a.messages = a.db.load_messages(&session.id).unwrap();
    let second = a.build_history();
    assert!(
        second
            .iter()
            .any(|message| message.content == "user turn 100")
    );
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second[..first.len()]).unwrap()
    );
}

#[test]
fn hundred_turns_preserve_messages_tool_inputs_results_and_wire_prefixes() {
    let mut a = app_with_key();
    a.base_system_prompt.clear();
    a.skills.clear();
    a.current_model = Some("a/one".into());
    let session =
        a.db.create_session("many turns", "a/one", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(session.clone());

    let mut previous_wire: Option<Vec<serde_json::Value>> = None;
    for turn in 0..100 {
        let user = format!("user turn {turn}: preserve this input");
        a.db.add_user_message(&session.id, &user).unwrap();
        a.messages.push(Message {
            role: "user".into(),
            content: user,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });

        for call_index in 0..2 {
            let id = format!("call-{turn}-{call_index}");
            let name = format!("tool-{call_index}");
            let arguments = format!(r#"{{"turn":{turn},"input":"value-{call_index}"}}"#);
            let result = format!("result for turn {turn}, call {call_index}");
            a.on_stream_event(StreamEvent::ToolCall {
                id,
                reasoning: (call_index == 0).then(|| format!("reasoning for turn {turn}")),
                assistant_content: (call_index == 0).then(|| format!("tool batch for turn {turn}")),
                name,
                arguments,
                result,
            })
            .unwrap();
        }

        let answer = format!("assistant answer for turn {turn}");
        a.db.add_assistant_message(&session.id, &answer, None, None, None, None, None, None)
            .unwrap();
        a.messages.push(Message {
            role: "assistant".into(),
            content: answer,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });

        let history = a.build_history();
        let wire: Vec<serde_json::Value> = history
            .iter()
            .map(serde_json::to_value)
            .collect::<serde_json::Result<_>>()
            .unwrap();
        if let Some(previous) = &previous_wire {
            assert_eq!(
                &wire[..previous.len()],
                previous.as_slice(),
                "wire prefix changed after turn {turn}"
            );
        }
        previous_wire = Some(wire);
    }

    let history = a.build_history();
    let mut cursor = 1; // the stable system message
    for turn in 0..100 {
        assert_eq!(history[cursor].role, "user");
        assert_eq!(
            history[cursor].content,
            format!("user turn {turn}: preserve this input")
        );
        cursor += 1;

        let batch = &history[cursor];
        assert_eq!(batch.role, "assistant");
        assert_eq!(batch.content, format!("tool batch for turn {turn}"));
        let calls = batch.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2);
        for (call_index, call) in calls.iter().enumerate() {
            assert_eq!(call.id, format!("call-{turn}-{call_index}"));
            assert_eq!(call.name, format!("tool-{call_index}"));
            assert_eq!(
                call.arguments,
                format!(r#"{{"turn":{turn},"input":"value-{call_index}"}}"#)
            );
        }
        cursor += 1;

        for call_index in 0..2 {
            let tool = &history[cursor];
            assert_eq!(tool.role, "tool");
            assert_eq!(
                tool.tool_call_id.as_deref(),
                Some(format!("call-{turn}-{call_index}").as_str())
            );
            assert_eq!(
                tool.content,
                format!("result for turn {turn}, call {call_index}")
            );
            cursor += 1;
        }
        assert_eq!(history[cursor].role, "assistant");
        assert_eq!(
            history[cursor].content,
            format!("assistant answer for turn {turn}")
        );
        cursor += 1;
    }
    assert_eq!(cursor, history.len());

    // Reload exactly what was persisted and require the same wire history.
    let persisted = a.db.load_messages(&session.id).unwrap();
    assert_eq!(persisted.len(), 100 * 4);
    a.messages = persisted;
    let reloaded_wire: Vec<serde_json::Value> = a
        .build_history()
        .iter()
        .map(serde_json::to_value)
        .collect::<serde_json::Result<_>>()
        .unwrap();
    assert_eq!(reloaded_wire, previous_wire.unwrap());
}

fn install_test_skill(a: &mut App, name: &str, desc: &str, body: &str) {
    let dir = std::env::temp_dir().join(format!("nexus-test-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n{body}"),
    )
    .unwrap();
    a.skills.push(crate::skills::Skill {
        name: name.to_string(),
        description: desc.to_string(),
        dir,
    });
}

#[tokio::test]
async fn forced_skill_with_trailing_text_sends_immediately() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search rust news").unwrap();
    assert!(a.forced_skill.is_none()); // consumed by send_message
    assert_eq!(a.messages.last().unwrap().content, "rust news");
    assert!(a.is_streaming());
}

#[test]
fn forced_skill_without_text_arms_for_next_message() {
    let mut a = app_with_key();
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search").unwrap();
    assert_eq!(a.forced_skill.as_deref(), Some("web-search"));
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("armed"));
}

#[tokio::test]
async fn forced_skill_persists_as_a_row_before_the_user_message() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search rust news").unwrap();

    let tail = &a.messages[a.messages.len() - 2..];
    assert_eq!(tail[0].role, "skill");
    assert!(tail[0].content.starts_with("web-search\n"));
    assert!(tail[0].content.contains("search instructions"));
    assert_eq!(tail[1].role, "user");
    assert_eq!(tail[1].content, "rust news");

    let persisted = a.db.load_messages(&a.session.as_ref().unwrap().id).unwrap();
    assert_eq!(persisted.len(), a.messages.len());
    let persisted_tail = &persisted[persisted.len() - 2..];
    for (stored, message) in persisted_tail.iter().zip(tail) {
        assert_eq!(stored.role, message.role);
        assert_eq!(stored.content, message.content);
    }
}

#[tokio::test]
async fn skill_row_replays_as_a_user_message_every_later_turn() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search rust news").unwrap();
    let first_history = a.build_history();

    // Reload the snapshot, then change the source file before a later turn.
    a.messages = a.db.load_messages(&a.session.as_ref().unwrap().id).unwrap();
    std::fs::write(
        a.skills.last().unwrap().dir.join("SKILL.md"),
        "changed instructions",
    )
    .unwrap();
    for (role, content) in [("assistant", "Here is the news"), ("user", "Tell me more")] {
        a.messages.push(Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
    }

    let history = a.build_history();
    let skill_messages: Vec<_> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.content
                .contains("Follow these instructions for the rest of this conversation")
        })
        .collect();
    assert_eq!(skill_messages.len(), 1);
    let (skill_index, skill) = skill_messages[0];
    assert_eq!(skill.role, "user");
    assert_eq!(
        skill.content,
        "The user invoked the skill 'web-search'. Follow these instructions for the rest of this conversation:\nsearch instructions"
    );
    let user_index = history
        .iter()
        .position(|m| m.content == "rust news")
        .unwrap();
    assert!(skill_index < user_index);
    assert!(
        !history
            .iter()
            .any(|m| m.role == "system" && m.content.contains("search instructions"))
    );

    let prefix = serde_json::to_value(&history[..=skill_index]).unwrap();
    assert_eq!(
        prefix,
        serde_json::to_value(&first_history[..=skill_index]).unwrap()
    );
    assert_eq!(
        prefix,
        serde_json::to_value(&a.build_history()[..=skill_index]).unwrap()
    );
}

#[test]
fn build_history_leaves_an_armed_skill_armed() {
    let mut a = app_with_key();
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search").unwrap();
    a.build_history();
    assert_eq!(a.forced_skill.as_deref(), Some("web-search"));
}

#[tokio::test]
async fn incognito_skill_row_stays_out_of_the_db() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.incognito = true;
    install_test_skill(
        &mut a,
        "web-search",
        "Search the web",
        "search instructions",
    );
    a.run_command("web-search").unwrap();
    a.send_message("rust news".into()).unwrap();
    assert!(a.messages.iter().any(|m| m.role == "skill"));
    let persisted = a.db.load_messages(&a.session.as_ref().unwrap().id).unwrap();
    assert!(!persisted.iter().any(|m| m.role == "skill"));
}

#[test]
fn split_inline_reasoning_strips_think_tags() {
    let (content, reasoning) = split_inline_reasoning("plain answer, no tags");
    assert_eq!(content, "plain answer, no tags");
    assert_eq!(reasoning, None);

    let (content, reasoning) =
        split_inline_reasoning("<think>let me work this out</think>the actual answer");
    assert_eq!(content, "the actual answer");
    assert_eq!(reasoning.as_deref(), Some("let me work this out"));

    // Multiple blocks join; text around/between them stays in content.
    let (content, reasoning) =
        split_inline_reasoning("intro <think>step one</think>middle<think>step two</think> outro");
    assert_eq!(content, "intro middle outro");
    assert_eq!(reasoning.as_deref(), Some("step one\nstep two"));

    // Unterminated tag (truncated stream): remainder is reasoning, not a
    // dangling tag leaked into the answer.
    let (content, reasoning) = split_inline_reasoning("<think>still thinking...");
    assert_eq!(content, "");
    assert_eq!(reasoning.as_deref(), Some("still thinking..."));
}

#[tokio::test]
async fn finish_stream_strips_inline_think_tags_into_reasoning() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.execute(AppCommand::Send {
        text: "hi".to_string(),
    })
    .unwrap();
    a.on_stream_event(StreamEvent::Token("<think>pondering</think>".into()))
        .unwrap();
    a.on_stream_event(StreamEvent::Token("the real answer".into()))
        .unwrap();
    a.on_stream_event(StreamEvent::Done).unwrap();

    let msg = a.messages.last().unwrap();
    assert_eq!(msg.content, "the real answer");
    assert_eq!(msg.reasoning.as_deref(), Some("pondering"));

    // (copy_message's reasoning-strip behavior moved to the TUI copy tests)
}

#[test]
fn parse_memory_ops_reads_add_update_delete() {
    let ops = parse_memory_ops(
        r#"sure, here:
        [{"op":"add","text":"likes rust"}, {"op":"update","id":2,"text":"deadline moved"}, {"op":"delete","id":5}, {"bogus":true}]"#,
    );
    assert_eq!(ops.len(), 3);
    assert!(matches!(&ops[0], MemoryOp::Add(t) if t == "likes rust"));
    assert!(matches!(&ops[1], MemoryOp::Update(2, t) if t == "deadline moved"));
    assert!(matches!(&ops[2], MemoryOp::Delete(5)));
    assert!(parse_memory_ops("no json here").is_empty());
}

#[test]
fn memory_ops_apply_and_renumber() {
    let mut a = app_with_key();
    let space = a.active_space.name.clone();
    std::fs::write(a_memory_path(&a), "1. likes rust\n2. old fact\n").unwrap();

    let ops = vec![
        MemoryOp::Delete(1),                        // drop "likes rust"
        MemoryOp::Update(2, "updated fact".into()), // "old fact" -> "updated fact"
        MemoryOp::Add("new fact".into()),
    ];
    a.on_memory_result(Some((space, ops)));

    let saved = std::fs::read_to_string(a_memory_path(&a)).unwrap();
    assert_eq!(saved, "1. updated fact\n2. new fact\n");
}

#[test]
fn memory_ops_dropped_if_space_switched_meanwhile() {
    let mut a = app_with_key();
    std::fs::write(a_memory_path(&a), "1. keep me\n").unwrap();
    a.on_memory_result(Some(("some-other-space".into(), vec![MemoryOp::Delete(1)])));
    assert_eq!(
        std::fs::read_to_string(a_memory_path(&a)).unwrap(),
        "1. keep me\n"
    );
}

fn a_memory_path(a: &App) -> std::path::PathBuf {
    a.space.ensure_space_dir(&a.active_space.name).unwrap();
    a.space.memory_path(&a.active_space.name)
}

#[tokio::test]
async fn history_carries_markdown_images_as_data_urls_for_vision_models() {
    let mut a = app_with_key();
    let s =
        a.db.create_session("t", "vis/model", &a.active_space.id, "chat")
            .unwrap();
    // A real tiny png on disk, referenced via markdown in content.
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    let png_path = dir.join("t.png");
    std::fs::write(
        &png_path,
        crate::app::transcribe::encode_png(1, 1, &[0, 0, 0, 255]).unwrap(),
    )
    .unwrap();
    let content = "what is ![this](t.png)?";
    a.db.add_user_message(&s.id, content).unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();
    a.models = vec![
        Model {
            id: "vis/model".into(),
            name: "v".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: true,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
        Model {
            id: "txt/model".into(),
            name: "t".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        },
    ];

    a.current_model = Some("vis/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert_eq!(user.images.len(), 1);
    assert!(user.images[0].starts_with("data:image/png;base64,"));

    // Non-vision model: markdown stays as text, no data URLs.
    a.current_model = Some("txt/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert!(user.images.is_empty());
    assert!(user.content.contains("![this](t.png)"));
}

#[test]
fn tool_call_events_persist_and_replay_into_history() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    a.session = Some(s.clone());
    a.on_stream_event(crate::provider::StreamEvent::ToolCall {
        id: "call_search".into(),
        reasoning: None,
        assistant_content: None,
        name: "search_files".into(),
        arguments: r#"{"query":"q3 revenue"}"#.into(),
        result: "report.pdf (page 3): revenue grew".into(),
    })
    .unwrap();

    // Rendered + persisted as a tool_call row…
    assert_eq!(a.messages.last().unwrap().role, "tool_call");
    let stored = a.db.load_messages(&s.id).unwrap();
    assert_eq!(stored.last().unwrap().role, "tool_call");
    assert!(stored.last().unwrap().content.contains("q3 revenue"));

    // …and replayed into the next request as a real assistant/tool pair, so
    // the model remembers what it already tried in a prior turn — no raw
    // "tool_call" role ever reaches the wire.
    let h = a.build_history();
    assert!(h.iter().all(|m| m.role != "tool_call"));
    let assistant = h
        .iter()
        .find(|m| m.role == "assistant" && m.tool_calls.is_some())
        .expect("assistant tool_calls message");
    let call = &assistant.tool_calls.as_ref().unwrap()[0];
    assert_eq!(call.name, "search_files");
    assert!(call.arguments.contains("q3 revenue"));
    let tool_msg = h
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id == Some(call.id.clone()))
        .expect("matching tool result message");
    assert!(tool_msg.content.contains("revenue grew"));
}

#[test]
fn parallel_tool_call_rows_replay_as_one_lossless_assistant_batch() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    a.session = Some(s);

    for (id, name, result, reasoning) in [
        ("call_a", "search", "first result", Some("think once")),
        ("call_b", "read_file", "second result", None),
    ] {
        a.on_stream_event(crate::provider::StreamEvent::ToolCall {
            id: id.into(),
            reasoning: reasoning.map(str::to_string),
            assistant_content: (id == "call_a").then(|| "visible fragment".to_string()),
            name: name.into(),
            arguments: "{}".into(),
            result: result.into(),
        })
        .unwrap();
    }

    let history = a.build_history();
    let assistant = history
        .iter()
        .find(|m| m.role == "assistant" && m.tool_calls.is_some())
        .expect("parallel assistant batch");
    let calls = assistant.tool_calls.as_ref().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[1].id, "call_b");
    assert_eq!(assistant.reasoning_content.as_deref(), Some("think once"));
    assert_eq!(assistant.content, "visible fragment");
    assert_eq!(
        history
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.tool_call_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("call_a"), Some("call_b")]
    );
}

#[test]
fn research_stage_rows_are_never_replayed_into_history() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space, "chat").unwrap();
    a.session = Some(s.clone());
    a.db.add_research_stage_message(&s.id, "planning…").unwrap();
    a.messages = a.db.load_messages(&s.id).unwrap();

    let h = a.build_history();
    assert!(h.iter().all(|m| m.role != "research_stage"));
    assert!(h.iter().all(|m| !m.content.contains("planning…")));
}

#[test]
// One assertion per tool call summary — the table is the point.
#[allow(clippy::too_many_lines)]
fn tool_call_summaries_name_the_interesting_argument() {
    use crate::app::tool_call_summary;
    assert_eq!(
        tool_call_summary("search_files", r#"{"query":"q3"}"#, "a\nb\nc"),
        "search_files \"q3\" → 3 hits"
    );
    assert_eq!(
        tool_call_summary("search_files", r#"{"query":"q3"}"#, "no matches"),
        "search_files \"q3\" → no hits"
    );
    assert_eq!(
        tool_call_summary(
            "read_file",
            r#"{"name":"r.pdf"}"#,
            "r.pdf (lines 1-200 of 831):\n…"
        ),
        "read_file r.pdf → r.pdf (lines 1-200 of 831)"
    );
    assert_eq!(
        tool_call_summary(
            "write_file",
            r#"{"app":"deck","path":"index.html","content":"12345"}"#,
            "wrote"
        ),
        "write_file deck/index.html (5 bytes)"
    );
    assert_eq!(
        tool_call_summary("edit_file", r#"{"app":"a","path":"b.js"}"#, "ok"),
        "edit_file a/b.js"
    );
    assert_eq!(
        tool_call_summary("skill", r#"{"name":"commit"}"#, "…"),
        "skill commit"
    );
    assert_eq!(
        tool_call_summary(
            "install_skill",
            r#"{"source":"anthropics/skills/pdf"}"#,
            "installed skill 'pdf' — load it with the skill tool"
        ),
        "install_skill anthropics/skills/pdf → installed skill 'pdf' — load it with the skill tool"
    );
    assert_eq!(
        tool_call_summary(
            "run_script",
            r#"{"skill":"pdf","script":"scripts/fill.py"}"#,
            "ok"
        ),
        "run_script pdf/scripts/fill.py"
    );
    assert_eq!(
        tool_call_summary(
            "install_packages",
            r#"{"packages":["pillow","requests"],"skill":"pdf"}"#,
            "ok"
        ),
        "install_packages pillow requests → pdf"
    );
    assert_eq!(
        tool_call_summary("skills", r#"{"action":"load","name":"commit"}"#, "…"),
        "skills/load commit"
    );
    assert_eq!(
        tool_call_summary(
            "skills",
            r#"{"action":"install","source":"anthropics/skills/pdf"}"#,
            "installed"
        ),
        "skills/install anthropics/skills/pdf"
    );
    assert_eq!(
        tool_call_summary(
            "scripts",
            r#"{"action":"run","path":"fill.py","space":true}"#,
            "ok"
        ),
        "scripts/run fill.py"
    );
    assert_eq!(
        tool_call_summary(
            "scripts",
            r#"{"action":"python","code":"print(1)","name":"x.py"}"#,
            "ok"
        ),
        "scripts/python x.py"
    );
    assert_eq!(
        tool_call_summary(
            "app",
            r#"{"action":"write","app":"deck","path":"index.html","content":"<h1>hi</h1>"}"#,
            "wrote"
        ),
        "app/write deck"
    );
    assert_eq!(
        tool_call_summary(
            "media",
            r#"{"action":"generate_video","prompt":"a red fox"}"#,
            "ok"
        ),
        "media/generate_video a red fox"
    );
    let long = format!(r#"{{"x":"{}"}}"#, "y".repeat(100));
    assert!(tool_call_summary("mystery", &long, "").ends_with('…'));
}

#[test]
fn swarm_progress_and_errors_are_visible_in_transcript() {
    let mut a = app_with_key();
    let space = a.active_space.id.clone();
    let s =
        a.db.create_session("swarm", "a/one", &space, "chat")
            .unwrap();
    let sid = s.id.clone();
    a.session = Some(s);

    a.on_swarm_update(Some((
        sid.clone(),
        super::swarm::SwarmUpdate::Progress(
            "round 1/4 · persona 1/3 — Skeptic is responding".into(),
        ),
    )));
    assert!(
        a.messages
            .iter()
            .any(|m| { m.role == "research_stage" && m.content.contains("Skeptic is responding") })
    );

    a.on_swarm_update(Some((
        sid.clone(),
        super::swarm::SwarmUpdate::Error("backend failed".into()),
    )));
    assert!(
        a.messages
            .iter()
            .any(|m| { m.role == "error" && m.content.contains("backend failed") })
    );

    // Subsequent progress updates replace only the progress row, not the error.
    a.on_swarm_update(Some((
        sid.clone(),
        super::swarm::SwarmUpdate::Progress("round 1 complete".into()),
    )));
    assert!(
        a.messages
            .iter()
            .any(|m| { m.role == "error" && m.content.contains("backend failed") })
    );
    let stored = a.db.load_messages(&sid).unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.role == "error" && m.content.contains("backend failed"))
    );
}

#[tokio::test]
async fn swarm_synthesis_triggers_post_reply_jobs_like_normal_chat() {
    let mut a = app_with_key();
    a.memory_model.clear(); // isolate this test from memory extraction network work
    a.settings.compact_threshold = 0;
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s =
        a.db.create_session("hello", "a/one", &space, "chat")
            .unwrap();
    let sid = s.id.clone();
    a.session = Some(s);
    a.messages.push(Message {
        role: "user".into(),
        content: "what should we do?".into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });

    a.on_swarm_update(Some((
        sid,
        super::swarm::SwarmUpdate::Synthesis("balance speed and safety".into()),
    )));

    assert_eq!(
        a.messages.last().unwrap().content,
        "balance speed and safety"
    );
    assert!(a.title_rx.is_some());
}

#[test]
fn build_history_skips_persona_round_replies_but_keeps_synthesis() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.messages.push(Message {
        role: "user".into(),
        content: "what should we do?".into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    a.messages.push(Message {
        role: "assistant".into(),
        content: "ship it fast".into(),
        model: Some("a/one".into()),
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: Some("Optimist".into()),
        created_at: None,
    });
    a.messages.push(Message {
        role: "assistant".into(),
        content: "balance speed and safety".into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    let history = a.build_history();
    let contents: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert!(!contents.iter().any(|c| c.contains("ship it fast")));
    assert!(contents.iter().any(|c| c.contains("what should we do?")));
    assert!(
        contents
            .iter()
            .any(|c| c.contains("balance speed and safety"))
    );
}

// ── the command/event seam ─────────────────────────────────────────────────

/// Every catalog entry must parse: the composer advertised `/help` for a
/// while with no parser arm behind it ("unknown command: /help").
#[test]
fn every_catalog_command_parses() {
    let a = app_with_key();
    for c in super::COMMANDS {
        if let Err(e) = a.parse_command(c.name) {
            assert!(!e.contains("unknown command"), "/{}: {e}", c.name);
        }
    }
    assert_eq!(a.parse_command("help").unwrap(), AppCommand::OpenHelp);
}

#[test]
fn interactive_ui_is_advertised_only_by_capable_clients() {
    let mut a = app_with_key();
    a.base_system_prompt = "test baseline".into();
    assert!(!a.system_prompt().contains("nexus-ui"));
    let before = a.context_breakdown().system_tokens;
    a.interactive_ui = true;
    let extra = u64::try_from(crate::interactive::INSTRUCTIONS.chars().count() / 4).unwrap();
    assert!(a.context_breakdown().system_tokens >= before + extra.saturating_sub(1));
    assert!(a.system_prompt().contains("nexus-ui"));
    assert!(a.system_prompt().contains("Prefer interactive UI"));
    assert!(a.parse_command("play rps").is_err());
}

#[test]
fn parse_command_maps_the_slash_catalog_into_the_seam() {
    let a = app_with_key();
    assert_eq!(a.parse_command("new").unwrap(), AppCommand::NewSession);
    assert_eq!(a.parse_command("web").unwrap(), AppCommand::ToggleWeb);
    assert_eq!(
        a.parse_command("theme opaque").unwrap(),
        AppCommand::SetTheme {
            mode: "opaque".into()
        }
    );
    assert_eq!(
        a.parse_command("research! rust async").unwrap(),
        AppCommand::RunResearch {
            topic: "rust async".into(),
            gated: false
        }
    );
    assert_eq!(
        a.parse_command("research").unwrap(),
        AppCommand::RunResearch {
            topic: String::new(),
            gated: true
        }
    );
    assert_eq!(
        a.parse_command("research kernels").unwrap(),
        AppCommand::RunResearch {
            topic: "kernels".into(),
            gated: true
        }
    );
    assert_eq!(
        a.parse_command("image").unwrap(),
        AppCommand::OpenFiles {
            tab: FilesTab::Images
        }
    );
    assert_eq!(
        a.parse_command("watch").unwrap(),
        AppCommand::Watch { topic: None }
    );
    assert_eq!(
        a.parse_command("watch rust async").unwrap(),
        AppCommand::Watch {
            topic: Some("rust async".into())
        }
    );
    // Incognito parses to an absolute on/off (toggle is the string front's
    // choice) — the app starts off, so the command turns it on.
    assert_eq!(
        a.parse_command("incognito").unwrap(),
        AppCommand::Incognito { on: true }
    );
    // `/local` opens the picker; the runtime aliases resolve to `local` and
    // carry themselves through as the spec.
    for (line, spec) in [
        ("local", ""),
        ("local off", "off"),
        ("ollama", "ollama"),
        ("edge0", "edge0"),
        ("mlx-serve", "mlx-serve"),
        (
            "mlx http://localhost:8080/v1",
            "mlx http://localhost:8080/v1",
        ),
    ] {
        assert_eq!(
            a.parse_command(line).unwrap(),
            AppCommand::ConfigureLocal { spec: spec.into() },
            "{line}"
        );
    }
    // Server verbs split off from runtime selection, in either word order:
    // `/local stop edge0` and `/edge0 stop` are the same command.
    for (line, verb, runtime) in [
        ("local start", "start", ""),
        ("local stop edge0", "stop", "edge0"),
        ("edge0 stop", "stop", "edge0"),
        ("local status", "status", ""),
        ("local restart mlx", "restart", "mlx"),
        ("mlx-serve start", "start", "mlx-serve"),
        // `/serve` is the alias that reads as the verb it is.
        ("serve", "start", ""),
        ("serve ollama", "start", "ollama"),
    ] {
        assert_eq!(
            a.parse_command(line).unwrap(),
            AppCommand::LocalServer {
                verb: verb.into(),
                runtime: runtime.into()
            },
            "{line}"
        );
    }
    assert!(a.parse_command("nosuchcommand").is_err());
    // Aliases resolve to the canonical command.
    assert_eq!(
        a.parse_command("history").unwrap(),
        AppCommand::OpenSessionPicker
    );
}

#[test]
fn set_model_and_setting_commands_apply() {
    let mut a = app_with_key();
    a.execute(AppCommand::SetModel { id: "a/one".into() })
        .unwrap();
    assert_eq!(a.current_model.as_deref(), Some("a/one"));
    a.execute(AppCommand::SetSetting {
        key: "verbosity".into(),
        value: "caveman".into(),
    })
    .unwrap();
    assert_eq!(a.verbosity, "caveman");
    assert!(
        a.db.load_settings()
            .unwrap()
            .iter()
            .any(|(k, v)| k == "verbosity" && v == "caveman")
    );
}

#[test]
fn set_setting_fails_fast_on_unknown_keys_and_invalid_values() {
    let mut a = app_with_key();
    // Unknown keys bail instead of persisting a no-op as success.
    assert!(
        a.execute(AppCommand::SetSetting {
            key: "verbosityy".into(),
            value: "caveman".into(),
        })
        .is_err()
    );
    // Constrained keys reject values `apply_setting` would drop silently.
    assert!(
        a.execute(AppCommand::SetSetting {
            key: "verbosity".into(),
            value: "shouty".into(),
        })
        .is_err()
    );
    assert!(
        a.execute(AppCommand::SetSetting {
            key: "temperature".into(),
            value: "hot".into(),
        })
        .is_err()
    );
    assert_eq!(a.verbosity, "concise"); // untouched by the rejected values
}

#[tokio::test]
async fn gate_in_another_session_never_swallows_typing_and_the_event_carries_its_session() {
    let mut a = app_with_key();
    let space = a.active_space.id.clone();
    let s_a =
        a.db.create_session("research A", "a/one", &space, "research")
            .unwrap();
    let s_b =
        a.db.create_session("chat B", "a/one", &space, "chat")
            .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    // App::new queues a launch status event; drain it so the queue holds
    // exactly the gate event.
    while a.pop_pending_event().is_some() {}
    a.set_survey_gate(Some(SurveyGate {
        session_id: s_a.id.clone(),
        reply_tx: tx,
        phase: SurveyPhase::Clarify { round: 1 },
        prompt_role: "survey".to_string(),
        prompt_content: "q1?".to_string(),
    }));
    // The Gate event names the session the reply must come from.
    match a.pending_events.pop_front() {
        Some(AppEvent::Gate(Some(g))) => assert_eq!(g.session_id, s_a.id),
        _other => panic!("expected Gate(Some) event, got something else"),
    }
    // View the other session: Enter must send a normal message, not answer
    // A's parked gate.
    a.switch_to_session_by_id(&s_b.id).unwrap();
    assert!(!a.survey_gate_targets_current_session());
    a.execute(AppCommand::Send {
        text: "hello B".to_string(),
    })
    .unwrap();
    let msg = a.messages.last().unwrap();
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content, "hello B");
    // The gate survives, still parked on A.
    assert_eq!(a.survey_gate.as_ref().unwrap().session_id, s_a.id);
}

#[test]
fn snapshot_serializes_sessions_models_settings_and_tasks() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let snap = a.snapshot().unwrap();
    let json = serde_json::to_string(&snap).unwrap();
    assert!(json.contains("\"sessions\""));
    assert!(json.contains("\"models\""));
    assert!(json.contains("\"settings\""));
    assert!(json.contains("\"tasks\""));
    assert!(json.contains("\"model\":\"a/one\""));
}

#[test]
fn cache_breakpoints_only_go_to_models_that_need_them() {
    assert!(crate::app::model_needs_cache_breakpoints(
        "anthropic/claude-sonnet-4"
    ));
    assert!(crate::app::model_needs_cache_breakpoints(
        "openrouter:anthropic/claude-opus-4.1"
    ));
    assert!(crate::app::model_needs_cache_breakpoints(
        "claude-3-5-haiku"
    ));
    // Auto-caching families must not be sent a marker they ignore or reject.
    for model in [
        "openai/gpt-5",
        "openai:gpt-5-mini",
        "deepseek/deepseek-v3",
        "google/gemini-2.5-pro",
    ] {
        assert!(
            !crate::app::model_needs_cache_breakpoints(model),
            "{model} caches automatically"
        );
    }
}

#[test]
fn explicit_cache_breakpoints_mark_the_stable_prefix() {
    let mut a = app_with_key();
    a.current_model = Some("anthropic/claude-sonnet-4".to_string());
    let message = |role: &str, content: &str| Message {
        role: role.into(),
        content: content.into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    };
    a.messages = vec![
        message("user", "first"),
        message("assistant", "answer"),
        message("user", "second"),
    ];
    let history = a.build_history();
    let marked: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| m.cache_breakpoint)
        .map(|(i, _)| i)
        .collect();
    // End of the system block, and the message before the live tail.
    assert_eq!(marked, vec![0, history.len() - 2], "history: {history:?}");
    let wire = serde_json::to_value(&history[0]).expect("serializes");
    assert_eq!(
        wire["content"][0]["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );

    // The same conversation on an auto-caching model carries no markers, and
    // its content stays a plain string.
    a.current_model = Some("openai/gpt-5".to_string());
    let history = a.build_history();
    assert!(history.iter().all(|m| !m.cache_breakpoint));
    let wire = serde_json::to_value(&history[0]).expect("serializes");
    assert!(wire["content"].is_string(), "wire: {wire}");
}

#[test]
fn turn_cache_reports_the_turn_not_its_last_request() {
    use crate::app::CacheTally;
    use crate::provider::{PromptConvention, Usage};
    let mut tally = CacheTally::default();
    // A tool loop: the first request caches nothing, later ones ride a
    // growing prefix. The last request alone would claim 90%.
    for (prompt, read) in [(1000u64, 0u64), (1200, 900), (1400, 1260)] {
        tally.observe(&Usage {
            prompt_tokens: prompt,
            cache_read_tokens: read,
            prompt_convention: PromptConvention::Inclusive,
            ..Default::default()
        });
    }
    let rate = tally.rate().expect("rateable turn");
    assert!((rate - 2160.0 / 3600.0).abs() < 1e-9, "rate: {rate}");
    assert!(!tally.is_partial());

    // A request the provider did not account for is excluded and flagged,
    // never averaged in as a zero.
    tally.observe(&Usage {
        prompt_tokens: 5000,
        prompt_convention: PromptConvention::Unknown,
        ..Default::default()
    });
    assert!(tally.is_partial());
    assert_eq!(tally.unrated_requests, 1);
    let after = tally.rate().expect("still rateable");
    assert!(
        (after - 2160.0 / 3600.0).abs() < 1e-9,
        "rate moved: {after}"
    );
}

fn user_command_app() -> App {
    let mut app = app_with_key();
    app.interactive_ui = true;
    app
}

fn insert_chat_task(app: &mut App, session_id: &str) {
    let abort = tokio::spawn(async {}).abort_handle();
    app.chat_tasks.insert(
        1,
        ChatTask {
            id: 1,
            session_id: session_id.to_string(),
            session_title: "chat".into(),
            space_id: app.active_space.id.clone(),
            model: "a/one".into(),
            model_id: "a/one".into(),
            backend: BackendTag::OpenRouter,
            incognito: app.incognito,
            buffer: String::new(),
            thinking: String::new(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            abort,
        },
    );
}

/// Drive `next_event` until the captured command lands, returning the last status.
async fn finish_captured_command(app: &mut App) -> String {
    let mut status = String::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), app.next_event())
            .await
            .unwrap();
        match event {
            AppEvent::Status(text) => status = text,
            AppEvent::HistoryInvalidated => break,
            _ => {}
        }
    }
    while let Some(event) = app.pop_pending_event() {
        if let AppEvent::Status(text) = event {
            status = text;
        }
    }
    status
}

#[tokio::test]
async fn captured_user_command_stores_its_output_and_replays_as_a_user_message() {
    let mut app = user_command_app();
    let root = app.space.root.clone();
    app.execute(AppCommand::RunUserCommand {
        cmd: "echo hello; echo oops >&2; exit 3".into(),
    })
    .unwrap();
    assert_eq!(
        app.last_status(),
        "running: echo hello; echo oops >&2; exit 3"
    );
    assert_eq!(
        finish_captured_command(&mut app).await,
        "ran: echo hello; echo oops >&2; exit 3 · exit 3"
    );
    let row = app.messages.last().unwrap();
    assert_eq!(row.role, "user_command");
    let content: serde_json::Value = serde_json::from_str(&row.content).unwrap();
    let cwd = app.user_command_cwd().display().to_string();
    assert_eq!(content["cmd"], "echo hello; echo oops >&2; exit 3");
    assert_eq!(content["cwd"], cwd.as_str());
    assert_eq!(content["exit"], 3);
    assert_eq!(content["output"], "hello\noops\n");
    assert_eq!(content["interactive"], false);
    let session = app.session.as_ref().unwrap().id.clone();
    assert_eq!(
        app.db.load_messages(&session).unwrap().last().unwrap().role,
        "user_command"
    );
    let history = app.build_history();
    let replay = history.last().unwrap();
    assert_eq!(replay.role, "user");
    assert_eq!(
        replay.content,
        format!(
            "I ran `echo hello; echo oops >&2; exit 3` in {cwd} (exit 3):\n```\nhello\noops\n```"
        )
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn captured_user_command_scrubs_saved_secrets() {
    let mut app = user_command_app();
    let root = app.space.root.clone();
    app.saved.host_token = Some("hosttoken-0123456789abcdef".into());
    app.execute(AppCommand::RunUserCommand {
        cmd: "printf 'token=hosttoken-%s\\n' 0123456789abcdef".into(),
    })
    .unwrap();
    finish_captured_command(&mut app).await;
    let content: serde_json::Value =
        serde_json::from_str(&app.messages.last().unwrap().content).unwrap();
    assert_eq!(content["output"], "token=[REDACTED]\n");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn user_commands_are_refused_headless_and_while_a_reply_streams() {
    let mut app = app_with_key();
    let root = app.space.root.clone();
    for command in [
        AppCommand::RunUserCommand { cmd: "ls".into() },
        AppCommand::RecordUserCommand {
            cmd: "sudo true".into(),
            exit: Some(0),
            started: true,
        },
    ] {
        app.execute(command).unwrap();
        assert_eq!(app.last_status(), "!commands run only in the terminal UI");
    }
    assert!(app.session.is_none());
    assert!(app.user_command_rx.is_none());
    app.interactive_ui = true;
    let session = app
        .db
        .create_session("chat", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session.clone());
    insert_chat_task(&mut app, &session.id);
    app.execute(AppCommand::RunUserCommand { cmd: "ls".into() })
        .unwrap();
    let (sets, status) = app.drain_ui_events();
    assert_eq!(status, "wait — response still streaming");
    assert_eq!(sets, vec!["!ls".to_string()]);
    assert!(app.user_command_rx.is_none());
    assert!(app.messages.is_empty());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn incognito_user_commands_stay_in_memory() {
    let mut app = user_command_app();
    let root = app.space.root.clone();
    app.incognito = true;
    app.execute(AppCommand::RunUserCommand {
        cmd: "echo private".into(),
    })
    .unwrap();
    finish_captured_command(&mut app).await;
    app.execute(AppCommand::RecordUserCommand {
        cmd: "ssh host".into(),
        exit: Some(0),
        started: true,
    })
    .unwrap();
    let session = app.session.as_ref().unwrap().id.clone();
    assert_eq!(
        app.messages
            .iter()
            .filter(|message| message.role == "user_command")
            .count(),
        2
    );
    assert!(app.db.load_messages(&session).unwrap().is_empty());
    assert!(app.db.get_session(&session).unwrap().is_none());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn recorded_interactive_command_keeps_only_its_command_and_exit_code() {
    let mut app = user_command_app();
    let root = app.space.root.clone();
    app.execute(AppCommand::RecordUserCommand {
        cmd: "sudo make install".into(),
        exit: Some(1),
        started: true,
    })
    .unwrap();
    assert_eq!(app.last_status(), "ran: sudo make install · exit 1");
    let row = app.messages.last().unwrap();
    assert_eq!(row.role, "user_command");
    let content: serde_json::Value = serde_json::from_str(&row.content).unwrap();
    assert_eq!(content["exit"], 1);
    assert_eq!(content["interactive"], true);
    assert_eq!(
        content["output"],
        "(interactive — output not captured; paste anything relevant)"
    );
    let session = app.session.as_ref().unwrap().id.clone();
    assert_eq!(app.db.load_messages(&session).unwrap().len(), 1);
    let replay = app.build_history().pop().unwrap();
    assert_eq!(replay.role, "user");
    assert!(
        replay.content.starts_with("I ran `sudo make install` in "),
        "{}",
        replay.content
    );
    assert!(
        replay
            .content
            .ends_with("(exit 1):\n(interactive — output not captured; paste anything relevant)"),
        "{}",
        replay.content
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn propose_command_tool_call_sets_the_proposal_and_its_reason() {
    let mut app = user_command_app();
    let root = app.space.root.clone();
    let session = app
        .db
        .create_session("chat", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session.clone());
    insert_chat_task(&mut app, &session.id);
    while app.pop_pending_event().is_some() {}
    app.on_chat_event(
        1,
        StreamEvent::ToolCall {
            id: "call_1".into(),
            reasoning: None,
            assistant_content: None,
            name: "propose_command".into(),
            arguments: r#"{"cmd":"sudo make install","reason":"installs into /usr/local"}"#
                .into(),
            result: "proposed to the user: !sudo make install · wait for them to run it; its output will appear in the conversation".into(),
        },
    )
    .unwrap();
    assert_eq!(app.proposed_command.as_deref(), Some("!sudo make install"));
    assert_eq!(app.last_status(), "installs into /usr/local");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn request_workspace_switches_to_agent_mode_only_after_an_approved_turn() {
    for decision in [Decision::Allow, Decision::Deny] {
        let mut app = app_with_key();
        let root = app.space.root.clone();
        let workspace = test_space().root;
        std::fs::create_dir_all(&workspace).unwrap();
        app.interactive_approvals = true;
        app.refresh_toolbox();
        assert!(
            app.toolbox
                .defs()
                .iter()
                .any(|def| def.name == "request_workspace")
        );
        let session = app
            .db
            .create_session("chat", "a/one", &app.active_space.id, "chat")
            .unwrap();
        app.session = Some(session.clone());
        insert_chat_task(&mut app, &session.id);
        while app.pop_pending_event().is_some() {}
        let arguments = serde_json::json!({
            "root": workspace.display().to_string(),
            "reason": "to run the tests",
        })
        .to_string();
        let toolbox = Arc::clone(&app.toolbox);
        let call_arguments = arguments.clone();
        let call =
            tokio::spawn(async move { toolbox.run("request_workspace", &call_arguments).await });
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), app.next_event())
            .await
            .unwrap();
        assert!(
            matches!(event, AppEvent::Status(status) if status == "approval needed · request_workspace agent")
        );
        assert_eq!(
            app.pending_approval_request.as_ref().unwrap().summary,
            format!(
                "switch this session to agent mode in {}\nto run the tests",
                workspace.display()
            )
        );
        app.execute(AppCommand::Approve { decision }).unwrap();
        let (result, _) = call.await.unwrap();
        app.on_chat_event(
            1,
            StreamEvent::ToolCall {
                id: "call_1".into(),
                reasoning: None,
                assistant_content: None,
                name: "request_workspace".into(),
                arguments,
                result,
            },
        )
        .unwrap();
        assert_eq!(app.work_mode, WorkMode::Chat, "never mid-turn");
        app.on_chat_event(1, StreamEvent::Done).unwrap();
        if decision == Decision::Allow {
            assert_eq!(app.work_mode, WorkMode::Agent);
            assert_eq!(
                app.workspace.as_ref().unwrap().root(),
                workspace.canonicalize().unwrap()
            );
        } else {
            assert_eq!(app.work_mode, WorkMode::Chat);
            assert!(app.workspace.is_none());
        }
        assert!(app.queued_workspace.is_none());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(workspace).unwrap();
    }
}

#[tokio::test]
async fn retry_keeps_user_command_rows_and_replaces_only_the_reply_after_them() {
    let mut a = app_with_reply();
    let sid = a.session.as_ref().unwrap().id.clone();
    let row = r#"{"cmd":"cargo test","cwd":"/w","exit":101,"started":true,"output":"failed","interactive":false}"#;
    a.db.add_user_command_message(&sid, row).unwrap();
    a.messages = a.db.load_messages(&sid).unwrap();
    a.run_command("retry").unwrap();
    assert_eq!(
        a.last_status(),
        "nothing to retry — no reply after the last user message"
    );
    assert!(!a.is_streaming());
    assert_eq!(a.db.load_messages(&sid).unwrap().len(), 3);

    a.db.add_assistant_message(&sid, "fixed it", None, None, None, None, None, None)
        .unwrap();
    a.messages = a.db.load_messages(&sid).unwrap();
    a.run_command("retry").unwrap();
    let stored = a.db.load_messages(&sid).unwrap();
    for messages in [&a.messages, &stored] {
        let roles: Vec<_> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "user_command"]);
    }
    assert!(a.is_streaming());
    assert!(
        a.build_history()
            .last()
            .unwrap()
            .content
            .starts_with("I ran `cargo test` in /w (exit 101):")
    );
    a.stop_stream().unwrap();
}
