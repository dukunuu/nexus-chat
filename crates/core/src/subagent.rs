//! Focused nested agent loops with isolated histories and workspace branches.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::jobs::service::{Tree, git};
use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, ToolCall, ToolDef, Usage};
use crate::tools::{ToolBox, cap_head_tail, run_scheduled};
use crate::trace::{TraceResponse, TraceStep, TraceToolResult};
use crate::workspace::Workspace;

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);
const REPORT_CAP: usize = 6_000;

#[cfg(test)]
mod tests;

pub(crate) struct Request<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub params: &'a ChatParams,
    pub tools: &'a [ToolDef],
    pub final_round: bool,
}

pub(crate) struct Round {
    pub content: String,
    pub reasoning: Option<String>,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

pub(crate) trait Provider: Send + Sync {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>>;
}

impl Provider for OpenRouter {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>> {
        Box::pin(self.subagent_round(request))
    }
}

/// The session model, request settings and event sink captured for one parent turn.
pub struct Context {
    provider: Arc<dyn Provider>,
    model: String,
    params: ChatParams,
    events: mpsc::UnboundedSender<StreamEvent>,
}

impl Context {
    pub(crate) fn new(
        provider: OpenRouter,
        model: String,
        params: ChatParams,
        events: mpsc::UnboundedSender<StreamEvent>,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            model,
            params,
            events,
        }
    }
}

fn branch_name(session: &str, number: u64) -> String {
    let short: String = session
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("nexus/sub/{short}/{number}")
}

fn prepare_tree(toolbox: &ToolBox, number: &mut u64) -> Result<(Tree, Arc<Workspace>)> {
    let root = toolbox
        .workspace
        .as_ref()
        .context("task needs an agent workspace")?
        .root();
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/nexus/sub/",
        ],
    )?;
    let refs = String::from_utf8_lossy(&refs.stdout);
    while refs
        .lines()
        .any(|name| name == branch_name(&toolbox.session_id, *number))
    {
        *number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut tree = Tree::prepare(
        root,
        &std::env::temp_dir().join("nexus-subagents"),
        "subagent",
        &id,
    )?;
    let branch = branch_name(&toolbox.session_id, *number);
    git(&tree.path, &["branch", "-m", &branch])?;
    tree.branch = Some(branch);
    let path = tree
        .path
        .join(String::from_utf8_lossy(&prefix.stdout).trim());
    let workspace = Arc::new(Workspace::open(&path, Vec::new())?);
    Ok((tree, workspace))
}

fn finish_tree(tree: &Tree, number: u64) -> Result<String> {
    // Stage before collecting the patch so new files appear in the diff too.
    git(&tree.path, &["add", "-A"])?;
    let diff = tree.changes()?;
    let stats = git(&tree.path, &["diff", "HEAD", "--numstat"])?;
    let stats = String::from_utf8_lossy(&stats.stdout);
    let mut added = 0_u64;
    let mut deleted = 0_u64;
    let mut files = Vec::new();
    for line in stats.lines() {
        let mut fields = line.splitn(3, '\t');
        added += fields
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or(0);
        deleted += fields
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or(0);
        files.push(fields.next().unwrap_or_default());
    }
    tree.finish("subagent", &number.to_string())?;
    if diff.is_empty() {
        return Ok("no changes; worktree removed".into());
    }
    Ok(format!(
        "changes on branch {}: {} (+{added} −{deleted})\n{}",
        tree.branch.as_deref().context("sub-agent branch")?,
        files.join(", "),
        cap_head_tail(&diff, REPORT_CAP),
    ))
}

/// Run one task, returning only its final report and optional branch review.
pub(crate) async fn run(context: &Context, toolbox: &ToolBox, args: &Value) -> Result<String> {
    let mut number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    let writable = args["tools"] == "workspace";
    let prepared = if writable {
        Some(prepare_tree(toolbox, &mut number)?)
    } else {
        None
    };
    let mut child = toolbox.for_subagent(prepared.as_ref().map(|(_, workspace)| workspace.clone()));
    child.decision_context = Arc::new(std::sync::Mutex::new((
        toolbox.session_id.clone(),
        args["prompt"].as_str().expect("validated prompt").into(),
    )));
    let mut params = context.params.clone();
    params.prompt_cache_key = params
        .prompt_cache_key
        .map(|key| format!("{key}:sub:{number}"));
    let max_steps = usize::try_from(args["max_steps"].as_u64().unwrap_or(20))?;
    let workspace = child
        .workspace
        .as_ref()
        .context("task needs an agent workspace")?;
    let environment = toolbox.subagent_environment.replace(
        &toolbox
            .workspace
            .as_ref()
            .expect("agent workspace")
            .root()
            .display()
            .to_string(),
        &workspace.root().display().to_string(),
    );
    let mut system = ChatMessage::text(
        "system",
        format!(
            "{}\n\n{environment}\n\n{}\n\nSub-agent capabilities: {}. No task, proc, or user bridge tools are available. Workspace changes stay on a review branch; never try to merge them yourself.",
            crate::prompts::render(crate::prompts::SUBAGENT, &[]),
            crate::prompts::render(crate::prompts::AGENT, &[]),
            if writable {
                "fs and write-confined shell in an isolated worktree"
            } else {
                "read-only fs and shell log only"
            },
        ),
    );
    system.cache_breakpoint = crate::provider::model_needs_cache_breakpoints(&context.model);
    let messages = vec![
        system,
        ChatMessage::text("user", args["prompt"].as_str().expect("validated prompt")),
    ];
    let outcome = run_loop(context, &child, &params, messages, max_steps, number).await;
    let review = prepared
        .as_ref()
        .map(|(tree, _)| finish_tree(tree, number))
        .transpose()?;
    let mut report = outcome.unwrap_or_else(|error| format!("sub-agent failed: {error}"));
    if let Some(review) = review {
        report.push_str("\n\n");
        report.push_str(&review);
    }
    Ok(report)
}

