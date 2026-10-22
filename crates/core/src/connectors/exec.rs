//! Running a connector tool on behalf of the model: the approval flow
//! (which applies in chat mode too), rug-pull protection, the call itself,
//! and the `connectors` meta-tool used when there are too many tools to
//! list.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::protocol::ToolInfo;
use super::registry::{Access, Registry, Target};
use super::results;
use super::transport::McpError;
use crate::tools::{ApprovalRequest, Approver, Decision};

/// The approval class connector requests carry.
pub const CLASS: &str = "connector";
/// The meta-tool's name.
pub const META_TOOL: &str = "connectors";

const SUMMARY_ARG_CHARS: usize = 1_200;
const DETAIL_ARG_CHARS: usize = 12_000;

/// Everything a connector call needs from the toolbox.
pub struct Exec<'a> {
    pub registry: &'a Registry,
    pub approver: Option<&'a Arc<dyn Approver>>,
    pub files_dir: &'a Path,
}

/// Whether `name` is a connector tool or the meta-tool.
#[must_use]
pub fn handles(registry: &Registry, name: &str) -> bool {
    !registry.is_empty() && (name == META_TOOL || registry.resolve(name).is_some())
}

fn blocked(text: impl Into<String>) -> (String, String) {
    (text.into(), "blocked".to_string())
}

fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n… ({} more characters)", count - max)
}

fn parse_arguments(raw: &Value) -> Result<Value, String> {
    match raw {
        Value::Null => Ok(Value::Object(serde_json::Map::new())),
        Value::Object(_) => Ok(raw.clone()),
        Value::String(text) if text.trim().is_empty() => Ok(Value::Object(serde_json::Map::new())),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value @ Value::Object(_)) => Ok(value),
            _ => Err("invalid tool arguments: expected a JSON object".to_string()),
        },
        _ => Err("invalid tool arguments: expected a JSON object".to_string()),
    }
}

/// The approval panel's content for one call.
fn approval_request(
    target: &Target,
    info: &ToolInfo,
    arguments: &Value,
    changed: bool,
) -> ApprovalRequest {
    let pretty = serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
    let mut head = String::new();
    if changed {
        head.push_str("this tool's definition changed since you allowed it\n");
    }
    head.push_str(&info.annotations.words());
    if let Some(title) = info.title.as_deref().filter(|title| *title != info.name) {
        let _ = write!(head, "\n{title}");
    }
    ApprovalRequest {
        tool: target.connector.clone(),
        action: target.tool.clone(),
        summary: format!("{head}\n\n{}", clip(&pretty, SUMMARY_ARG_CHARS)),
        class: Some(CLASS.to_string()),
        detail: (pretty.chars().count() > SUMMARY_ARG_CHARS)
            .then(|| format!("{head}\n\n{}", clip(&pretty, DETAIL_ARG_CHARS))),
        allow_session: true,
    }
}

struct Outcome {
    text: String,
    status: String,
    /// The call reached the server (it was allowed and sent).
    ran: bool,
}

async fn call_target(exec: &Exec<'_>, target: &Target, arguments: Value) -> Outcome {
    let registry = exec.registry;
    let refused = |(text, status): (String, String)| Outcome {
        text,
        status,
        ran: false,
    };
    let Some(info) = registry.tool_info(target) else {
        return refused(blocked(format!(
            "unknown tool: {} · its connector may have disconnected",
            target.exposed
        )));
    };
    match registry.access(target) {
        Access::Denied => {
            return refused(blocked(format!(
                "denied by policy: you set {} to deny · ask the user",
                target.exposed
            )));
        }
        Access::Free => {}
        Access::Ask { changed } => {
            let Some(approver) = exec.approver else {
                return refused(blocked(
                    "needs approval · not available here (headless); describe what you would do and let the user run it",
                ));
            };
            let decision = approver
                .request(approval_request(target, &info, &arguments, changed))
                .await;
            if registry.access(target) == Access::Denied
                || registry
                    .tool_info(target)
                    .is_none_or(|current| current != info)
            {
                return refused(blocked(
                    "this tool's definition or policy changed while awaiting approval · review it again before calling",
                ));
            }
            match decision {
                Decision::Allow => {}
                Decision::AllowForSession => registry.grant_session(target),
                Decision::AllowAlways => {
                    if registry.allow_always(target).is_err() {
                        // The grant still holds for this session.
                        registry.grant_session(target);
                    }
                }
                Decision::Deny | Decision::ReviewEach => {
                    return refused(blocked(
                        "denied by the user · ask what they want instead of retrying",
                    ));
                }
            }
        }
    }
    let label = info.title.as_deref().unwrap_or(&target.tool);
    let status = format!("Calling {} · {label}…", target.connector);
    let text = match registry.call(target, arguments).await {
        Ok(result) => {
            results::render(&target.connector, &target.tool, &result, exec.files_dir).text
        }
        Err(McpError::NeedsAuth { .. }) => format!(
            "connector {} failed: sign-in required · tell the user to open /connectors",
            target.connector
        ),
        Err(error) => format!(
            "connector {} failed: {error} · the tool may not have run; tell the user rather than retrying blindly",
            target.connector
        ),
    };
    Outcome {
        text,
        status,
        ran: true,
    }
}

/// Run a connector tool or the meta-tool. Returns `(result, status)`.
pub async fn run(exec: Exec<'_>, name: &str, args: &str) -> (String, String) {
    let raw: Value = if args.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(args) {
            Ok(value) => value,
            Err(error) => {
                return (
                    format!("invalid tool arguments: {error}"),
                    "invalid arguments".to_string(),
                );
            }
        }
    };
    if name == META_TOOL {
        return run_meta(&exec, &raw).await;
    }
    let Some(target) = exec.registry.resolve(name) else {
        return blocked(format!("unknown tool: {name}"));
    };
    let arguments = match parse_arguments(&raw) {
        Ok(arguments) => arguments,
        Err(error) => return (error, "invalid arguments".to_string()),
    };
    let outcome = call_target(&exec, &target, arguments).await;
    (outcome.text, outcome.status)
}

async fn run_meta(exec: &Exec<'_>, raw: &Value) -> (String, String) {
    let field = |key: &str| raw.get(key).and_then(Value::as_str).unwrap_or_default();
    match field("action") {
        "search" => (
            exec.registry.exposure().search(field("query")),
            "Searching connectors…".to_string(),
        ),
        "call" => {
            let tool = field("tool");
            if tool.is_empty() {
                return (
                    "missing required field: tool".to_string(),
                    "invalid arguments".to_string(),
                );
            }
            let Some(target) = exec.registry.resolve(tool) else {
                return blocked(format!(
                    "unknown connector tool: {tool} · use connectors {{action: \"search\"}} to find the exact name"
                ));
            };
            let arguments = match parse_arguments(raw.get("arguments").unwrap_or(&Value::Null)) {
                Ok(arguments) => arguments,
                Err(error) => return (error, "invalid arguments".to_string()),
            };
            let outcome = call_target(exec, &target, arguments).await;
            if outcome.ran {
                exec.registry.promote(&target);
            }
            (outcome.text, outcome.status)
        }
        other => (
            format!("invalid action for connectors: {other}"),
            "invalid arguments".to_string(),
        ),
    }
}
