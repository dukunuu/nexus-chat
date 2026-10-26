use super::*;

#[test]
fn usage_session_flag_text_and_json_include_all_requests_and_totals() {
    let parsed = Cli::try_parse_from(["nexus", "usage", "--session", "s", "--json"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Command::Usage { session: Some(id), json: true, .. }) if id == "s")
    );
    let db = db::Db::open_in_memory().unwrap();
    for (kind, model, cost) in [
        (Some("chat"), "test/model", Some(0.01)),
        (None, "subscription", None),
    ] {
        let id = db
            .log_usage(
                "OpenRouter",
                model,
                100,
                20,
                40,
                0,
                Default::default(),
                cost,
                cost.is_some(),
                Some("s"),
                None,
            )
            .unwrap();
        if let Some(kind) = kind {
            db.set_usage_kind(id, kind).unwrap();
            db.add_usage_tool(id, "fs").unwrap();
        }
    }
    db.log_usage(
        "OpenRouter",
        "other",
        900,
        1,
        0,
        0,
        Default::default(),
        Some(1.0),
        true,
        Some("other"),
        None,
    )
    .unwrap();
    let text = session_usage_output(&db, "s", false).unwrap();
    assert!(text.contains("chat · fs"), "{text}");
    assert!(
        text.contains("test/model  100 / 40 / 20  $0.0100"),
        "{text}"
    );
    assert!(text.contains("subscription  100 / 40 / 20  —"), "{text}");
    assert!(
        text.contains("Total: 2 requests · 240 tokens · $0.0100"),
        "{text}"
    );
    assert!(text.contains("1 request(s) with unknown cost"), "{text}");
    assert!(!text.contains("other"));
    let value: serde_json::Value =
        serde_json::from_str(&session_usage_output(&db, "s", true).unwrap()).unwrap();
    assert_eq!(value["session_id"], "s");
    assert_eq!(value["requests"].as_array().unwrap().len(), 2);
    assert_eq!(value["requests"][0]["kind"], "chat");
    assert_eq!(value["requests"][0]["tools"], "fs");
    assert!(value["requests"][1]["kind"].is_null());
    assert!(value["requests"][1]["cost"].is_null());
    assert_eq!(value["totals"]["prompt_tokens"], 200);
    assert_eq!(value["totals"]["cache_read_tokens"], 80);
    assert_eq!(value["totals"]["completion_tokens"], 40);
    assert_eq!(value["totals"]["unknown_cost_requests"], 1);
    assert_eq!(value["totals"]["cost"], 0.01);
    assert!(
        session_usage_output(&db, "empty", false)
            .unwrap()
            .contains("No requests in this session.")
    );
}