async fn run_loop(
    context: &Context,
    toolbox: &ToolBox,
    params: &ChatParams,
    mut messages: Vec<ChatMessage>,
    max_steps: usize,
    number: u64,
) -> Result<String> {
    let tools = toolbox.defs();
    let mut tokens = 0;
    for step in 0..=max_steps {
        let _ = context.events.send(StreamEvent::Status(format!(
            "sub-agent: {}/{max_steps}",
            (step + 1).min(max_steps)
        )));
        let started = Instant::now();
        let round = context
            .provider
            .round(Request {
                model: &context.model,
                messages: &messages,
                params,
                tools: &tools,
                final_round: step == max_steps,
            })
            .await?;
        if let Some(usage) = round.usage {
            tokens += usage.total_tokens;
            let _ = context.events.send(StreamEvent::Usage(usage));
        }
        let done = round.calls.is_empty() || step == max_steps;
        let request = messages.clone();
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: round.content.clone(),
            reasoning_content: round.reasoning.clone(),
            tool_calls: (!round.calls.is_empty()).then(|| round.calls.clone()),
            ..Default::default()
        });
        let mut results = Vec::new();
        if !done {
            let pending: Vec<_> = round
                .calls
                .iter()
                .map(|call| (call.name.clone(), call.arguments.clone()))
                .collect();
            for (call, (full, _)) in round
                .calls
                .iter()
                .zip(run_scheduled(toolbox, &pending).await)
            {
                let seen = params
                    .tool_result_cap
                    .map_or_else(|| full.clone(), |cap| cap_head_tail(&full, cap));
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: seen.clone(),
                    tool_call_id: Some(call.id.clone()),
                    ..Default::default()
                });
                results.push(TraceToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    full,
                    seen,
                });
            }
        }
        record_step(
            context, params, &tools, request, &round, results, step, number, started,
        )
        .await;
        if done {
            return Ok(format!(
                "sub-agent report ({step} steps, {tokens} tokens):\n{}",
                cap_head_tail(&round.content, REPORT_CAP)
            ));
        }
    }
    unreachable!("the last round always returns a report")
}

#[allow(clippy::too_many_arguments)] // One trace record, mirroring the parent loop.
async fn record_step(
    context: &Context,
    params: &ChatParams,
    tools: &[ToolDef],
    messages: Vec<ChatMessage>,
    round: &Round,
    results: Vec<TraceToolResult>,
    step: usize,
    number: u64,
    started: Instant,
) {
    let Some(trace) = &params.trace else { return };
    let record = TraceStep {
        session_id: trace.session_id.clone(),
        space_id: trace.space_id.clone(),
        turn_id: trace.turn_id.clone(),
        round: u32::try_from(step).unwrap_or(u32::MAX),
        model: context.model.clone(),
        backend: trace.backend.clone(),
        system: messages[0].content.clone(),
        tools: tools.to_vec(),
        messages: messages[1..].to_vec(),
        params: json!({ "reasoning_effort": params.reasoning_effort, "temperature": params.temperature, "top_p": params.top_p, "max_tokens": params.max_tokens }),
        response: TraceResponse {
            content: round.content.clone(),
            reasoning: round.reasoning.clone(),
            tool_calls: round.calls.clone(),
            finish: if round.calls.is_empty() {
                "stop"
            } else {
                "tool_calls"
            }
            .into(),
        },
        usage: round.usage,
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        tool_results: results,
    };
    let store = trace.store.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(id) = store.record_step(&record) {
            let _ = store.add_label(&id, "subagent", &number.to_string());
        }
    })
    .await;
}
