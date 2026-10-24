use std::path::Path;

use super::{App, tests::app_with_key};

// Measured 3,938 with per-action tool definitions; the `deck` tool is about 865 of
// that (its ops, block kinds, commenting principles and the history action are
// always on). Raised from 3,900 by 60 for the `history` action and the
// one-deck-per-request / images / read-by-slides guidance.
const CHAT_PROMPT_TOKEN_CEILING: usize = 3_960;

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
fn agent_prompt_snapshot() {
    let mut a = fixture_app();
    let root = std::env::temp_dir().join(format!("nexus-agent-prompt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    a.run_command(&format!("agent {}", root.display())).unwrap();
    assert_eq!(a.work_mode, super::WorkMode::Agent);
    a.skills.clear();
    a.prompt_datetime = "2026-01-01 00:00 UTC, Thursday".into();
    let workspace = a.workspace.as_ref().unwrap().root().display().to_string();
    let actual = normalized_prompt(&a).replace(&workspace, "<workspace>");
    assert_snapshot("agent.txt", &actual);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_prompt_snapshot() {
    let mut a = fixture_app();
    a.interactive_ui = true;
    assert_snapshot("tui.txt", &normalized_prompt(&a));
}

#[test]
fn terminal_ui_prompt_streams_without_a_preflight_tool() {
    let mut a = fixture_app();
    a.interactive_ui = true;
    let prompt = a.system_prompt();
    assert!(prompt.contains(crate::interactive::INSTRUCTIONS));
    assert!(prompt.contains("no preflight tool call is needed"));
    assert!(!prompt.contains("ui_check"));
}

#[test]
fn tool_defs_snapshot() {
    let a = fixture_app();
    let actual = serde_json::to_string_pretty(&a.toolbox.defs()).unwrap() + "\n";
    assert_snapshot("tool-defs.json", &actual);
}

#[test]
fn tool_defs_compact_snapshot() {
    let a = fixture_app();
    let actual = serde_json::to_string_pretty(&a.toolbox.defs_compact()).unwrap() + "\n";
    assert_snapshot("tool-defs-compact.json", &actual);
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
    "version", "title", "state", "view", "body", "actions", "number", "null", "true",
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

fn connector_fixture() -> crate::connectors::Registry {
    use crate::connectors::protocol::{Annotations, ToolInfo};
    use crate::connectors::{ConnectorConfig, Registry, Status};
    let registry = Registry::empty();
    let tool =
        |name: &str, title: Option<&str>, description: &str, read_only: Option<bool>| ToolInfo {
            name: name.into(),
            title: title.map(str::to_string),
            description: description.into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "the record id"}},
                "required": ["id"]
            }),
            annotations: Annotations {
                read_only,
                raw: read_only.map(|_| "{}".to_string()),
                ..Annotations::default()
            },
        };
    for (name, url, tools) in [
        (
            "linear",
            "https://mcp.linear.app/mcp",
            vec![
                tool(
                    "get_issue",
                    Some("Get issue"),
                    "Fetch one issue by id.",
                    Some(true),
                ),
                tool("create_issue", None, "Create an issue.", Some(false)),
            ],
        ),
        (
            "notes",
            "https://notes.example/mcp",
            vec![tool("search notes!", None, "", None)],
        ),
    ] {
        registry.add(name, ConnectorConfig::remote(url)).unwrap();
        registry.force_state(name, Status::Connected { tools: tools.len() }, tools);
    }
    registry
}

#[test]
fn connector_tool_defs_snapshot() {
    let registry = connector_fixture();
    let exposure = registry.exposure();
    assert!(!exposure.meta);
    assert_snapshot(
        "tool-defs-connectors.json",
        &(serde_json::to_string_pretty(&exposure.defs).unwrap() + "\n"),
    );
}

#[test]
fn connector_meta_tool_def_snapshot() {
    let meta = {
        use crate::connectors::protocol::{Annotations, ToolInfo};
        use crate::connectors::{ConnectorConfig, Registry, Status};
        let many = Registry::empty();
        many.add("big", ConnectorConfig::remote("https://big.example/mcp"))
            .unwrap();
        let tools: Vec<ToolInfo> = (0..31)
            .map(|i| ToolInfo {
                name: format!("t{i}"),
                title: None,
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                annotations: Annotations::default(),
            })
            .collect();
        many.force_state("big", Status::Connected { tools: 31 }, tools);
        many.exposure().defs[0].clone()
    };
    assert_eq!(meta.name, "connectors");
    assert_snapshot(
        "connectors-meta-def.json",
        &(serde_json::to_string_pretty(&meta).unwrap() + "\n"),
    );
}

#[test]
fn no_connectors_leaves_the_tool_list_byte_identical() {
    let a = fixture_app();
    let baseline = serde_json::to_string(&a.toolbox.defs()).unwrap();
    let mut toolbox = crate::tools::ToolBox::new(
        std::path::PathBuf::new(),
        None,
        None,
        "auto".into(),
        Vec::new(),
        None,
        None,
        None,
    );
    for registry in [None, Some(crate::connectors::Registry::empty())] {
        toolbox.connectors = registry;
        let bare = serde_json::to_string(&toolbox.defs()).unwrap();
        // Same builder, same capabilities: nothing about connectors leaks in.
        assert!(!bare.contains("connectors"), "{bare}");
        assert!(!bare.contains("__"));
    }
    // The app's own list (golden-tested above) has the empty registry attached.
    assert!(a.toolbox.defs().iter().all(|d| d.name != "connectors"));
    assert_eq!(baseline, serde_json::to_string(&a.toolbox.defs()).unwrap());
}
