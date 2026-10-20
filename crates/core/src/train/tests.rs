use super::*;
use crate::provider::{ChatMessage, ToolCall, ToolDef};
use crate::trace::{TraceResponse, TraceStep, TraceToolResult};

struct Fixture {
    root: PathBuf,
    store: TraceStore,
    options: ExportOptions,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-train-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("evals")).unwrap();
        let store = TraceStore::open(&root.join("traces"), Scrubber::new(Vec::new())).unwrap();
        let options = ExportOptions {
            format: Format::ChatJsonl,
            filter: Filter::Success,
            models: vec!["local:*".into(), "distillable".into()],
            since: None,
            space_id: None,
            evals: root.join("evals"),
            out: root.join("out"),
            split: false,
        };
        Self {
            root,
            store,
            options,
        }
    }

    fn export(&self) -> (Report, Vec<Value>) {
        self.export_with(&Scrubber::new(Vec::new()), &BTreeMap::new())
    }

    fn export_with(
        &self,
        scrubber: &Scrubber,
        catalog: &BTreeMap<String, Option<bool>>,
    ) -> (Report, Vec<Value>) {
        let report = export(&self.store, &self.options, scrubber, catalog).unwrap();
        let rows = read_jsonl(
            &self
                .options
                .out
                .join(format!("{}.jsonl", self.options.format.name())),
        );
        (report, rows)
    }

    fn record(&self, step: &TraceStep) -> String {
        self.store.record_step(step).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn read_jsonl(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn step(turn: &str) -> TraceStep {
    TraceStep {
        session_id: "session-1".into(),
        space_id: "space-1".into(),
        turn_id: turn.into(),
        round: 0,
        model: "qwen/model".into(),
        backend: "Local".into(),
        system: "Use the tools carefully.".into(),
        tools: vec![definition("fs")],
        messages: vec![ChatMessage::text("user", "Read the notes and summarize.")],
        params: json!({}),
        response: TraceResponse {
            content: "The notes describe a useful plan.".into(),
            reasoning: None,
            tool_calls: Vec::new(),
            finish: "stop".into(),
        },
        usage: None,
        latency_ms: 1,
        tool_results: Vec::new(),
    }
}

fn definition(name: &str) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: "Recorded definition, not today's description".into(),
        parameters: json!({
            "type": "object", "properties": {"path": {"type": "string"}},
            "required": ["path"],
        }),
    }
}

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments: r#"{"path":"notes.txt","action":"read"}"#.into(),
    }
}

fn tool_result(seen: &str) -> TraceToolResult {
    TraceToolResult {
        call_id: "call-1".into(),
        name: "fs".into(),
        full: "FULL PRIVATE DETAIL NEVER SEEN BY MODEL".into(),
        seen: seen.into(),
    }
}

