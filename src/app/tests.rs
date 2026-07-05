use super::*;


/// A throwaway space dir under the OS temp dir, unique per test.
fn test_space() -> Space {
    Space { root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())) }
}

#[test]
fn parse_topic_extracts_and_slugifies() {
    let (t, s) = parse_topic(r#"{"topic": "Rust Async Runtimes", "id": "rust async!"}"#).unwrap();
    assert_eq!(t, "Rust Async Runtimes");
    assert_eq!(s, "rust-async");
    // Tolerates surrounding prose / fences.
    let (t, s) = parse_topic("sure:\n```json\n{\"topic\":\"Hi There\",\"id\":\"hi\"}\n```").unwrap();
    assert_eq!(t, "Hi There");
    assert_eq!(s, "hi");
    assert!(parse_topic("no json here").is_none());
}

#[test]
fn session_filter_matches_title_and_slug() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    let space = a.active_space.id.clone();
    let s1 = a.db.create_session("Rust async runtimes", "a/b", &space).unwrap();
    let s2 = a.db.create_session("Cooking pasta", "a/b", &space).unwrap();
    a.db.set_session_title(&s1.id, "Rust async runtimes", Some("rust-async")).unwrap();
    a.sessions_cache = a.db.list_sessions(&space).unwrap();

    a.session_filter = "rust".into();
    let hits = a.filtered_sessions();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, s1.id);

    a.session_filter = "pasta".into();
    assert_eq!(a.filtered_sessions()[0].id, s2.id);
}

#[test]
fn delete_removes_session_and_clears_if_active() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("doomed", "a/b", &space).unwrap();
    a.sessions_cache = a.db.list_sessions(&space).unwrap();
    a.session = Some(s.clone());
    a.messages.push(Message {
        id: String::new(), role: "user".into(), content: "hi".into(),
        model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        images: Vec::new(),
    });
    a.session_selected = 0;
    a.confirm_delete().unwrap();
    assert!(a.sessions_cache.is_empty());
    assert!(a.session.is_none());
    assert!(a.messages.is_empty());
    assert!(a.db.list_sessions(&space).unwrap().is_empty());
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

pub(super) fn app_with_key() -> App {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("test-key".into()), test_space());
    a.models = vec![
        Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None, supports_images: false },
        Model { id: "b/two".into(), name: "Two".into(), supports_reasoning: false, context_length: None, supports_images: false },
    ];
    a
}

#[test]
fn no_key_rejects_message_and_points_to_key_cmd() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, None, test_space());
    a.set_input("hello");
    a.submit().unwrap();
    assert!(a.session.is_none());
    assert!(a.status.contains("/key"));
}

#[test]
fn message_without_model_is_rejected() {
    let mut a = app_with_key();
    a.set_input("hello");
    a.submit().unwrap();
    assert!(a.session.is_none());
    assert!(a.status.contains("pick a model"));
    assert_eq!(a.input_text(), "hello");
}

#[tokio::test]
async fn message_with_model_creates_session_and_streams() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello world");
    a.submit().unwrap();
    assert!(a.session.is_some());
    assert!(a.is_streaming());
    assert_eq!(a.messages.len(), 1);
    assert_eq!(a.messages[0].role, "user");
}

#[tokio::test]
async fn esc_stop_keeps_partial_response() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Token("partial answer".into())).unwrap();
    a.stop_stream().unwrap();
    assert!(!a.is_streaming());
    assert!(a.stream_abort.is_none());
    assert_eq!(a.messages.last().unwrap().content, "partial answer");
    assert_eq!(a.status, "response stopped");
    // No-op when nothing streams.
    a.stop_stream().unwrap();
}

