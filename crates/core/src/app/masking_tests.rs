use super::*;
use crate::provider::{ChatMessage, ToolCall, seed_tool_result_dedup};

const ARGS: &str = r#"{"action":"read","path":"file.txt"}"#;

fn message(role: &str, content: &str) -> Message {
    Message {
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
    }
}

fn tool(id: &str, name: &str, result: &str) -> Message {
    message(
        "tool_call",
        &serde_json::json!({
            "id": id, "name": name, "arguments": ARGS, "result": result,
            "reasoning_content": "Check the evidence", "assistant_content": "Checking now",
        })
        .to_string(),
    )
}

fn app(db: Db, root: std::path::PathBuf) -> App {
    let mut app = App::new(db, None, Space { root });
    app.memory_model.clear();
    app.settings.compact_threshold = 0;
    app.models = vec![Model {
        id: "a/one".into(),
        name: "One".into(),
        reasoning_efforts: Vec::new(),
        context_length: Some(100_000),
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: BackendTag::OpenRouter,
        pricing: None,
    }];
    app.current_model = Some("a/one".into());
    app
}

fn fixture(rounds: usize, result_chars: usize) -> App {
    let root = std::env::temp_dir().join(format!("nexus-masking-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut app = app(Db::open_in_memory().unwrap(), root);
    app.session = Some(
        app.db
            .create_session("masking", "a/one", &app.active_space.id, "chat")
            .unwrap(),
    );
    for round in 0..rounds {
        add_round(&mut app, round, result_chars);
    }
    app
}

fn add_round(app: &mut App, round: usize, result_chars: usize) {
    app.messages
        .push(message("user", &format!("question {round}")));
    for call in 0..2 {
        app.messages.push(tool(
            &format!("c{round}-{call}"),
            "fs",
            &"x".repeat(result_chars),
        ));
    }
    app.messages.push(message("assistant", "answer"));
}

fn discard(app: App) {
    let root = app.space.root.clone();
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn experiment_off_replays_long_history_byte_identically_to_legacy_output() {
    let mut app = fixture(14, 5_000);
    assert!(!app.experiments.mask_tool_results);
    // Even a boundary left by an earlier enabled run must be inert when off.
    app.mask_through = 40;
    let mut expected = vec![ChatMessage::text("system", app.system_prompt())];
    for round in 0..14 {
        expected.push(ChatMessage::text("user", format!("question {round}")));
        let mut assistant = ChatMessage::text("assistant", "Checking now");
        assistant.reasoning_content = Some("Check the evidence".into());
        assistant.tool_calls = Some(
            (0..2)
                .map(|call| ToolCall {
                    id: format!("c{round}-{call}"),
                    name: "fs".into(),
                    arguments: ARGS.into(),
                })
                .collect(),
        );
        expected.push(assistant);
        for call in 0..2 {
            // Pre-masking replay keeps the first full result and dedups all
            // later byte-identical results with the same tool and arguments.
            let content = if round == 0 && call == 0 {
                "x".repeat(5_000)
            } else {
                crate::tools::tool_result_unchanged_note("fs", ARGS)
            };
            let mut result = ChatMessage::text("tool", content);
            result.tool_call_id = Some(format!("c{round}-{call}"));
            expected.push(result);
        }
        expected.push(ChatMessage::text("assistant", "answer"));
    }
    assert_eq!(
        serde_json::to_vec(&app.build_history()).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    let epoch = app.cache_epoch;
    app.context_total = Some(100_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 40);
    assert_eq!(app.cache_epoch, epoch);
    discard(app);
}

#[test]
fn mask_trigger_moves_in_bulk_retains_whole_rounds_and_bumps_epoch_once() {
    let mut app = fixture(4, 5_000);
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 2;
    app.experiments.mask_trigger_tokens = 50_000;
    let epoch = app.cache_epoch;
    app.drain_ui_events();
    app.context_total = Some(49_999);
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    assert_eq!(app.cache_epoch, epoch);
    assert!(app.last_status().is_empty());

    app.context_total = Some(50_000);
    app.maybe_mask();
    // Each round has user + two consecutive tool rows + assistant.
    assert_eq!(app.mask_through, 9);
    assert_eq!(app.cache_epoch, epoch + 1);
    assert!(app.context_total.is_none());
    assert_eq!(
        app.last_status(),
        "masked 4 old tool results (~5000 tokens) · experiment"
    );
    assert_eq!(app.db.session_mask(&app.session.as_ref().unwrap().id), 9);
    let bytes = serde_json::to_vec(&app.build_history()).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&app.build_history()).unwrap());
    assert_eq!(app.cache_epoch, epoch + 1);

    add_round(&mut app, 4, 5_000);
    assert!(app.context_used() < 50_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 9);
    assert_eq!(app.cache_epoch, epoch + 1);
    assert!(app.last_status().is_empty());
    app.context_total = Some(50_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 13);
    assert_eq!(app.cache_epoch, epoch + 2);
    assert_eq!(
        app.last_status(),
        "masked 2 old tool results (~2500 tokens) · experiment"
    );
    app.context_total = Some(50_000);
    app.maybe_mask();
    assert_eq!(app.cache_epoch, epoch + 2);
    assert!(app.last_status().is_empty());
    discard(app);
}

#[test]
fn mask_trigger_uses_half_the_window_and_requires_two_thousand_maskable_tokens() {
    let mut app = fixture(2, 3_999);
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 1;
    app.models[0].context_length = None;
    app.context_total = Some(100_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    app.models[0].context_length = Some(100_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 0); // 7,998 chars / 4 < 2,000
    app.messages[1] = tool("c0-0", "fs", &"x".repeat(4_001));
    app.context_total = Some(49_999);
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    app.context_total = Some(50_000);
    app.maybe_mask();
    assert_eq!(app.mask_through, 5); // exactly 8,000 chars
    discard(app);
}

#[test]
fn errors_and_compacted_observations_do_not_count_toward_the_mask_budget() {
    let mut app = fixture(2, 5_000);
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 1;
    app.experiments.mask_trigger_tokens = 1;
    app.messages[1] = tool(
        "c0-0",
        "fs",
        &format!("write failed: {}", "x".repeat(8_000)),
    );
    let epoch = app.cache_epoch;
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    assert_eq!(app.cache_epoch, epoch);
    app.messages[1] = tool("c0-0", "fs", &"x".repeat(5_000));
    app.session.as_mut().unwrap().compact_through = 4;
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    app.session.as_mut().unwrap().compact_through = 0;
    app.experiments.mask_keep_rounds = 3;
    app.maybe_mask();
    assert_eq!(app.mask_through, 0); // fewer than K rounds
    discard(app);
}

#[test]
fn masked_replay_preserves_calls_reasoning_errors_and_non_tool_messages() {
    let mut app = fixture(0, 0);
    app.messages = vec![
        message("user", "question"),
        message("skill", "review\nKeep evidence"),
        message("user_command", "legacy command output"),
        tool("old", "fs", "file contents"),
        tool("forgotten", "search", "old search hits"),
        tool("error", "shell", "write failed: denied\nmore details"),
        message("assistant", "old answer"),
        message("user", "read again"),
        tool("fresh", "fs", "file contents"),
        tool("duplicate", "fs", "file contents"),
    ];
    let unmasked = app.build_history();
    app.experiments.mask_tool_results = true;
    app.mask_through = 6;
    let history = app.build_history();
    // All non-tool wire messages, including the complete assistant call batch,
    // must be byte-identical; only observation contents change.
    let non_tools = |messages: &[ChatMessage]| {
        serde_json::to_vec(
            &messages
                .iter()
                .filter(|m| m.role != "tool")
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    assert_eq!(non_tools(&history), non_tools(&unmasked));
    let results: Vec<_> = history.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(
        results[0].content,
        "[result cleared to save context — call fs again if you need it]"
    );
    assert_eq!(
        results[1].content,
        crate::tools::tool_result_masked_note("search", ARGS)
    );
    assert_eq!(results[2].content, "write failed: denied\nmore details");
    assert_eq!(results[3].content, "file contents");
    assert_eq!(
        results[4].content,
        crate::tools::tool_result_unchanged_note("fs", ARGS)
    );
    assert_eq!(results[0].tool_call_id.as_deref(), Some("old"));
    let seeded = seed_tool_result_dedup(&history);
    assert_eq!(seeded.len(), 2);
    assert_eq!(seeded[&("fs".into(), ARGS.into())], "file contents");
    assert_eq!(seeded[&("shell".into(), ARGS.into())], results[2].content);
    assert!(!seeded.contains_key(&("search".into(), ARGS.into())));
    let prefix = &history[..history
        .iter()
        .position(|m| m.content == "old answer")
        .unwrap()];
    assert!(!seed_tool_result_dedup(prefix).contains_key(&("fs".into(), ARGS.into())));
    assert_eq!(
        serde_json::to_vec(&history).unwrap(),
        serde_json::to_vec(&app.build_history()).unwrap()
    );
    discard(app);
}

#[test]
fn masking_uses_session_indices_after_compaction_and_counts_masked_size() {
    let mut app = fixture(3, 5_000);
    app.experiments.mask_tool_results = true;
    app.session.as_mut().unwrap().compact_through = 4;
    app.session.as_mut().unwrap().compact_summary = Some("digest".into());
    app.mask_through = 9;
    let history = app.build_history();
    let results: Vec<_> = history.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(results.len(), 4);
    assert!(
        results[0]
            .content
            .starts_with(crate::tools::TOOL_RESULT_MASKED_PREFIX)
    );
    assert!(
        results[1]
            .content
            .starts_with(crate::tools::TOOL_RESULT_MASKED_PREFIX)
    );
    assert_eq!(results[2].content, "x".repeat(5_000));
    let masked_tokens = app.context_breakdown().conversation_tokens;
    let masked_used = app.context_used();
    app.experiments.mask_tool_results = false;
    let full_tokens = app.context_breakdown().conversation_tokens;
    let full_used = app.context_used();
    let note_chars = crate::tools::tool_result_masked_note("fs", ARGS)
        .chars()
        .count();
    let saved_tokens = u64::try_from((2 * (5_000 - note_chars)) / 4).unwrap();
    assert!((saved_tokens..=saved_tokens + 1).contains(&(full_tokens - masked_tokens)));
    assert!((saved_tokens..=saved_tokens + 1).contains(&(full_used - masked_used)));
    assert!(history.iter().any(|m| m.content.ends_with("digest")));
    discard(app);
}

#[test]
fn masked_tool_images_are_not_resent() {
    let mut app = fixture(0, 0);
    app.models[0].supports_images = true;
    app.refresh_toolbox();
    let files = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(&files).unwrap();
    image::RgbaImage::new(1, 1)
        .save(files.join("image.png"))
        .unwrap();
    app.messages = vec![tool("image", "fs", "![evidence](image.png)")];
    assert_eq!(
        app.build_history()
            .iter()
            .filter(|m| !m.images.is_empty())
            .count(),
        1
    );
    app.experiments.mask_tool_results = true;
    app.mask_through = 1;
    let history = app.build_history();
    assert_eq!(history.len(), 3); // system + assistant call + cleared tool result
    assert!(history.iter().all(|m| m.images.is_empty()));
    app.messages.push(message("user", "get the image again"));
    app.messages
        .push(tool("fresh-image", "fs", "![evidence](image.png)"));
    assert_eq!(
        app.build_history()
            .iter()
            .filter(|m| !m.images.is_empty())
            .count(),
        1
    );
    discard(app);
}

#[test]
fn mask_boundary_persists_in_cache_across_switches_and_restarts() {
    let root = std::env::temp_dir().join(format!("nexus-mask-db-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.db");
    let mut app = app(Db::open(&path).unwrap(), root.clone());
    let first = app
        .db
        .create_session("first", "a/one", &app.active_space.id, "chat")
        .unwrap();
    let second = app
        .db
        .create_session("second", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(first.clone());
    add_round(&mut app, 0, 5_000);
    add_round(&mut app, 1, 5_000);
    for m in &app.messages {
        app.db
            .insert_message(
                &first.id, &m.role, &m.content, None, None, None, None, None, None,
            )
            .unwrap();
    }
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 1;
    app.experiments.mask_trigger_tokens = 1;
    app.maybe_mask();
    assert_eq!(app.mask_through, 5);
    app.switch_to_session_by_id(&second.id).unwrap();
    assert_eq!(app.mask_through, 0);
    app.switch_to_session_by_id(&first.id).unwrap();
    assert_eq!(app.mask_through, 5);
    let count: i64 = app
        .db
        .conn_for_test()
        .query_row(
            "SELECT count(*) FROM main.sqlite_master WHERE name = 'session_masks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let updated_at: String = app
        .db
        .conn_for_test()
        .query_row(
            "SELECT updated_at FROM cache.session_masks WHERE session_id = ?1",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!updated_at.is_empty());
    drop(app);

    let mut restarted = self::app(Db::open(&path).unwrap(), root);
    restarted.switch_to_session_by_id(&first.id).unwrap();
    assert_eq!(restarted.mask_through, 5);
    restarted.new_session();
    assert_eq!(restarted.mask_through, 0);
    assert_eq!(restarted.db.session_mask(&first.id), 5);
    discard(restarted);
}

#[tokio::test]
async fn reply_completion_masks_incognito_in_memory_only_and_never_mid_turn() {
    let mut app = fixture(2, 5_000);
    app.incognito = true;
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 1;
    app.experiments.mask_trigger_tokens = 1;
    let session = app.session.as_ref().unwrap().clone();
    let task = tokio::spawn(std::future::pending::<()>());
    app.chat_tasks.insert(
        0,
        ChatTask {
            id: 0,
            session_id: session.id.clone(),
            session_title: session.title,
            space_id: app.active_space.id.clone(),
            model: "a/one".into(),
            model_id: "a/one".into(),
            backend: BackendTag::OpenRouter,
            incognito: true,
            buffer: "finished answer".into(),
            thinking: String::new(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            abort: task.abort_handle(),
        },
    );
    let changes = app.db.conn_for_test().total_changes();
    app.maybe_mask();
    assert_eq!(app.mask_through, 0);
    app.on_chat_event(0, StreamEvent::Done).unwrap();
    task.abort();
    assert_eq!(app.mask_through, 5);
    assert_eq!(app.db.conn_for_test().total_changes(), changes);
    assert_eq!(app.db.session_mask(&session.id), 0);
    assert!(app.pending_events.iter().any(|event| {
        matches!(event, AppEvent::Status(status) if status.contains("experiment"))
    }));
    app.toggle_incognito().unwrap();
    assert_eq!(app.mask_through, 0);
    discard(app);
}

#[test]
fn digest_insertion_keeps_the_mask_attached_to_the_same_observations() {
    for backfill in [false, true] {
        let mut app = fixture(3, 5_000);
        app.experiments.mask_tool_results = true;
        app.mask_through = 9;
        let id = app.session.as_ref().unwrap().id.clone();
        app.db.set_session_mask(&id, 9).unwrap();
        if backfill {
            let session = app.session.as_mut().unwrap();
            session.compact_summary = Some("digest".into());
            session.compact_through = 4;
            app.backfill_compaction_row();
        } else {
            app.on_compact_result(Some((id.clone(), "digest".into(), 4, 50)));
        }
        assert_eq!(app.mask_through, 10);
        assert_eq!(app.db.session_mask(&id), 10);
        let history = app.build_history();
        let fresh = history
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2-0"))
            .unwrap();
        assert_eq!(fresh.content, "x".repeat(5_000));
        discard(app);
    }
}

#[test]
fn background_digest_insertion_updates_the_persisted_mask_boundary() {
    let mut app = fixture(3, 5_000);
    let id = app.session.as_ref().unwrap().id.clone();
    app.db.set_session_mask(&id, 9).unwrap();
    app.new_session();
    app.on_compact_result(Some((id.clone(), "digest".into(), 4, 50)));
    assert_eq!(app.mask_through, 0);
    assert_eq!(app.db.session_mask(&id), 10);
    discard(app);
}

#[test]
fn retry_cannot_leave_a_boundary_that_masks_future_observations() {
    let mut app = fixture(1, 5_000);
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 0;
    app.experiments.mask_trigger_tokens = 1;
    app.maybe_mask();
    let id = app.session.as_ref().unwrap().id.clone();
    app.retry_last_reply().unwrap();
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.mask_through, 1);
    assert_eq!(app.db.session_mask(&id), 1);
    app.messages.push(tool("new", "fs", "fresh contents"));
    assert_eq!(
        app.build_history().last().unwrap().content,
        "fresh contents"
    );
    discard(app);
}

#[test]
fn context_estimate_removes_escaped_results_at_their_masked_size() {
    let mut app = fixture(0, 0);
    app.experiments.mask_tool_results = true;
    app.mask_through = 1;
    app.messages
        .push(tool("escaped", "fs", &"\"\\\n".repeat(3_000)));
    let masked = tool(
        "escaped",
        "fs",
        &crate::tools::tool_result_masked_note("fs", ARGS),
    );
    assert_eq!(
        app.context_breakdown().conversation_tokens,
        u64::try_from(masked.content.chars().count() / 4).unwrap()
    );
    discard(app);
}

#[test]
fn zero_retained_rounds_masks_all_old_observations() {
    let mut app = fixture(1, 5_000);
    app.experiments.mask_tool_results = true;
    app.experiments.mask_keep_rounds = 0;
    app.experiments.mask_trigger_tokens = 1;
    app.maybe_mask();
    assert_eq!(app.mask_through, app.messages.len());
    assert!(
        app.build_history()
            .iter()
            .filter(|m| m.role == "tool")
            .all(|m| {
                m.content
                    .starts_with(crate::tools::TOOL_RESULT_MASKED_PREFIX)
            })
    );
    discard(app);
}