#[test]
fn every_offered_tool_is_current_and_its_recorded_definition_exports() {
    use crate::tools::{AppsCtx, FilesCtx, ToolBox};
    use std::sync::Arc;

    let fixture = Fixture::new();
    let db_path = fixture.root.join("nexus.db");
    let db = crate::db::Db::open(&db_path).unwrap();
    let space_id = db.default_space_id().unwrap();
    db.upsert_file(&space_id, "notes.txt", "hash", 1, "ok")
        .unwrap();
    let skills_dir = fixture.root.join("skills");
    fs::create_dir_all(skills_dir.join("example")).unwrap();
    fs::write(
        skills_dir.join("example/SKILL.md"),
        "---\nname: example\ndescription: example skill\n---\nInstructions\n",
    )
    .unwrap();
    let mut toolbox = ToolBox::new(
        skills_dir,
        None,
        None,
        "auto".into(),
        Vec::new(),
        Some(db_path.clone()),
        Some(FilesCtx {
            db_path: db_path.clone(),
            space_id: space_id.clone(),
            embedder: None,
        }),
        Some(AppsCtx {
            dir: fixture.root.join("apps"),
            server_port: 9999,
            public_base: None,
            registry: crate::appserver::AppRegistry::load(&fixture.root),
            space_name: "default".into(),
            space_id,
            space_db_path: db_path,
            files_dir: fixture.root.join("files"),
            session_id: "session-1".into(),
        }),
    )
    .with_research_session("session-1".into());
    toolbox.interactive_ui = true;
    toolbox.approver = Some(Arc::new(crate::app::EvalApprover {
        allow_shell: Vec::new(),
    }));
    let provider =
        crate::provider::openrouter::OpenRouter::local("http://localhost:11434/v1").unwrap();
    toolbox.image_gen_backend = Some((provider.clone(), "image-model".into()));
    toolbox.video_gen_backend = Some((provider, "video-model".into()));
    let mut offered = HashSet::new();
    for agent in [false, true] {
        toolbox.workspace = agent.then(|| {
            Arc::new(crate::workspace::Workspace::open(&fixture.root, Vec::new()).unwrap())
        });
        let defs = toolbox.defs();
        for def in &defs {
            assert!(is_current_tool(&def.name), "export rejects {}", def.name);
            offered.insert(def.name.clone());
        }
        let mut step = step(if agent { "agent" } else { "chat" });
        step.tools = defs;
        fixture.record(&step);
    }
    for (name, _) in crate::tools::current_tools() {
        assert!(offered.contains(name), "fixture did not exercise {name}");
    }
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 2);
    assert_eq!(report.dropped["stale_schema"], 0);
}

