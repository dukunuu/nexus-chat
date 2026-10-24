use super::*;
use crate::interactive::{Document, Node, Session, StreamParser, merge_program};
use crate::provider::ToolDef;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<(String, String)>>,
    result: String,
    status: String,
}
impl ToolExecutor for Fake {
    fn defs(&self) -> Vec<ToolDef> {
        [
            "files",
            "search",
            "fetch_url",
            "fs",
            "deck",
            "shell",
            "task",
            "batch",
            "media",
            "custom__read",
        ]
        .into_iter()
        .map(|name| ToolDef {
            name: name.into(),
            description: String::new(),
            parameters: json!({}),
        })
        .collect()
    }
    fn is_read_only(&self, name: &str, args: &str) -> bool {
        name == "custom__read" || crate::tools::is_read_only_tool(name, args)
    }
    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        self.calls.lock().unwrap().push((name.into(), args.into()));
        Box::pin(std::future::ready((
            self.result.clone(),
            self.status.clone(),
        )))
    }
    fn supports_images(&self) -> bool {
        false
    }
    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}
struct Approval {
    decision: Decision,
    requests: Mutex<Vec<ApprovalRequest>>,
}
impl Approver for Approval {
    fn request(
        &self,
        request: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(std::future::ready(self.decision))
    }
}

const QUERY: &str = r#"root = Panel("Files", [Input($name,"File"), Text(q.text), Buttons([Button("Refresh", @Run(q))])])
$name = "a"
q = Query("files", {action:"read",name:$name}, {text:"Defaults"}, 1)"#;
fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}
fn text(session: &Session) -> String {
    let Node::Text { text, .. } = &session.document().nodes()[1] else {
        panic!("text")
    };
    session.text(text).unwrap()
}
fn fake(result: &str) -> Arc<Fake> {
    Arc::new(Fake {
        result: result.into(),
        ..Fake::default()
    })
}
async fn run(call: &Call, executor: Arc<Fake>) -> Outcome {
    execute(call, "p1-1", executor, None, &Scrubber::new(Vec::new())).await
}

