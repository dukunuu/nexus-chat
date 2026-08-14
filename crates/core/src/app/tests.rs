use super::*;

/// A throwaway space dir under the OS temp dir, unique per test.
fn test_space() -> Space {
    Space {
        root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())),
    }
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
    // The composer restore is an event now (2e) — the view would re-apply it.
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
    assert_eq!(a.context_used(), 10);
}

#[test]
fn compaction_narrows_effective_messages_and_context_used() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from installed skills
    a.base_system_prompt = String::new(); // isolate from the base system prompt
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
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
    // 20 (summary) + 2*10 (tail) = 40 tokens; the two compacted-away
    // messages must NOT be counted.
    assert_eq!(a.context_used(), 40);
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

// ── Phase 2c: the command/event seam ─────────────────────────────────────

#[test]
fn parse_command_maps_the_slash_catalog_into_the_seam() {
    let a = app_with_key();
    assert_eq!(a.parse_command("new").unwrap(), AppCommand::NewSession);
    assert_eq!(a.parse_command("web").unwrap(), AppCommand::ToggleWeb);
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
