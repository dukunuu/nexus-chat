use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::tools::{ApprovalRequest, Approver, Decision, ToolExecutor};
use crate::trace::{Scrubber, TraceContext, TraceStore};

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("nexus-subagent-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("repo")).unwrap();
        Self(path)
    }

    fn root(&self) -> PathBuf {
        self.0.join("repo")
    }

    fn toolbox(&self) -> ToolBox {
        let mut toolbox = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".into(),
            Vec::new(),
            None,
            None,
            None,
        );
        toolbox.workspace = Some(Arc::new(Workspace::open(&self.root(), Vec::new()).unwrap()));
        toolbox.session_id = "12345678-aaaa-bbbb-cccc-dddddddddddd".into();
        toolbox.subagent_environment = format!(
            "## Environment\nworkspace {}",
            toolbox.workspace.as_ref().unwrap().root().display()
        );
        toolbox.require_approval = true;
        toolbox
    }

    fn init_git(&self) {
        command(&self.root(), &["init", "-q"]);
        command(&self.root(), &["config", "user.name", "Test"]);
        command(
            &self.root(),
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(self.root().join("tracked.txt"), "parent\n").unwrap();
        command(&self.root(), &["add", "tracked.txt"]);
        command(&self.root(), &["commit", "-qm", "initial"]);
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

struct Seen {
    model: String,
    messages: Vec<ChatMessage>,
    params: ChatParams,
    tools: Vec<ToolDef>,
    final_round: bool,
}

struct Fake {
    replies: Mutex<VecDeque<Round>>,
    seen: Mutex<Vec<Seen>>,
}

impl Fake {
    fn new(replies: Vec<Round>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::default(),
        })
    }
}