#[tokio::test]
async fn activation_defaults_loading_results_debounce_and_stale_arguments() {
    let now = Instant::now();
    let mut panel = session(QUERY);
    let mut count = 0;
    assert_eq!(text(&panel), "Defaults");
    assert!(panel.next_data_call(now, &mut count).unwrap().is_none());
    assert!(panel.needs_data_load());
    panel.interact_data(now);
    let call = panel.next_data_call(now, &mut count).unwrap().unwrap();
    assert_eq!(text(&panel), "Defaults");
    assert_eq!(panel.data_hint(), Some("Loading data"));
    assert_eq!(panel.live.values()["__openui_loading"]["q"], true);
    panel.input("name", "b".into()).unwrap();
    let executor = fake(r#"{"text":"Old"}"#);
    let result = run(&call, Arc::clone(&executor)).await;
    panel.finish_data_call(&call, &result, now).unwrap();
    assert_eq!(
        text(&panel),
        "Defaults",
        "stale results must not replace defaults"
    );
    assert!(panel.next_data_call(now, &mut count).unwrap().is_none());
    panel.input("name", "c".into()).unwrap();
    assert!(
        panel
            .next_data_call(now + Duration::from_millis(200), &mut count)
            .unwrap()
            .is_none()
    );
    assert!(
        panel
            .next_data_call(now + Duration::from_millis(499), &mut count)
            .unwrap()
            .is_none()
    );
    let call = panel
        .next_data_call(now + DEBOUNCE + Duration::from_millis(200), &mut count)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&call.arguments).unwrap()["name"],
        "c"
    );
    let result = run(&call, fake(r#"{"text":"Loaded"}"#)).await;
    panel
        .finish_data_call(&call, &result, now + DEBOUNCE)
        .unwrap();
    assert_eq!(text(&panel), "Loaded");
    assert!(panel.data_hint().is_none());
    panel.activate_button(2, 0).unwrap();
    let call = panel
        .next_data_call(now + Duration::from_secs(1), &mut count)
        .unwrap()
        .unwrap();
    assert_eq!(text(&panel), "Loaded");
    assert_eq!(panel.live.values()["__openui_refetching"]["q"], true);
    let failure = outcome(
        "fetch failed: unavailable",
        false,
        &Scrubber::new(Vec::new()),
    );
    panel.finish_data_call(&call, &failure, now).unwrap();
    assert_eq!(text(&panel), "Loaded");
    assert!(panel.data_hint().unwrap().contains("error"));
    assert_eq!(count, 3);
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refresh_minimum_idle_timeout_and_both_call_caps() {
    let now = Instant::now();
    let mut panel = session(QUERY);
    panel.load_data(now);
    let mut count = 0;
    let call = panel.next_data_call(now, &mut count).unwrap().unwrap();
    let result = run(&call, fake("ok")).await;
    panel.finish_data_call(&call, &result, now).unwrap();
    assert!(
        panel
            .next_data_call(now + Duration::from_secs(14), &mut count)
            .unwrap()
            .is_none()
    );
    let call = panel
        .next_data_call(now + Duration::from_secs(15), &mut count)
        .unwrap()
        .unwrap();
    panel
        .finish_data_call(&call, &result, now + Duration::from_secs(15))
        .unwrap();
    assert!(
        panel
            .next_data_call(now + IDLE, &mut count)
            .unwrap()
            .is_none()
    );
    panel.interact_data(now + IDLE);
    let call = panel
        .next_data_call(now + IDLE, &mut count)
        .unwrap()
        .unwrap();
    panel.finish_data_call(&call, &result, now + IDLE).unwrap();
    panel.live.count = PANEL_CALL_CAP;
    panel.activate_button(2, 0).unwrap();
    assert!(
        panel
            .next_data_call(now + IDLE, &mut count)
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    panel.live.count = 0;
    count = SESSION_CALL_CAP;
    assert!(
        panel
            .next_data_call(now + IDLE, &mut count)
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    let too_many = format!(
        "root = Panel(\"Timers\", [Text(\"x\")])\n{}",
        (0..6)
            .map(|i| format!("q{i} = Query(\"files\", {{action:\"read\",name:\"a\"}}, null, 15)"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(Document::parse(&too_many).is_err());
}

#[tokio::test]
async fn literal_dependencies_only_and_restored_streams_are_inert() {
    let source = QUERY
        .replace("name:$name", "name:derived")
        .replace("q = Query", "derived = $name\nq = Query");
    let now = Instant::now();
    let mut panel = session(&source);
    let mut count = 0;
    panel.interact_data(now);
    let call = panel.next_data_call(now, &mut count).unwrap().unwrap();
    let result = run(&call, fake("ok")).await;
    panel.finish_data_call(&call, &result, now).unwrap();
    panel.input("name", "changed".into()).unwrap();
    assert!(
        panel
            .next_data_call(now + Duration::from_secs(1), &mut count)
            .unwrap()
            .is_none()
    );
    for document in [
        Document::parse(QUERY).unwrap(),
        StreamParser::default().set(QUERY).unwrap(),
    ] {
        let mut restored = Session::new(document).unwrap();
        assert!(restored.next_data_call(now, &mut count).unwrap().is_none());
    }
    let merged = merge_program(QUERY, "root = Panel(\"Patched\", [Text(\"x\")])").unwrap();
    assert!(
        merged.contains("q = Query"),
        "unreferenced queries remain statements"
    );
}

#[tokio::test]
async fn paid_queries_wait_for_load_and_query_writes_and_escape_tools_are_refused() {
    let now = Instant::now();
    let mut count = 0;
    for replacement in [
        "\"search\", {mode:\"web\",query:$name}",
        "\"files\", {action:\"search\",query:$name}",
        "\"custom__read\", {query:$name}",
    ] {
        let mut panel =
            session(&QUERY.replace("\"files\", {action:\"read\",name:$name}", replacement));
        panel.interact_data(now);
        assert!(panel.next_data_call(now, &mut count).unwrap().is_none());
        assert!(panel.needs_data_load());
        panel.load_data(now);
        let call = panel.next_data_call(now, &mut count).unwrap().unwrap();
        assert!(run(&call, fake("ok")).await.success);
    }
    for (tool, args) in [
        ("fs", json!({"action":"write","path":"x","content":"x"})),
        ("shell", json!({"action":"log"})),
        ("task", json!({})),
        ("batch", json!({"calls":[]})),
        ("media", json!({"action":"generate_image"})),
    ] {
        let executor = fake("must not run");
        let call = Call {
            id: "q".into(),
            tool: tool.into(),
            arguments: args.to_string(),
            query: true,
            token: 0,
        };
        let result = run(&call, Arc::clone(&executor)).await;
        assert!(!result.success, "{tool}");
        assert!(executor.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn mutations_require_approval_deny_stops_tail_and_success_resumes_it() {
    let source = r#"root = Panel("Write", [Text(m.status), Text($done), Buttons([Button("Write", Action([@Run(m), @Set($done,true), @Set($received,m.data.text)]))])])
$done = false
$received = ""
m = Mutation("fs", {action:"write",path:"a",content:"data"})"#;
    for decision in [Decision::Allow, Decision::Deny] {
        let now = Instant::now();
        let mut panel = session(source);
        let mut count = 0;
        assert!(panel.next_data_call(now, &mut count).unwrap().is_none());
        assert_eq!(panel.document().plain_text().lines().nth(1), Some("idle"));
        panel.activate_button(2, 0).unwrap();
        let call = panel.next_data_call(now, &mut count).unwrap().unwrap();
        assert!(panel.activate_button(2, 0).is_err());
        let executor = fake(r#"{"text":"written"}"#);
        let approval = Arc::new(Approval {
            decision,
            requests: Mutex::default(),
        });
        let result = execute(
            &call,
            "p4-2",
            executor.clone(),
            Some(approval.clone()),
            &Scrubber::new(Vec::new()),
        )
        .await;
        panel.finish_data_call(&call, &result, now).unwrap();
        let requests = approval.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].summary.contains("p4-2"));
        assert!(requests[0].summary.contains("content"));
        assert!(!requests[0].allow_session);
        assert_eq!(
            panel.value("done"),
            if decision == Decision::Allow {
                "true"
            } else {
                "false"
            }
        );
        assert_eq!(
            panel.value("received"),
            if decision == Decision::Allow {
                "written"
            } else {
                ""
            }
        );
        assert_eq!(
            executor.calls.lock().unwrap().len(),
            usize::from(decision == Decision::Allow)
        );
        assert_eq!(
            panel.live.values()["m"]["status"],
            if decision == Decision::Allow {
                "success"
            } else {
                "error"
            }
        );
    }
}

#[tokio::test]
async fn tool_and_timer_expressions_null_refresh_and_program_status_members() {
    let source = r#"root = Panel("Status", [Text(__openui_loading.q), Text(__openui_errors.q)])
tool = "files"
$seconds = 15.5
q = Query(tool, {action:"read",name:"a"}, {text:"Defaults"}, $seconds)"#;
    let now = Instant::now();
    let mut panel = session(source);
    assert_eq!(panel.document.calls[0].refresh, Some(15.5));
    assert!(
        session(&QUERY.replace(", 1)", ", null)")).document.calls[0]
            .refresh
            .is_none()
    );
    panel.interact_data(now);
    let call = panel.next_data_call(now, &mut 0).unwrap().unwrap();
    let Node::Text { text, .. } = &panel.document.nodes()[0] else {
        panic!("text")
    };
    assert_eq!(panel.text(text).unwrap(), "true");
    let failure = outcome("fetch failed: offline", false, &Scrubber::new(Vec::new()));
    panel.finish_data_call(&call, &failure, now).unwrap();
    let Node::Text { text, .. } = &panel.document.nodes()[1] else {
        panic!("text")
    };
    assert_eq!(panel.text(text).unwrap(), "fetch failed: offline");
}

#[tokio::test]
async fn result_bounds_plain_text_redaction_and_read_only_mutation() {
    let executor = fake("one\ntwo");
    let call = Call {
        id: "m".into(),
        tool: "files".into(),
        arguments: json!({"action":"read","name":"a"}).to_string(),
        query: false,
        token: 0,
    };
    let result = run(&call, executor).await;
    assert!(result.success, "read-only mutation needs no approver");
    assert_eq!(
        result.value,
        json!({"text":"one\ntwo","lines":["one","two"]})
    );
    let paid = Call {
        tool: "task".into(),
        arguments: "{}".into(),
        ..call.clone()
    };
    let executor = fake("must not run an LLM without approval");
    assert!(!run(&paid, executor.clone()).await.success);
    assert!(executor.calls.lock().unwrap().is_empty());
    let many = json!((0..300).collect::<Vec<_>>()).to_string();
    assert_eq!(
        outcome(&many, true, &Scrubber::new(Vec::new()))
            .value
            .as_array()
            .unwrap()
            .len(),
        200
    );
    let result = outcome(&"界".repeat(10000), true, &Scrubber::new(Vec::new()));
    assert_eq!(result.text.chars().count(), 8000);
    assert!(result.value["text"].as_str().unwrap().len() <= 1024);
    let scrubber = Scrubber::new(vec!["private-key-value".into()]);
    let arguments = r#"{"api_key":"hidden","name":"private-key-value"}"#;
    let clean = redacted_arguments(arguments, &scrubber);
    assert!(!clean.contains("hidden") && !clean.contains("private-key-value"));
    let escaped = "private-\"key\\value";
    let scrubber = Scrubber::new(vec![escaped.into()]);
    let encoded = json!({"name":escaped}).to_string();
    assert_eq!(
        serde_json::from_str::<Value>(&redacted_arguments(&encoded, &scrubber)).unwrap()["name"],
        "[REDACTED]"
    );
    let result = outcome(&encoded, true, &scrubber);
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap()["name"],
        "[REDACTED]"
    );
    let executor = fake("never");
    let call = Call {
        arguments: arguments.into(),
        ..call
    };
    assert!(
        !execute(&call, "panel", executor.clone(), None, &scrubber)
            .await
            .success
    );
    assert!(executor.calls.lock().unwrap().is_empty());
}