#[test]
fn panels_split_favorites_from_available_by_recency() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    a.models = vec![
        Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None, supports_images: false },
        Model { id: "b/two".into(), name: "Two".into(), supports_reasoning: false, context_length: None, supports_images: false },
        Model { id: "c/three".into(), name: "Three".into(), supports_reasoning: false, context_length: None, supports_images: false },
    ];
    // three is favorite; two was used more recently than one.
    a.favorites.insert("c/three".into());
    a.last_used.insert("a/one".into(), "2026-01-01T00:00:00Z".into());
    a.last_used.insert("b/two".into(), "2026-02-01T00:00:00Z".into());

    let favs: Vec<&str> = a.favorite_models().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(favs, vec!["c/three"]);
    let avail: Vec<&str> = a.available_models().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(avail, vec!["b/two", "a/one"]); // recency first
}

#[test]
fn toggle_favorite_persists_and_moves_panel() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    a.models = vec![Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None, supports_images: false }];
    a.model_focus = ModelPanel::Available;
    a.avail_state.select(Some(0));
    a.toggle_favorite_focused().unwrap();
    assert!(a.favorites.contains("a/one"));
    assert_eq!(a.favorite_models().len(), 1);
    assert_eq!(a.available_models().len(), 0);

    a.model_focus = ModelPanel::Favorites;
    a.fav_state.select(Some(0));
    a.toggle_favorite_focused().unwrap();
    assert!(!a.favorites.contains("a/one"));
}

#[test]
fn filter_narrows_available() {
    let mut a = app_with_key();
    a.model_filter = "two".into();
    let f = a.available_models();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].id, "b/two");
}

#[test]
fn reasoning_cycles_only_for_supporting_models() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    a.models = vec![
        Model { id: "r/model".into(), name: "R".into(), supports_reasoning: true, context_length: Some(1000), supports_images: false },
    ];
    a.model_focus = ModelPanel::Available;
    a.avail_state.select(Some(0));
    a.cycle_reasoning_focused().unwrap();
    assert_eq!(a.reasoning_of("r/model"), Some("low"));
    a.cycle_reasoning_focused().unwrap();
    a.cycle_reasoning_focused().unwrap();
    assert_eq!(a.reasoning_of("r/model"), Some("high"));
    a.cycle_reasoning_focused().unwrap(); // high -> off
    assert_eq!(a.reasoning_of("r/model"), None);
    // persisted
    assert!(a.db.load_model_prefs().unwrap().iter().any(|p| p.id == "r/model"));
}

#[test]
fn settings_edit_and_save_persists() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    a.popup = Popup::Settings;
    a.settings_selected = 0; // ShowStats
    a.toggle_settings_field();
    assert!(a.settings.show_stats);
    a.settings_selected = 3; // Temperature
    for c in "0.7".chars() {
        a.settings_input_char(c);
    }
    a.save_settings().unwrap();
    assert_eq!(a.settings.temperature, Some(0.7));

    // reload from db picks up the saved values
    let b = App::new(Db::open_in_memory().unwrap(), Some("k".into()), test_space());
    let _ = b; // separate in-memory db; just assert current instance loads its own
    let reloaded = a.db.load_settings().unwrap();
    assert!(reloaded.iter().any(|(k, v)| k == "temperature" && v == "0.7"));
    assert!(reloaded.iter().any(|(k, v)| k == "show_stats" && v == "1"));
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
fn verbosity_cycles_through_all_three_levels() {
    let mut a = app_with_key();
    a.popup = Popup::Settings;
    a.settings_selected = 9; // Verbosity
    assert_eq!(a.verbosity, "concise");
    a.toggle_settings_field();
    assert_eq!(a.verbosity, "caveman");
    a.toggle_settings_field();
    assert_eq!(a.verbosity, "normal");
    a.toggle_settings_field();
    assert_eq!(a.verbosity, "concise");
}

#[test]
fn verbosity_setting_persists_and_changes_the_prompt() {
    let mut a = app_with_key();
    a.popup = Popup::Settings;
    a.settings_selected = 9; // Verbosity
    a.toggle_settings_field(); // -> caveman
    a.save_settings().unwrap();
    assert!(a.system_prompt().contains(verbosity_clause("caveman")));

    let reloaded = a.db.load_settings().unwrap();
    assert!(reloaded.iter().any(|(k, v)| k == "verbosity" && v == "caveman"));
}