#[test]
fn retired_family_definitions_and_calls_follow_dispatch_selectors() {
    let fixture = Fixture::new();
    let cases = [
        (
            "skill_admin",
            json!({"action": "install", "source": "owner/skill"}),
            "skills",
            "action",
            "install",
        ),
        (
            "script_files",
            json!({"action": "write", "path": "run.sh", "content": ""}),
            "scripts",
            "action",
            "write",
        ),
        (
            "app_inspect",
            json!({"action": "read", "app": "site", "path": "index.html"}),
            "app",
            "action",
            "read",
        ),
        (
            "app_modify",
            json!({"action": "diff", "app": "site", "path": "index.html", "content": ""}),
            "app",
            "action",
            "diff",
        ),
        (
            "app_assets",
            json!({"action": "list"}),
            "app",
            "action",
            "list",
        ),
        (
            "video_transform",
            json!({"action": "stitch", "video_ids": ["a", "b"]}),
            "media",
            "action",
            "stitch",
        ),
        (
            "video_references",
            json!({"action": "save", "name": "ref", "image_id": "i", "description": "scene"}),
            "media",
            "action",
            "save_reference",
        ),
        (
            "read_app_file",
            json!({"app": "site", "path": "index.html"}),
            "app",
            "action",
            "read",
        ),
        (
            "search_sources",
            json!({"query": "notes"}),
            "research_lookup",
            "scope",
            "session_sources",
        ),
    ];
    for (index, (alias, args, _, _, _)) in cases.iter().enumerate() {
        let mut step = step(&format!("alias-{index}"));
        let mut def = definition(alias);
        if let Some(action) = args.get("action") {
            def.parameters["properties"]["action"] = json!({"type": "string", "enum": [action]});
        }
        step.tools = vec![def];
        step.messages.push(ChatMessage {
            role: "assistant".into(),
            tool_calls: Some(vec![ToolCall {
                id: "call-1".into(),
                name: (*alias).into(),
                arguments: args.to_string(),
            }]),
            ..Default::default()
        });
        step.messages.push(ChatMessage {
            role: "tool".into(),
            tool_call_id: Some("call-1".into()),
            content: "done".into(),
            ..Default::default()
        });
        fixture.record(&step);
    }
    let (report, rows) = fixture.export();
    assert_eq!(report.turns_kept, cases.len());
    for (row, (_, original_args, tool, selector, action)) in rows.iter().zip(&cases) {
        let function = &row["messages"][2]["tool_calls"][0]["function"];
        assert_eq!(function["name"], *tool);
        let args: Value = serde_json::from_str(function["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args[*selector], *action);
        for (key, value) in original_args.as_object().unwrap() {
            if key.as_str() != *selector {
                assert_eq!(args[key], *value);
            }
        }
        let def = &row["tools"][0]["function"];
        assert_eq!(def["name"], *tool);
        assert_eq!(
            def["parameters"]["properties"][*selector]["enum"],
            json!([action])
        );
    }
}

#[test]
fn groups_complete_turns_and_preserves_final_request_seen_results() {
    let fixture = Fixture::new();
    let mut first = step("turn-1");
    first.response.content.clear();
    first.response.tool_calls = vec![call("fs")];
    first.response.finish = "tool_calls".into();
    first.tool_results = vec![tool_result("shortened model-visible notes")];
    fixture.record(&first);
    let mut last = step("turn-1");
    last.round = 1;
    last.system = "Final round system".into();
    last.messages.extend([
        ChatMessage {
            role: "assistant".into(),
            tool_calls: Some(first.response.tool_calls.clone()),
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            tool_call_id: Some("call-1".into()),
            content: first.tool_results[0].seen.clone(),
            ..Default::default()
        },
    ]);
    fixture.record(&step("turn-2")); // Interleaved turns must not split turn-1.
    fixture.record(&last);
    let (report, rows) = fixture.export();
    assert_eq!(report.steps_read, 3);
    assert_eq!(report.turns_kept, 2);
    assert_eq!(rows[0]["messages"].as_array().unwrap().len(), 5);
    assert_eq!(rows[0]["messages"][0]["content"], "Final round system");
    assert_eq!(rows[0]["messages"][3]["tool_call_id"], "call-1");
    assert_eq!(
        rows[0]["messages"][3]["content"],
        first.tool_results[0].seen
    );
    assert_eq!(rows[0]["messages"][4]["content"], last.response.content);
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("FULL PRIVATE")
    );
    assert_eq!(rows[0]["tools"][0]["type"], "function");
    assert_eq!(rows[0]["tools"][0]["function"]["name"], "fs");
    assert_eq!(
        rows[0]["tools"][0]["function"]["description"],
        last.tools[0].description
    );
}

#[test]
fn success_rejects_denials_nonstop_and_labels_but_allows_recovered_errors() {
    let mut fixture = Fixture::new();
    let rejected = fixture.record(&step("rejected"));
    fixture
        .store
        .add_label(&rejected, "rejected", "retry")
        .unwrap();
    for finish in ["tool_calls", "length", "error"] {
        let mut step = step(finish);
        step.response.finish = finish.into();
        fixture.record(&step);
    }
    for (index, result) in [
        "denied by the user · ask instead",
        "denied by policy: no",
        "blocked: forbidden",
        "refused: forbidden",
        "path escapes workspace",
        "path denied: read-only",
        "privilege escalation is blocked: sudo",
    ]
    .iter()
    .enumerate()
    {
        let mut step = step(&format!("denied-{index}"));
        step.tool_results = vec![tool_result(result)];
        fixture.record(&step);
    }
    let mut earlier = step("recovered");
    earlier.response.finish = "tool_calls".into();
    earlier.tool_results = vec![tool_result("unknown file: missing.txt")];
    fixture.record(&earlier);
    let mut recovered = step("recovered");
    recovered.round = 1;
    fixture.record(&recovered);
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.dropped["not_successful"], 11);
    fixture.options.filter = Filter::All;
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 11);
    assert_eq!(report.dropped["not_successful"], 1);
}

#[test]
fn license_is_checked_for_every_step_and_explicit_patterns_override() {
    let mut fixture = Fixture::new();
    fixture.record(&step("local"));
    for backend in ["OpenAI", "Codex", "OpenCode Go", "OpenRouter"] {
        let mut step = step(backend);
        step.backend = backend.into();
        step.model = "qwen/model".into();
        fixture.record(&step);
    }
    let mut mixed = step("mixed");
    fixture.record(&mixed);
    mixed.backend = "OpenAI".into();
    mixed.round = 1;
    fixture.record(&mixed);
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.dropped["license"], 5);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("no distillable metadata"))
    );
    fixture.options.models.push("openai:qwen/*".into());
    fixture.options.models.push("openrouter:qwen/?odel".into());
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 4);
    assert_eq!(report.dropped["license"], 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("provider's terms"))
    );
}

