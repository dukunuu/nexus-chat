use super::*;

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

#[test]
fn structured_errors_return_once_for_the_last_reply_without_changing_stored_source() {
    let mut app = super::super::tests::app_with_key();
    app.interactive_ui = true;
    let broken = "```nexus-ui\nroot = Panel('Broken', [Text('Good'), Nope()])\n```";
    app.messages = vec![message("assistant", broken), message("user", "fix it")];
    let history = app.build_history();
    let feedback = history
        .iter()
        .find(|m| m.content.starts_with("nexus-ui errors: "))
        .unwrap();
    let errors: serde_json::Value =
        serde_json::from_str(feedback.content.strip_prefix("nexus-ui errors: ").unwrap()).unwrap();
    assert_eq!(errors[0]["statementId"], "p1-1");
    assert_eq!(errors[0]["source"], "parser");
    assert!(errors[0]["message"].as_str().unwrap().contains("Nope"));
    assert!(errors[0]["hint"].is_string());
    assert!(
        !app.build_history()
            .iter()
            .any(|m| m.content.starts_with("nexus-ui errors: "))
    );
    assert_eq!(app.messages[0].content, broken);
    app.messages.push(message("assistant", broken));
    assert!(
        app.build_history()
            .iter()
            .any(|m| m.content.starts_with("nexus-ui errors: "))
    );
}

#[test]
fn history_exposes_stable_panel_ids_programs_and_strips_edit_markers() {
    let messages = vec![
        message(
            "assistant",
            "```nexus-ui\nroot = Panel('Original', [text])\ntext = Text('Old')\n```",
        ),
        message(
            "assistant",
            "```nexus-ui\nedit p1-1\ntext = Text('New')\n```",
        ),
        message("assistant", "```nexus-ui\nedit p1-1\ntext = Nope()\n```"),
    ];
    let (history, errors) = ui_program_history(&messages, 0);
    assert!(history[&0].contains("nexus-ui panel p1-1"));
    assert!(history[&0].contains("root = Panel"));
    assert!(history[&0].contains("text = Text('New')"));
    assert!(!history[&0].contains("Text('Old')"));
    assert!(history[&1].contains("Patch for nexus-ui panel p1-1"));
    assert!(!history[&1].contains("edit p1-1"));
    assert_eq!(errors[0]["path"], "edit");
    assert!(errors[0]["message"].as_str().unwrap().contains("Nope"));
    assert!(
        failing_ui_blocks(&messages[1].content).is_empty(),
        "patches must not trigger whole-panel repair requests"
    );
}