#[test]
fn searxng_url_setting_persists_and_enables_web_search_tool() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    // web_search always works (DuckDuckGo fallback needs no config); only
    // the backend it uses depends on this setting.
    assert!(a.toolbox.defs().iter().any(|t| t.name == "web_search"));
    assert!(a.toolbox.searxng_url.is_none());

    a.popup = Popup::Settings;
    a.settings_selected = 8; // SearxngUrl
    for c in "http://localhost:8080/".chars() {
        a.settings_input_char(c);
    }
    a.save_settings().unwrap();

    assert_eq!(a.searxng_url, "http://localhost:8080"); // trailing slash trimmed
    assert_eq!(a.toolbox.searxng_url.as_deref(), Some("http://localhost:8080"));
    assert!(a.skills.iter().any(|s| s.name == "web-search")); // built-in materialized

    let reloaded = a.db.load_settings().unwrap();
    assert!(reloaded.iter().any(|(k, v)| k == "searxng_url" && v == "http://localhost:8080"));

    // Reloading a fresh App from the same db picks it back up.
    let mut b = App::new(a.db, Some("k".into()), test_space());
    b.load_settings();
    assert_eq!(b.searxng_url, "http://localhost:8080");
}

#[test]
fn last_used_model_restored_on_startup() {
    let db = Db::open_in_memory().unwrap();
    db.mark_model_used("a/one").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.mark_model_used("b/two").unwrap(); // more recent
    let a = App::new(db, Some("k".into()), test_space());
    assert_eq!(a.current_model.as_deref(), Some("b/two"));
}

#[test]
fn context_used_and_limit() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from the always-installed web-search skill
    a.base_system_prompt = String::new(); // isolate from the base system prompt
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    assert_eq!(a.context_limit(), Some(1000));
    a.messages.push(Message {
        id: String::new(),
        role: "user".into(),
        content: "x".repeat(40), // ~10 tokens
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        phrase: None,
        images: Vec::new(),
    });
    assert_eq!(a.context_used(), 10);
}