#[test]
fn distillable_requires_positive_openrouter_catalog_metadata() {
    let fixture = Fixture::new();
    for (backend, model) in [
        ("OpenRouter", "trainable"),
        ("OpenRouter", "forbidden"),
        ("OpenRouter", "unknown"),
        ("OpenAI", "trainable"),
    ] {
        let mut step = step(&format!("{backend}-{model}"));
        step.backend = backend.into();
        step.model = model.into();
        fixture.record(&step);
    }
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 0);
    let catalog = BTreeMap::from([
        ("trainable".into(), Some(true)),
        ("forbidden".into(), Some(false)),
        ("unknown".into(), None),
    ]);
    let (report, _) = fixture.export_with(&Scrubber::new(Vec::new()), &catalog);
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.dropped["license"], 3);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("no distillable metadata"))
    );
}

#[test]
fn aliases_rewrite_calls_definitions_and_batch_children_unknown_tools_drop() {
    let mut fixture = Fixture::new();
    fixture.options.filter = Filter::All;
    let mut alias = step("alias");
    alias.tools = vec![definition("fs_read"), definition("fs_write")];
    alias.messages.push(ChatMessage {
        role: "assistant".into(),
        tool_calls: Some(vec![call("fs_read")]),
        ..Default::default()
    });
    alias.response.tool_calls = vec![ToolCall {
        id: "batch-1".into(),
        name: "batch".into(),
        arguments: json!({"calls": [{"tool": "web_search", "arguments": {"query": "notes"}}]})
            .to_string(),
    }];
    alias.response.finish = "tool_calls".into();
    fixture.record(&alias);
    let mut unknown_call = step("unknown-call");
    unknown_call.response.tool_calls = vec![call("vanished_tool")];
    fixture.record(&unknown_call);
    let mut unknown_def = step("unknown-def");
    unknown_def.tools = vec![definition("vanished_tool")];
    fixture.record(&unknown_def);
    let mut unknown_earlier = step("unknown-earlier");
    unknown_earlier.tools = unknown_def.tools;
    fixture.record(&unknown_earlier);
    fixture.record(&step("unknown-earlier"));
    let (report, rows) = fixture.export();
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.dropped["stale_schema"], 3);
    let function = &rows[0]["messages"][2]["tool_calls"][0]["function"];
    assert_eq!(function["name"], "fs");
    let args: Value = serde_json::from_str(function["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["action"], "read");
    let batch = &rows[0]["messages"][3]["tool_calls"][0]["function"];
    let args: Value = serde_json::from_str(batch["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["calls"][0]["tool"], "search");
    assert_eq!(args["calls"][0]["arguments"]["mode"], "web");
    let tools = rows[0]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "fs");
    assert_eq!(
        tools[0]["function"]["parameters"]["properties"]["action"]["enum"],
        json!(["read", "write"])
    );
}

#[test]
fn evals_exclude_normalized_containment_all_task_turns_and_eight_gram_overlap() {
    let fixture = Fixture::new();
    fs::write(
        fixture.options.evals.join("single.toml"),
        "prompt = 'HELD OUT task'\n[check]\nanswer_contains = ['ok']\n",
    )
    .unwrap();
    fs::write(fixture.options.evals.join("multi.toml"),
        "turns = ['first eval prompt', 'one two three four five six seven eight nine ten eleven twelve']\n[check]\nanswer_contains = ['ok']\n").unwrap();
    for (index, user) in [
        "Please solve HELD\n   out TASK now",
        "Now do first EVAL prompt please",
        // 2/3 user 8-grams overlap, but neither string contains the other.
        "prefix one two three four five six seven eight nine",
        // Only 1/3 user 8-grams overlap: it must survive.
        "prefix one two three four five six seven eight suffix",
        "An unrelated real-world prompt",
    ]
    .iter()
    .enumerate()
    {
        let mut step = step(&format!("turn-{index}"));
        step.messages = vec![ChatMessage::text("user", *user)];
        fixture.record(&step);
    }
    let (report, _) = fixture.export();
    assert_eq!(report.turns_kept, 2);
    assert_eq!(report.dropped["eval_match"], 3);
}

#[test]
fn missing_evals_fail_closed() {
    let mut fixture = Fixture::new();
    fixture.options.evals = fixture.root.join("absent");
    fixture.record(&step("turn"));
    assert!(
        export(
            &fixture.store,
            &fixture.options,
            &Scrubber::new(Vec::new()),
            &BTreeMap::new()
        )
        .is_err()
    );
    assert!(!fixture.options.out.exists());
}

#[test]
fn fresh_credentials_rescrub_every_exported_string_and_report_once_per_turn() {
    let fixture = Fixture::new();
    let mut step = step("secret");
    let secret = "newly-recognized-credential";
    step.system = format!("Do not reveal {secret}");
    step.tools[0].description = secret.into();
    step.response.content = format!("answer includes {secret}");
    step.messages[0].content = format!("question includes {secret}");
    fixture.record(&step);
    let (report, rows) = fixture.export_with(&Scrubber::new(vec![secret.into()]), &BTreeMap::new());
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.scrub_flagged, 1);
    let encoded = serde_json::to_string(&rows).unwrap();
    assert!(!encoded.contains(secret));
    assert!(encoded.contains("[REDACTED]"));
    let saved: Value =
        serde_json::from_slice(&fs::read(fixture.options.out.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(saved, serde_json::to_value(report).unwrap());
}

#[test]
fn sharegpt_has_system_human_gpt_and_tool_entries_with_json_calls() {
    let mut fixture = Fixture::new();
    fixture.options.format = Format::Sharegpt;
    let mut step = step("share");
    step.messages.push(ChatMessage {
        role: "assistant".into(),
        tool_calls: Some(vec![call("fs")]),
        ..Default::default()
    });
    step.messages.push(ChatMessage {
        role: "tool".into(),
        tool_call_id: Some("call-1".into()),
        content: "model-visible notes".into(),
        ..Default::default()
    });
    fixture.record(&step);
    let (_, rows) = fixture.export();
    let entries = rows[0]["conversations"].as_array().unwrap();
    let roles: Vec<_> = entries
        .iter()
        .map(|entry| entry["from"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "human", "gpt", "tool", "gpt"]);
    let call: Value = serde_json::from_str(entries[2]["value"].as_str().unwrap()).unwrap();
    assert_eq!(call["tool_calls"][0]["function"]["name"], "fs");
    assert_eq!(entries[3]["value"], "model-visible notes");
}

#[test]
fn dpo_pairs_retry_with_successful_replacement_not_other_sessions_or_failed_turns() {
    let mut fixture = Fixture::new();
    fixture.options.format = Format::Dpo;
    fixture.options.filter = Filter::All; // DPO still requires successful chosen turns.
    let mut rejected = step("rejected");
    rejected.response.content = "bad answer".into();
    let id = fixture.record(&rejected);
    fixture.store.add_label(&id, "rejected", "retry").unwrap();
    let mut unrelated = step("other-session");
    unrelated.session_id = "session-2".into();
    fixture.record(&unrelated);
    let mut replacement = step("replacement");
    replacement.response.content = "good answer".into();
    fixture.record(&replacement);
    let id = fixture.record(&step("second-rejected"));
    fixture.store.add_label(&id, "rejected", "retry").unwrap();
    let mut failed = step("failed-replacement");
    failed.response.finish = "length".into();
    fixture.record(&failed);
    fixture.record(&step("not-a-retry"));
    let (report, rows) = fixture.export();
    assert_eq!(report.turns_kept, 1);
    assert_eq!(report.dropped["not_successful"], 3);
    assert_eq!(rows[0]["chosen"], "good answer");
    assert_eq!(rows[0]["rejected"], "bad answer");
    assert_eq!(rows[0]["prompt"].as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["prompt"][0]["role"], "system");
    assert_eq!(rows[0]["prompt"][1]["role"], "user");
}

#[test]
fn dpo_rejected_side_must_also_pass_license_and_eval_safety() {
    let mut fixture = Fixture::new();
    fixture.options.format = Format::Dpo;
    let mut rejected = step("rejected");
    rejected.backend = "OpenAI".into();
    let id = fixture.record(&rejected);
    fixture.store.add_label(&id, "rejected", "retry").unwrap();
    fixture.record(&step("replacement"));
    let (report, rows) = fixture.export();
    assert_eq!(report.turns_kept, 0);
    assert!(rows.is_empty());
}

#[test]
fn escaped_credentials_in_json_arguments_are_rescrubbed_in_both_chat_formats() {
    for format in [Format::ChatJsonl, Format::Sharegpt] {
        let mut fixture = Fixture::new();
        fixture.options.format = format;
        let secret = "credential-\"with\\escapes";
        let mut step = step("escaped-secret");
        let mut call = call("fs");
        call.arguments = json!({"action": "read", "path": secret}).to_string();
        step.messages.push(ChatMessage {
            role: "assistant".into(),
            tool_calls: Some(vec![call]),
            ..Default::default()
        });
        fixture.record(&step);
        let (report, rows) =
            fixture.export_with(&Scrubber::new(vec![secret.into()]), &BTreeMap::new());
        assert_eq!(report.scrub_flagged, 1);
        let message = if format == Format::ChatJsonl {
            rows[0]["messages"][2].clone()
        } else {
            serde_json::from_str(rows[0]["conversations"][2]["value"].as_str().unwrap()).unwrap()
        };
        let args: Value = serde_json::from_str(
            message["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args["path"], "[REDACTED]");
    }
}

#[test]
fn since_selects_whole_turn_even_when_its_first_step_precedes_the_boundary() {
    let mut fixture = Fixture::new();
    let first = fixture.record(&step("crosses-midnight"));
    let mut last = step("crosses-midnight");
    last.round = 1;
    let last = fixture.record(&last);
    let index = rusqlite::Connection::open(fixture.root.join("traces/index.db")).unwrap();
    index
        .execute(
            "UPDATE steps SET at = '2026-09-30T23:59:59+00:00' WHERE id = ?1",
            [first],
        )
        .unwrap();
    index
        .execute(
            "UPDATE steps SET at = '2026-10-01T00:00:01+00:00' WHERE id = ?1",
            [last],
        )
        .unwrap();
    fixture.options.since = Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
    let (report, _) = fixture.export();
    assert_eq!(report.steps_read, 2);
    assert_eq!(report.turns_kept, 1);
}

#[test]
fn split_ratio_space_date_filters_and_saved_report() {
    let mut fixture = Fixture::new();
    for index in 0..20 {
        fixture.record(&step(&format!("turn-{index}")));
    }
    let mut elsewhere = step("other-space");
    elsewhere.space_id = "space-2".into();
    fixture.record(&elsewhere);
    fixture.options.space_id = Some("space-1".into());
    fixture.options.since = Some(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());
    fixture.options.split = true;
    let (report, rows) = fixture.export();
    assert_eq!(report.steps_read, 20);
    assert_eq!(report.turns_kept, 20);
    assert_eq!(
        read_jsonl(&fixture.options.out.join("train.jsonl")),
        rows[..18]
    );
    assert_eq!(
        read_jsonl(&fixture.options.out.join("valid.jsonl")),
        rows[18..]
    );
    fixture.options.since = Some(NaiveDate::from_ymd_opt(2999, 1, 1).unwrap());
    let (report, _) = fixture.export();
    assert_eq!(report.steps_read, 0);
    assert_eq!(report.turns_kept, 0);
}