impl Provider for Fake {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>> {
        self.seen.lock().unwrap().push(Seen {
            model: request.model.into(),
            messages: request.messages.to_vec(),
            params: request.params.clone(),
            tools: request.tools.to_vec(),
            final_round: request.final_round,
        });
        Box::pin(std::future::ready(Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider request"))))
    }
}

fn reply(text: &str, calls: Vec<ToolCall>) -> Round {
    Round {
        content: text.into(),
        reasoning: None,
        calls,
        usage: Some(Usage {
            prompt_tokens: 6,
            completion_tokens: 4,
            total_tokens: 10,
            cost: Some(0.125),
            ..Default::default()
        }),
    }
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

fn context(
    provider: Arc<dyn Provider>,
    params: ChatParams,
) -> (Arc<Context>, mpsc::UnboundedReceiver<StreamEvent>) {
    let (events, rx) = mpsc::unbounded_channel();
    (
        Arc::new(Context {
            provider,
            model: "session-model".into(),
            params,
            events,
        }),
        rx,
    )
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end context, accounting and trace assertions.
async fn read_only_task_is_fresh_restricted_capped_accounted_and_traced() {
    let temp = Temp::new();
    fs::write(temp.root().join("tracked.txt"), "useful finding\n").unwrap();
    let fake = Fake::new(vec![
        reply(
            "intermediate output must stay private",
            vec![
                call("fs", json!({"action":"read", "path":"tracked.txt"})),
                call("shell", json!({"action":"run", "cmd":"touch forbidden"})),
                call("fs_write", json!({"path":"forbidden", "content":"bad"})),
                call("task", json!({"prompt":"do not nest"})),
            ],
        ),
        reply(&format!("HEAD{}TAIL", "é".repeat(8_000)), Vec::new()),
    ]);
    let store =
        Arc::new(TraceStore::open(&temp.0.join("trace"), Scrubber::new(Vec::new())).unwrap());
    let params = ChatParams {
        prompt_cache_key: Some("parent-cache".into()),
        reasoning_effort: Some("high".into()),
        temperature: Some(0.4),
        trace: Some(TraceContext {
            store: store.clone(),
            session_id: "parent-session".into(),
            space_id: "space".into(),
            turn_id: "parent-turn".into(),
            backend: "fake".into(),
        }),
        ..Default::default()
    };
    let (ctx, mut events) = context(fake.clone(), params);
    let toolbox = Arc::new(temp.toolbox()).with_subagents(ctx).unwrap();
    let (result, _) = toolbox.run("task", r#"{"prompt":"find callers"}"#).await;
    let (header, report) = result.split_once('\n').unwrap();
    assert_eq!(header, "sub-agent report (1 steps, 20 tokens):");
    assert!(report.starts_with("HEAD") && report.ends_with("TAIL"));
    assert!(report.chars().count() <= REPORT_CAP);
    assert!(!result.contains("intermediate output"));
    assert!(!temp.root().join("forbidden").exists());
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[0].messages.len(), 2);
    assert_eq!(seen[0].messages[1].content, "find callers");
    assert_eq!(seen[0].model, "session-model");
    assert!(
        seen[0]
            .params
            .prompt_cache_key
            .as_ref()
            .unwrap()
            .starts_with("parent-cache:sub:")
    );
    assert_eq!(seen[0].params.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(seen[0].params.temperature, Some(0.4));
    assert!(seen[0].messages[0].content.contains("focused helper"));
    assert_eq!(
        seen[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["fs", "shell"]
    );
    assert_eq!(
        seen[0].tools[0].parameters["properties"]["action"]["enum"],
        json!(["list", "glob", "grep", "read"])
    );
    assert_eq!(
        seen[0].tools[1].parameters["properties"]["action"]["enum"],
        json!(["log"])
    );
    let outputs: Vec<_> = seen[1]
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect();
    assert!(outputs[0].content.contains("useful finding"));
    assert!(
        outputs[1..]
            .iter()
            .all(|message| message.content.contains("not available in this sub-agent"))
    );
    drop(seen);
    let mut tokens = 0;
    let mut cost = 0.0;
    let mut statuses = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            StreamEvent::Usage(usage) => {
                tokens += usage.total_tokens;
                cost += usage.cost.unwrap();
            }
            StreamEvent::Status(status) => statuses.push(status),
            other => panic!("child output leaked to parent: {other:?}"),
        }
    }
    assert_eq!(tokens, 20);
    assert!((cost - 0.25).abs() < f64::EPSILON);
    assert_eq!(statuses, ["sub-agent: 1/20", "sub-agent: 2/20"]);
    let steps = store.steps_for_session("parent-session").unwrap();
    assert_eq!(steps.len(), 2);
    for id in steps {
        let step = store.load_step(&id).unwrap();
        assert_eq!(step["turn_id"], "parent-turn");
        assert_eq!(step["labels"][0]["kind"], "subagent");
    }
}

struct Parallel {
    barrier: tokio::sync::Barrier,
    order: Mutex<Vec<String>>,
}

impl Provider for Parallel {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>> {
        Box::pin(async move {
            let prompt = &request.messages[1].content;
            self.order.lock().unwrap().push(format!("start {prompt}"));
            self.barrier.wait().await;
            tokio::time::sleep(Duration::from_millis(if prompt == "slow" { 40 } else { 1 })).await;
            self.order.lock().unwrap().push(format!("end {prompt}"));
            Ok(reply(prompt, Vec::new()))
        })
    }
}

#[tokio::test]
async fn read_only_tasks_overlap_but_results_remain_in_call_order() {
    let temp = Temp::new();
    let fake = Arc::new(Parallel {
        barrier: tokio::sync::Barrier::new(2),
        order: Mutex::default(),
    });
    let (ctx, _) = context(fake.clone(), ChatParams::default());
    let toolbox = Arc::new(temp.toolbox()).with_subagents(ctx).unwrap();
    let calls = vec![
        ("task".into(), json!({"prompt":"slow"}).to_string()),
        (
            "task".into(),
            json!({"prompt":"fast", "tools":"read-only"}).to_string(),
        ),
    ];
    assert_eq!(
        toolbox.effect("task", &calls[0].1),
        crate::tools::Effect::read_only()
    );
    assert!(
        toolbox
            .effect("task", r#"{"prompt":"write","tools":"workspace"}"#)
            .barrier
    );
    let results = tokio::time::timeout(Duration::from_secs(2), run_scheduled(&*toolbox, &calls))
        .await
        .unwrap();
    assert!(results[0].0.ends_with("slow"));
    assert!(results[1].0.ends_with("fast"));
    let order = fake.order.lock().unwrap();
    assert!(order[..2].iter().all(|event| event.starts_with("start ")));
    assert_eq!(&order[2..], ["end fast", "end slow"]);
}

#[derive(Default)]
struct Approval(Mutex<Vec<ApprovalRequest>>);

impl Approver for Approval {
    fn request(
        &self,
        request: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>> {
        self.0.lock().unwrap().push(request);
        Box::pin(std::future::ready(Decision::Allow))
    }
}

#[tokio::test]
async fn workspace_task_asks_once_and_commits_only_its_worktree_with_a_review_diff() {
    let temp = Temp::new();
    temp.init_git();
    // Neither staged nor unstaged user changes may enter the helper's commit.
    fs::write(temp.root().join("staged.txt"), "user staged\n").unwrap();
    command(&temp.root(), &["add", "staged.txt"]);
    fs::write(temp.root().join("tracked.txt"), "user unstaged\n").unwrap();
    let before = command(&temp.root(), &["diff", "--binary"]);
    let staged = command(&temp.root(), &["diff", "--cached", "--binary"]);
    let fake = Fake::new(vec![
        reply(
            "",
            vec![
                call(
                    "fs",
                    json!({"action":"write", "path":"new.txt", "content":"child change\n"}),
                ),
                call(
                    "fs",
                    json!({"action":"write", "path":temp.root().join("escape.txt"), "content":"escape"}),
                ),
            ],
        ),
        reply("Finished new.txt:1", Vec::new()),
    ]);
    let approver = Arc::new(Approval::default());
    let mut toolbox = temp.toolbox();
    toolbox.approver = Some(approver.clone());
    let (ctx, _) = context(fake.clone(), ChatParams::default());
    let toolbox = Arc::new(toolbox).with_subagents(ctx).unwrap();
    let (report, _) = toolbox
        .run(
            "task",
            r#"{"prompt":"write a file\nwith detail","tools":"workspace"}"#,
        )
        .await;
    assert!(report.contains("Finished new.txt:1"), "{report}");
    let review = report
        .lines()
        .find(|line| line.starts_with("changes on branch "))
        .unwrap();
    let branch = review
        .strip_prefix("changes on branch ")
        .unwrap()
        .split_once(':')
        .unwrap()
        .0;
    assert!(branch.starts_with("nexus/sub/12345678/"));
    assert!(review.ends_with("new.txt (+1 −0)"));
    assert!(report.contains("+child change"));
    assert_eq!(
        command(&temp.root(), &["show", &format!("{branch}:new.txt")]),
        "child change\n"
    );
    assert!(!temp.root().join("new.txt").exists());
    assert!(!temp.root().join("escape.txt").exists());
    assert_eq!(command(&temp.root(), &["diff", "--binary"]), before);
    assert_eq!(
        command(&temp.root(), &["diff", "--cached", "--binary"]),
        staged
    );
    assert_eq!(
        command(&temp.root(), &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    let approvals = approver.0.lock().unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].summary, "sub-agent (workspace): write a file");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["fs", "shell"]
    );
    assert!(
        !seen[0].messages[0]
            .content
            .contains(&temp.root().display().to_string())
    );
}

#[tokio::test]
async fn no_change_task_removes_worktree_and_auto_ask_is_denied_without_prompting() {
    let temp = Temp::new();
    temp.init_git();
    let fake = Fake::new(vec![
        reply(
            "",
            vec![call(
                "fs",
                json!({"action":"write", "path":"new.txt", "content":"must not happen"}),
            )],
        ),
        reply(
            "Report: I would have written new.txt, but approval was unavailable.",
            Vec::new(),
        ),
    ]);
    let approver = Arc::new(Approval::default());
    let mut toolbox = temp.toolbox();
    toolbox.auto_decider = Some(Arc::new(crate::decider::Chain::new(Vec::new(), Vec::new())));
    toolbox.approver = Some(approver.clone());
    let (ctx, _) = context(fake.clone(), ChatParams::default());
    let toolbox = Arc::new(toolbox).with_subagents(ctx).unwrap();
    let (report, _) = toolbox
        .run("task", r#"{"prompt":"try writing","tools":"workspace"}"#)
        .await;
    assert!(report.ends_with("no changes; worktree removed"), "{report}");
    assert_eq!(approver.0.lock().unwrap().len(), 1);
    assert!(!temp.root().join("new.txt").exists());
    let seen = fake.seen.lock().unwrap();
    let denial = seen[1]
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .unwrap();
    assert!(
        denial
            .content
            .contains("sub-agents cannot prompt; report what you would have done")
    );
    assert_eq!(
        command(&temp.root(), &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

#[tokio::test]
async fn unchanged_workspace_task_removes_its_worktree() {
    let temp = Temp::new();
    temp.init_git();
    let fake = Fake::new(vec![reply("Nothing to change", Vec::new())]);
    let mut toolbox = temp.toolbox();
    toolbox.approver = Some(Arc::new(Approval::default()));
    let (ctx, _) = context(fake, ChatParams::default());
    let toolbox = Arc::new(toolbox).with_subagents(ctx).unwrap();
    let before = command(&temp.root(), &["worktree", "list", "--porcelain"]);
    let (report, _) = toolbox
        .run("task", r#"{"prompt":"inspect only","tools":"workspace"}"#)
        .await;
    assert!(report.contains("Nothing to change"));
    assert!(report.ends_with("no changes; worktree removed"));
    assert_eq!(
        command(&temp.root(), &["worktree", "list", "--porcelain"]),
        before
    );
}

#[tokio::test]
async fn step_budget_forces_a_final_report_and_never_executes_final_calls() {
    let temp = Temp::new();
    let fake = Fake::new(vec![
        reply("", vec![call("fs", json!({"action":"list"}))]),
        reply(
            "Budget exhausted",
            vec![call("fs", json!({"action":"list"}))],
        ),
    ]);
    let (ctx, _) = context(fake.clone(), ChatParams::default());
    let toolbox = Arc::new(temp.toolbox()).with_subagents(ctx).unwrap();
    let (report, _) = toolbox
        .run("task", r#"{"prompt":"keep going","max_steps":1}"#)
        .await;
    assert!(report.starts_with("sub-agent report (1 steps, 20 tokens):"));
    assert!(report.ends_with("Budget exhausted"));
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(!seen[0].final_round && seen[1].final_round);
    assert!(
        seen.iter()
            .all(|request| request.tools.iter().all(|tool| tool.name != "task"))
    );
}

#[tokio::test]
async fn task_schema_defaults_validation_and_agent_only_availability() {
    let temp = Temp::new();
    let mut toolbox = temp.toolbox();
    let task = toolbox
        .defs()
        .into_iter()
        .find(|tool| tool.name == "task")
        .unwrap();
    assert_eq!(task.parameters["required"], json!(["prompt"]));
    assert_eq!(
        task.parameters["properties"]["tools"]["default"],
        "read-only"
    );
    assert_eq!(task.parameters["properties"]["max_steps"]["default"], 20);
    assert_eq!(task.parameters["properties"]["max_steps"]["maximum"], 40);
    for args in [
        json!({"prompt":"x", "max_steps":41}),
        json!({"prompt":"x", "max_steps":0}),
        json!({"prompt":"x", "tools":"all"}),
        json!({"prompt":""}),
    ] {
        assert_eq!(
            toolbox.run("task", &args.to_string()).await.1,
            "invalid arguments"
        );
    }
    toolbox.workspace = None;
    assert!(toolbox.defs().iter().all(|tool| tool.name != "task"));
}