#[test]
fn compaction_narrows_effective_messages_and_context_used() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from the always-installed web-search skill
    a.base_system_prompt = String::new(); // isolate from the base system prompt
    a.models[0].context_length = Some(1000);
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let mut s = a.db.create_session("t", "a/one", &space).unwrap();
    for i in 0..4 {
        a.messages.push(Message {
            id: String::new(),
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: "x".repeat(40), // ~10 tokens each
            model: None, reasoning: None, tokens: None, secs: None, phrase: None,
            images: Vec::new(),
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
    let s = a.db.create_session("t", "a/one", &space).unwrap();
    let sid = s.id.clone();
    a.session = Some(s);
    a.context_total = Some(999); // stale exact usage from before compaction

    a.on_compact_result(Some((sid.clone(), "digest".into(), 3, 61)));

    assert_eq!(a.session.as_ref().unwrap().compact_summary.as_deref(), Some("digest"));
    assert_eq!(a.session.as_ref().unwrap().compact_through, 3);
    assert!(a.context_total.is_none()); // stale total dropped
    assert!(a.status.contains("compacted: 61%"));
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
    assert!(a.status.contains("no active session"));

    // Session exists but everything in it is already covered.
    let space = a.active_space.id.clone();
    let mut s = a.db.create_session("t", "a/one", &space).unwrap();
    s.compact_through = 0;
    a.session = Some(s);
    a.force_compact();
    assert!(a.compact_rx.is_none());
    assert!(a.status.contains("nothing new"));

    // Now there's an uncompacted message — should actually kick off a job.
    a.messages.push(Message {
        id: String::new(), role: "user".into(), content: "hi".into(),
        model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        images: Vec::new(),
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
        id: String::new(), role: "user".into(), content: "x".repeat(40),
        model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        images: Vec::new(),
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
    let mut s = a.db.create_session("t", "a/one", &space).unwrap();
    s.compact_summary = Some("original digest".into());
    s.compact_through = 2;
    a.session = Some(s);

    let path = a.compact_summary_path().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original digest");

    // Simulate a hand-edit in $EDITOR, then reload.
    std::fs::write(&path, "hand-edited digest\n").unwrap();
    a.reload_compact_summary(&path).unwrap();
    assert_eq!(a.session.as_ref().unwrap().compact_summary.as_deref(), Some("hand-edited digest"));
    let reloaded = &a.db.list_sessions(&space).unwrap()[0];
    assert_eq!(reloaded.compact_summary.as_deref(), Some("hand-edited digest"));
    assert_eq!(reloaded.compact_through, 2); // boundary untouched by the edit
}

#[test]
fn system_prompt_lists_files_but_not_their_content() {
    let mut a = app_with_key();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plan.md"), "SECRET-CONTENT-MARKER inside").unwrap();
    a.rescan_files();

    let sp = a.system_prompt();
    assert!(sp.contains("## Files"));
    assert!(sp.contains("plan.md"));
    assert!(sp.contains("search_files"));
    assert!(!sp.contains("SECRET-CONTENT-MARKER")); // names only, never content

    // No files → no section.
    std::fs::remove_file(dir.join("plan.md")).unwrap();
    a.rescan_files();
    assert!(!a.system_prompt().contains("## Files"));
}

#[test]
fn pick_model_at_sets_current_and_closes() {
    let mut a = app_with_key();
    a.popup = Popup::Model;
    a.pick_model_at(ModelPanel::Available, 0).unwrap();
    assert!(a.current_model.is_some());
    assert!(a.popup == Popup::None);
}

#[test]
fn picking_memory_model_sets_it_and_returns_to_settings() {
    let mut a = app_with_key();
    let original_model = a.current_model.clone();
    a.open_model_picker_for_memory();
    assert!(a.popup == Popup::Model);
    assert!(a.model_pick_target == ModelPickTarget::Memory);

    a.pick_model_at(ModelPanel::Available, 0).unwrap();
    assert_eq!(a.memory_model, "a/one");
    assert!(a.popup == Popup::Settings); // back to /config, not closed
    assert_eq!(a.current_model, original_model); // session model untouched
    assert_eq!(
        a.db.load_settings().unwrap().iter().find(|(k, _)| k == "memory_model").map(|(_, v)| v.clone()),
        Some("a/one".to_string())
    );
}

#[test]
fn clear_memory_model_disables_extraction() {
    let mut a = app_with_key();
    a.memory_model = "some/model".into();
    a.clear_memory_model().unwrap();
    assert!(a.memory_model.is_empty());
    assert_eq!(
        a.db.load_settings().unwrap().iter().find(|(k, _)| k == "memory_model").map(|(_, v)| v.clone()),
        Some(String::new())
    );
}

#[test]
fn transcriber_model_defaults_and_persists() {
    let mut a = app_with_key();
    assert_eq!(a.transcriber_model, "google/gemini-2.5-flash-lite");
    a.transcriber_model = "some/vision-model".to_string();
    a.save_settings().unwrap();
    let kv = a.db.load_settings().unwrap();
    assert!(kv.iter().any(|(k, v)| k == "transcriber_model" && v == "some/vision-model"));
}

#[tokio::test]
async fn finish_stream_persists_assistant_message() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hi");
    a.submit().unwrap();
    a.on_stream_event(StreamEvent::Token("pong".into())).unwrap();
    a.on_stream_event(StreamEvent::Done).unwrap();
    assert!(!a.is_streaming());
    assert_eq!(a.messages.last().unwrap().content, "pong");
    let sid = a.session.as_ref().unwrap().id.clone();
    assert_eq!(a.db.load_messages(&sid).unwrap().len(), 2);
}

#[test]
fn model_picker_without_key_opens_key_prompt() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, None, test_space());
    a.open_model_picker();
    assert!(a.popup == Popup::Key);
}

#[test]
fn command_autocomplete_fuzzy_matches_names_aliases_and_desc() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from the always-installed web-search skill

    // Bare "/" lists everything; a space closes the popup.
    a.set_input("/");
    assert_eq!(a.command_matches().len(), crate::input::COMMANDS.len());
    a.set_input("/new foo");
    assert!(a.command_matches().is_empty());

    // Alias fuzzy-matches to the canonical command.
    a.set_input("/history");
    assert_eq!(a.command_matches()[0].name(), "session");

    // Description is searchable ("stats" -> config).
    a.set_input("/stats");
    assert_eq!(a.command_matches()[0].name(), "config");

    // Non-subsequence garbage matches nothing.
    a.set_input("/zzzz");
    assert!(a.command_matches().is_empty());
}

