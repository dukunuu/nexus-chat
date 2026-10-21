use super::*;
use serde_json::json;

const ORIGINAL: &str = "https://upload.wikimedia.org/wikipedia/commons/thumb/c/cf/LeBron_James_%2851959977144%29_%28cropped2%29.jpg/330px-LeBron_James_%2851959977144%29_%28cropped2%29.jpg";

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

fn tool_result(result: &str) -> Message {
    message(
        "tool_call",
        &json!({"name":"fetch_url","arguments":"{}","result":result}).to_string(),
    )
}

fn hero(url: &str) -> String {
    format!(
        "```nexus-ui\n{}\n```",
        json!({"version":1,"title":"Player","view":[{"type":"hero","title":"LeBron James","image":url}]})
    )
}

#[test]
fn dropped_lebron_digit_is_repaired_in_both_cards_and_markdown() {
    let bad = ORIGINAL.replace("51959977144", "5199977144");
    let result = format!("thumbnail: {{\"source\":\"{ORIGINAL}\"}}");
    let source = format!("{}\n{}\n![LeBron]({bad})", hero(&bad), hero(&bad));
    let (fixed, count) = repair_image_links(&source, &[tool_result(&result)]);
    assert_eq!(count, 3);
    assert!(!fixed.contains(&bad));
    assert_eq!(fixed.matches(ORIGINAL).count(), 3);
}

#[test]
fn image_repair_refuses_other_hosts_ambiguity_and_verbatim_urls() {
    let bad = "https://photos.example/player1.jpg";
    for candidates in [
        "https://other.example/player.jpg",
        "https://photos.example/player2.jpg https://photos.example/player3.jpg",
        "https://photos.example/player2.jpg https://photos.example/player.jpg",
        "https://photos.example/player1.jpg https://photos.example/player2.jpg",
    ] {
        let reply = hero(bad);
        assert_eq!(
            repair_image_links(&reply, &[tool_result(candidates)]),
            (reply, 0)
        );
    }
    let reply = hero("https://photos.example/verydifferent.jpg");
    assert_eq!(repair_image_links(&reply, &[tool_result(bad)]), (reply, 0));
    let reply = hero(bad);
    assert_eq!(
        repair_image_links(
            &reply,
            &[message("user", "https://photos.example/player.jpg")]
        ),
        (reply, 0)
    );
}

#[test]
fn repair_uses_full_candidate_query_and_extensionless_images_search_results() {
    let candidate = "https://photos.example/picture?id=55&width=800";
    let row = message("tool_call", &json!({"name":"search","arguments":r#"{"mode":"images","query":"player"}"#,"result":format!("[1] Player\n    {candidate}\n    image · photos.example · page https://photos.example/page")}).to_string());
    let bad = "https://photos.example/pictur?id=5";
    let source = format!("![Player](<{bad}>)");
    assert_eq!(
        repair_image_links(&source, &[row]),
        (format!("![Player](<{candidate}>)"), 1)
    );
    let row = message("tool", "https://photos.example/player.jpg?size=800");
    let source = "![Player](https://photos.example/playe.jpg?size=80 \"Player\")";
    assert_eq!(
        repair_image_links(source, &[row]),
        (
            "![Player](https://photos.example/player.jpg?size=800 \"Player\")".into(),
            1
        )
    );
}

#[test]
fn all_ui_image_fields_are_repaired_but_literal_markdown_is_untouched() {
    let good = "https://photos.example/player.jpg";
    let bad = "https://photos.example/playe.jpg";
    let source = format!(
        "```nexus-ui\n{}\n```",
        json!({"version":1,"title":"Images","view":[
        {"type":"text","text":bad},
        {"type":"image","src":bad},
        {"type":"cards","items":[{"title":"Card","text":"Info","image":bad}]},
        {"type":"buttons","items":[{"label":"Tile","action":"pick","image":bad}]}
    ],"actions":{"pick":[{"op":"answer"}]}})
    );
    let (fixed, count) = repair_image_links(&source, &[tool_result(good)]);
    assert_eq!(count, 3);
    assert_eq!(fixed.matches(good).count(), 3);
    assert_eq!(fixed.matches(bad).count(), 1);
    for literal in [
        format!("```text\n![literal]({bad})\n```"),
        format!("`![literal]({bad})`"),
        format!("    ![literal]({bad})"),
    ] {
        assert_eq!(
            repair_image_links(&literal, &[tool_result(good)]),
            (literal, 0)
        );
    }
}

#[test]
fn mixed_literal_and_real_markdown_images_only_repair_the_real_one() {
    let good = "https://photos.example/player.jpg";
    let bad = "https://photos.example/playe.jpg";
    let source = format!("`![literal]({bad})` \\![escaped]({bad}) ![real]({bad})");
    let expected = format!("`![literal]({bad})` \\![escaped]({bad}) ![real]({good})");
    assert_eq!(
        repair_image_links(&source, &[tool_result(&format!("![source]({good})"))]),
        (expected, 1)
    );
}

#[test]
fn image_verifier_preference_is_shared_across_toolbox_refreshes() {
    let mut app = super::super::tests::app_with_key();
    let fetcher = app.image_fetcher.clone();
    assert!(fetcher.enabled());
    app.set_setting("remote_images", "off").unwrap();
    assert!(!fetcher.enabled());
    app.set_setting("remote_images", "on").unwrap();
    assert!(fetcher.enabled());
    assert!(std::sync::Arc::ptr_eq(&fetcher, &app.image_fetcher));
}

#[tokio::test]
async fn finalized_image_repair_is_stored_and_replayed_exactly() {
    let mut app = super::super::tests::app_with_key();
    app.current_model = None;
    let session = app
        .db
        .create_session("Player", "default", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session.clone());
    let tool = tool_result(ORIGINAL);
    app.db
        .add_tool_call_message(&session.id, &tool.content)
        .unwrap();
    app.messages.push(tool);
    let bad = ORIGINAL.replace("51959977144", "5199977144");
    let abort = tokio::spawn(std::future::pending::<()>()).abort_handle();
    abort.abort();
    let task = super::super::ChatTask {
        id: 1,
        session_id: session.id.clone(),
        session_title: "Player".into(),
        space_id: app.active_space.id.clone(),
        model: "test".into(),
        model_id: "test".into(),
        backend: crate::provider::BackendTag::OpenRouter,
        incognito: false,
        buffer: hero(&bad),
        thinking: String::new(),
        tool_status: None,
        usage: None,
        usage_row_id: None,
        started: std::time::Instant::now(),
        thinking_idx: 0,
        spinner_color: super::super::SpinnerColor::Green,
        abort,
    };
    app.finish_chat_task_state(task, None, false).unwrap();
    let stored = app.db.load_messages(&session.id).unwrap();
    let reply = stored
        .iter()
        .find(|message| message.role == "assistant")
        .unwrap();
    assert_eq!(reply.content, hero(ORIGINAL));
    assert_eq!(app.messages.last().unwrap().content, reply.content);
    assert!(
        app.build_history()
            .iter()
            .any(|message| message.role == "assistant" && message.content == reply.content)
    );
    assert!(app.last_status().contains("fixed 1 image link"));
}
