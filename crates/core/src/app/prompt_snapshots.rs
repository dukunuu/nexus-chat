use std::path::Path;

use super::{App, tests::app_with_key};

const CHAT_PROMPT_TOKEN_CEILING: usize = 3_500; // measured 3,074 with per-action tool definitions

fn fixture_app() -> App {
    // app_with_key uses a fresh temporary space, with no space instructions.
    let mut a = app_with_key();
    a.skills.clear();
    a.current_model = Some("a/one".into());
    a.prompt_datetime = "2026-01-01 00:00 UTC, Thursday".into();
    a.system_prompt_overlay.clear();
    a.memory_snapshot.clear();
    a.interactive_ui = false;
    a
}

fn normalized_prompt(a: &App) -> String {
    a.system_prompt().replace(
        &format!(
            "- OS: {} ({})",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        "- OS: <os> (<arch>)",
    )
}

fn assert_snapshot(name: &str, actual: &str) {
    let directory = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/prompt-snapshots/"
    ));
    let path = directory.join(name);
    if std::env::var("NEXUS_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let message = format!(
        "snapshot {}: rerun with NEXUS_UPDATE_SNAPSHOTS=1 cargo test -p nexus-chat-core prompt_snapshots and review the diff",
        path.display()
    );
    let expected =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{message}: {error}"));
    assert_eq!(expected, actual, "{message}");
}

#[test]
fn chat_prompt_snapshot() {
    assert_snapshot("chat.txt", &normalized_prompt(&fixture_app()));
}

#[test]
fn tui_prompt_snapshot() {
    let mut a = fixture_app();
    a.interactive_ui = true;
    assert_snapshot("tui.txt", &normalized_prompt(&a));
}

#[test]
fn tool_defs_snapshot() {
    let a = fixture_app();
    let actual = serde_json::to_string_pretty(&a.toolbox.defs()).unwrap() + "\n";
    assert_snapshot("tool-defs.json", &actual);
}

#[test]
fn chat_prompt_and_tool_defs_fit_the_token_budget() {
    let a = fixture_app();
    let defs = serde_json::to_string(&a.toolbox.defs()).unwrap();
    let tokens = (a.system_prompt().chars().count() + defs.chars().count()) / 4;
    assert!(
        tokens <= CHAT_PROMPT_TOKEN_CEILING,
        "chat prompt and tool definitions use {tokens} estimated tokens, exceeding the ceiling of {CHAT_PROMPT_TOKEN_CEILING}"
    );
}

/// Field and operator names from the `nexus-ui` schema (`interactive-ui.md`), not tools.
const NEXUS_UI_SCHEMA_WORDS: &[&str] = &[
    "version", "title", "state", "view", "body", "actions", "number",
];

#[test]
fn assembled_prompts_mention_only_existing_tools() {
    let mut a = fixture_app();
    let defs = a.toolbox.defs();
    for (name, interactive_ui) in [("chat.txt", false), ("tui.txt", true)] {
        a.interactive_ui = interactive_ui;
        let prompt = a.system_prompt();
        // Scan adjacent backticks so fenced examples do not shift inline pairs.
        for (start, _) in prompt.match_indices('`') {
            let Some((token, _)) = prompt[start + 1..].split_once('`') else {
                continue;
            };
            if token.is_empty()
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                continue;
            }
            if NEXUS_UI_SCHEMA_WORDS.contains(&token) {
                continue;
            }
            assert!(
                defs.iter().any(|tool| tool.name == token),
                "{name} advertises unavailable tool `{token}`"
            );
        }
    }
}