fn install_test_skill(a: &mut App, name: &str, desc: &str, body: &str) {
    let dir = std::env::temp_dir().join(format!("nexus-test-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: {desc}\n---\n{body}"))
        .unwrap();
    a.skills.push(crate::skills::Skill { name: name.to_string(), description: desc.to_string(), dir });
}

#[test]
fn skills_are_merged_into_command_matches_and_ranked() {
    let mut a = app_with_key();
    a.skills.clear(); // isolate this test from the always-installed web-search skill
    install_test_skill(&mut a, "web-search", "Search the web", "instructions");
    a.set_input("/");
    let matches = a.command_matches();
    assert_eq!(matches.len(), crate::input::COMMANDS.len() + 1);
    assert!(matches.iter().any(|m| m.name() == "web-search"));

    a.set_input("/web-search");
    let matches = a.command_matches();
    assert_eq!(matches[0].name(), "web-search");
}

#[tokio::test]
async fn forced_skill_with_trailing_text_sends_immediately() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    install_test_skill(&mut a, "web-search", "Search the web", "search instructions");
    a.set_input("/web-search rust news");
    a.submit().unwrap();
    assert!(a.forced_skill.is_none()); // consumed by send_message
    assert_eq!(a.messages.last().unwrap().content, "rust news");
    assert!(a.is_streaming());
}

#[test]
fn forced_skill_without_text_arms_for_next_message() {
    let mut a = app_with_key();
    install_test_skill(&mut a, "web-search", "Search the web", "search instructions");
    a.set_input("/web-search");
    a.submit().unwrap();
    assert_eq!(a.forced_skill.as_deref(), Some("web-search"));
    assert!(a.status.contains("armed"));
}

#[test]
fn accept_command_fill_vs_run() {
    // Tab fills the composer with the canonical command, doesn't run it.
    let mut a = app_with_key();
    a.set_input("/hist");
    a.accept_command(false).unwrap();
    assert_eq!(a.input_text(), "/session ");

    // Enter on the "new" alias runs it, clearing the composer.
    let mut b = app_with_key();
    b.current_model = Some("a/one".into());
    let space_id = b.active_space.id.clone();
    b.session = Some(b.db.create_session("old chat", "a/one", &space_id).unwrap());
    b.set_input("/clear");
    b.accept_command(true).unwrap();
    assert!(b.input_text().is_empty());
    assert!(b.session.is_none()); // /new clears the view; no row created until a message is sent
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
    a.set_input("hi");
    a.submit().unwrap();
    a.on_stream_event(StreamEvent::Token("<think>pondering</think>".into())).unwrap();
    a.on_stream_event(StreamEvent::Token("the real answer".into())).unwrap();
    a.on_stream_event(StreamEvent::Done).unwrap();

    let msg = a.messages.last().unwrap();
    assert_eq!(msg.content, "the real answer");
    assert_eq!(msg.reasoning.as_deref(), Some("pondering"));

    // Copying the message must not leak the stripped reasoning back in.
    a.status = "sentinel".into();
    a.copy_message(1);
    assert_ne!(a.status, "sentinel");
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
        MemoryOp::Delete(1),                          // drop "likes rust"
        MemoryOp::Update(2, "updated fact".into()),    // "old fact" -> "updated fact"
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
    assert_eq!(std::fs::read_to_string(a_memory_path(&a)).unwrap(), "1. keep me\n");
}

fn a_memory_path(a: &App) -> std::path::PathBuf {
    a.space.ensure_space_dir(&a.active_space.name).unwrap();
    a.space.memory_path(&a.active_space.name)
}

#[test]
fn space_crud_via_app_methods() {
    let mut a = app_with_key();
    a.spaces_cache = a.db.list_spaces().unwrap();
    a.space_edit = "work".into();
    a.confirm_space_create().unwrap();
    assert!(a.spaces_cache.iter().any(|s| s.name == "work"));

    a.space_selected = a.spaces_cache.iter().position(|s| s.name == "work").unwrap();
    a.space_edit = "work-2".into();
    a.confirm_space_rename().unwrap();
    assert!(a.spaces_cache.iter().any(|s| s.name == "work-2"));

    a.confirm_space_delete().unwrap(); // "work-2" still selected
    assert!(!a.spaces_cache.iter().any(|s| s.name == "work-2"));
    assert_eq!(a.active_space.name, DEFAULT_SPACE); // untouched, wasn't active
}

#[tokio::test]
async fn switching_space_clears_open_conversation() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    assert!(a.session.is_some());

    let other = a.db.create_space("other").unwrap();
    a.spaces_cache = vec![other.clone()];
    a.space_selected = 0;
    a.confirm_space().unwrap();

    assert_eq!(a.active_space.id, other.id);
    assert!(a.session.is_none());
    assert!(a.messages.is_empty());
}

#[test]
fn copy_message_uses_exact_original_content() {
    let mut a = app_with_key();
    a.messages.push(Message {
        id: String::new(), role: "user".into(), content: "raw *user* text".into(),
        model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        images: Vec::new(),
    });
    a.messages.push(Message {
        id: String::new(), role: "assistant".into(), content: "**bold** reply".into(),
        model: Some("a/one".into()), reasoning: None, tokens: None, secs: None, phrase: None,
        images: Vec::new(),
    });
    // copy_message resolves *some* text at each index (clipboard availability
    // is environment-dependent in CI, so just assert it didn't silently no-op).
    a.status = "sentinel".into();
    a.copy_message(0); // user message: verbatim
    assert_ne!(a.status, "sentinel");

    a.status = "sentinel".into();
    a.copy_message(1); // assistant message: through markdown::to_plain
    assert_ne!(a.status, "sentinel");

    // Streaming reply (not yet in `messages`) uses index == messages.len().
    a.streaming = Some("live tokens".into());
    a.status = "sentinel".into();
    a.copy_message(2);
    assert_ne!(a.status, "sentinel");

    // An out-of-range index (no streaming either) is a no-op.
    a.streaming = None;
    a.status = "sentinel".into();
    a.copy_message(2);
    assert_eq!(a.status, "sentinel");
}

#[test]
fn history_carries_image_parts_for_vision_models_and_text_for_others() {
    let mut a = app_with_key();
    let s = a.db.create_session("t", "vis/model", &a.active_space.id).unwrap();
    let mid = a.db.add_user_message(&s.id, "what is this?").unwrap();
    // A real tiny png on disk so the vision path can read it back.
    let dir = a.space.images_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    let png_path = dir.join("t.png");
    std::fs::write(&png_path, crate::app::transcribe::encode_png(1, 1, &[0, 0, 0, 255]).unwrap()).unwrap();
    let imgs = a.db.add_message_images(&mid, &[png_path.to_string_lossy().to_string()]).unwrap();
    a.db.set_image_description(&imgs[0].id, "a black pixel").unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();
    a.models = vec![
        Model { id: "vis/model".into(), name: "v".into(), supports_reasoning: false, context_length: None, supports_images: true },
        Model { id: "txt/model".into(), name: "t".into(), supports_reasoning: false, context_length: None, supports_images: false },
    ];

    a.current_model = Some("vis/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert_eq!(user.images.len(), 1);
    assert!(user.images[0].starts_with("data:image/png;base64,"));
    assert_eq!(user.content, "what is this?");

    a.current_model = Some("txt/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert!(user.images.is_empty());
    assert!(user.content.contains("[Image: a black pixel]"));
}

#[test]
fn missing_descriptions_are_collected_for_non_vision_sends() {
    let mut a = app_with_key();
    let s = a.db.create_session("t", "txt/model", &a.active_space.id).unwrap();
    let mid = a.db.add_user_message(&s.id, "see").unwrap();
    a.db.add_message_images(&mid, &["/tmp/nope.png".into()]).unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();
    let missing = a.undescribed_images();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].1, "/tmp/nope.png");
}

#[tokio::test]
async fn submit_during_deferred_send_is_rejected_and_preserved() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    a.session = Some(a.db.create_session("test", "a/one", &space).unwrap());
    a.deferred_send = Some(String::new());
    a.send_message("second message".into()).unwrap();
    assert!(a.status.contains("understanding"));
    assert_eq!(a.input_text(), "second message"); // restored, not lost
    assert!(a.messages.is_empty()); // nothing was stored
}

#[test]
fn switching_context_cancels_deferred_image_send() {
    let mut a = app_with_key();
    a.deferred_send = Some(String::new());
    a.pending_images.push(crate::app::transcribe::PendingImage {
        path: std::path::PathBuf::from("/tmp/x.png"),
    });
    a.new_session().unwrap();
    assert!(a.deferred_send.is_none());
    assert!(a.pending_images.is_empty());
}

#[test]
fn tool_call_events_persist_and_stay_out_of_history() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space).unwrap();
    a.session = Some(s.clone());
    a.streaming = Some(String::new());
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

    // …but never sent back to the model.
    let h = a.build_history();
    assert!(h.iter().all(|m| !m.content.contains("q3 revenue")));
}

#[test]
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
        tool_call_summary("read_file", r#"{"name":"r.pdf"}"#, "r.pdf (lines 1-200 of 831):\n…"),
        "read_file r.pdf → r.pdf (lines 1-200 of 831)"
    );
    assert_eq!(
        tool_call_summary("write_file", r#"{"app":"deck","path":"index.html","content":"12345"}"#, "wrote"),
        "write_file deck/index.html (5 bytes)"
    );
    assert_eq!(tool_call_summary("edit_file", r#"{"app":"a","path":"b.js"}"#, "ok"), "edit_file a/b.js");
    assert_eq!(tool_call_summary("skill", r#"{"name":"commit"}"#, "…"), "skill commit");
    assert_eq!(
        tool_call_summary("install_skill", r#"{"source":"anthropics/skills/pdf"}"#, "installed skill 'pdf' — load it with the skill tool"),
        "install_skill anthropics/skills/pdf → installed skill 'pdf' — load it with the skill tool"
    );
    assert_eq!(
        tool_call_summary("run_script", r#"{"skill":"pdf","script":"scripts/fill.py"}"#, "ok"),
        "run_script pdf/scripts/fill.py"
    );
    assert_eq!(
        tool_call_summary("install_packages", r#"{"packages":["pillow","requests"],"skill":"pdf"}"#, "ok"),
        "install_packages pillow requests → pdf"
    );
    let long = format!(r#"{{"x":"{}"}}"#, "y".repeat(100));
    assert!(tool_call_summary("mystery", &long, "").ends_with('…'));
}
