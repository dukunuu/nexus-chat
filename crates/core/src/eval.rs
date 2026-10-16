//! Golden-task loading and deterministic answer/tool-call checks for `nexus eval`.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::db::Message;
use crate::tools::classify_tool_result;

/// One golden task, loaded from a TOML file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalTask {
    /// Display name; defaults to the task file's stem when omitted.
    #[serde(default)]
    pub name: String,
    /// Question sent to the model.
    pub prompt: String,
    /// Enable search-first web mode for this task.
    #[serde(default)]
    pub web: bool,
    /// Deterministic expectations for the answer and tool calls.
    #[serde(default)]
    pub check: EvalChecks,
}

/// Optional checks; at least one effective check is required per task.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalChecks {
    pub answer_contains: Vec<String>,
    pub answer_not_contains: Vec<String>,
    pub tool_called: Vec<String>,
    pub tool_not_called: Vec<String>,
    pub tool_args_contains: Vec<String>,
    pub max_tool_calls: Option<usize>,
    pub no_invalid_tool_calls: bool,
}

impl EvalChecks {
    fn is_empty(&self) -> bool {
        self.answer_contains.is_empty()
            && self.answer_not_contains.is_empty()
            && self.tool_called.is_empty()
            && self.tool_not_called.is_empty()
            && self.tool_args_contains.is_empty()
            && self.max_tool_calls.is_none()
            && !self.no_invalid_tool_calls
    }
}

/// Read the directory's `.toml` tasks, reject invalid tasks, and sort by name.
pub fn load_tasks(dir: &Path) -> Result<Vec<EvalTask>> {
    let mut tasks = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut task: EvalTask =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if task.name.is_empty() {
            task.name = path
                .file_stem()
                .context("task file has no stem")?
                .to_string_lossy()
                .into_owned();
        }
        if task.check.is_empty() {
            bail!("{}: task has no checks", path.display());
        }
        tasks.push(task);
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

/// A tool call's raw arguments and returned text.
#[derive(Debug, Deserialize)]
pub struct ObservedCall {
    pub name: String,
    pub arguments: String,
    pub result: String,
}

/// The model's final answer and the calls made during its turn.
#[derive(Debug)]
pub struct Observed {
    pub answer: String,
    pub calls: Vec<ObservedCall>,
}

/// Return every unmet expectation as a human-readable failure; empty means pass.
#[must_use]
pub fn check(task: &EvalTask, observed: &Observed) -> Vec<String> {
    let mut failures = Vec::new();
    let answer = observed.answer.to_lowercase();
    for needle in &task.check.answer_contains {
        if !answer.contains(&needle.to_lowercase()) {
            failures.push(format!("answer_contains: missing {needle:?}"));
        }
    }
    for needle in &task.check.answer_not_contains {
        if answer.contains(&needle.to_lowercase()) {
            failures.push(format!("answer_not_contains: found {needle:?}"));
        }
    }
    for name in &task.check.tool_called {
        if !observed.calls.iter().any(|call| call.name == *name) {
            failures.push(format!("tool_called: {name:?} was not called"));
        }
    }
    for name in &task.check.tool_not_called {
        if observed.calls.iter().any(|call| call.name == *name) {
            failures.push(format!("tool_not_called: {name:?} was called"));
        }
    }
    for needle in &task.check.tool_args_contains {
        if !observed
            .calls
            .iter()
            .any(|call| call.arguments.contains(needle))
        {
            failures.push(format!("tool_args_contains: missing {needle:?}"));
        }
    }
    if let Some(max) = task.check.max_tool_calls
        && observed.calls.len() > max
    {
        failures.push(format!(
            "max_tool_calls: {} calls exceeds {max}",
            observed.calls.len()
        ));
    }
    if task.check.no_invalid_tool_calls {
        for call in &observed.calls {
            if matches!(
                classify_tool_result(&call.result, ""),
                Some("invalid_args" | "unknown_tool")
            ) {
                failures.push(format!(
                    "no_invalid_tool_calls: {}: {}",
                    call.name,
                    call.result.lines().next().unwrap_or_default()
                ));
            }
        }
    }
    failures
}

/// Extract calls from a turn's stored `tool_call` rows, as history replay does.
#[must_use]
pub fn observed_from_messages(answer: &str, turn: &[Message]) -> Observed {
    Observed {
        answer: answer.to_string(),
        calls: turn
            .iter()
            .filter(|message| message.role == "tool_call")
            .filter_map(|message| serde_json::from_str(&message.content).ok())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("nexus-eval-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn load_tasks_parses_and_sorts() {
        let dir = TempDir::new();
        fs::write(
            dir.0.join("a.toml"),
            "name = 'zulu'\nprompt = 'Hello'\nweb = true\n[check]\nmax_tool_calls = 0\n",
        )
        .unwrap();
        fs::write(
            dir.0.join("bravo.toml"),
            "prompt = 'Hi'\n[check]\nanswer_contains = ['hi']\n",
        )
        .unwrap();
        fs::write(dir.0.join("README.md"), "not a task").unwrap();
        let tasks = load_tasks(&dir.0).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].name, "bravo");
        assert_eq!(tasks[0].prompt, "Hi");
        assert!(!tasks[0].web);
        assert_eq!(tasks[0].check.answer_contains, ["hi"]);
        assert_eq!(tasks[1].name, "zulu");
        assert!(tasks[1].web);
        assert_eq!(tasks[1].check.max_tool_calls, Some(0));
    }

    #[test]
    fn load_tasks_rejects_unknown_keys() {
        let dir = TempDir::new();
        for text in [
            "prompt = 'Hi'\nunknown = true\n[check]\nmax_tool_calls = 0\n",
            "prompt = 'Hi'\n[check]\nmax_tool_calls = 0\nunknown = true\n",
        ] {
            fs::write(dir.0.join("task.toml"), text).unwrap();
            let error = format!("{:#}", load_tasks(&dir.0).unwrap_err());
            assert!(error.contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn load_tasks_rejects_a_task_without_checks() {
        let dir = TempDir::new();
        for text in [
            "prompt = 'Hi'\n",
            "prompt = 'Hi'\n[check]\n",
            "prompt = 'Hi'\n[check]\nanswer_contains = []\nno_invalid_tool_calls = false\n",
        ] {
            fs::write(dir.0.join("task.toml"), text).unwrap();
            let error = load_tasks(&dir.0).unwrap_err().to_string();
            assert!(error.contains("no checks"), "{error}");
        }
    }

    fn observed(answer: &str, name: &str, arguments: &str, result: &str) -> Observed {
        Observed {
            answer: answer.to_string(),
            calls: vec![ObservedCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
                result: result.to_string(),
            }],
        }
    }

    #[test]
    fn check_reports_each_failure_kind() {
        let good = || {
            observed(
                "Hello WORLD",
                "scripts",
                r#"{"action":"list"}"#,
                "no scripts",
            )
        };
        let wrong_call = || observed("", "media", r#"{"action":"List"}"#, "done");
        for (field, checks, passing, failing) in [
            (
                "answer_contains",
                "answer_contains = ['hello', 'world']",
                good(),
                observed("hello", "scripts", "", ""),
            ),
            (
                "answer_not_contains",
                "answer_not_contains = ['SECRET', 'world']",
                observed("hello", "scripts", "", ""),
                good(),
            ),
            (
                "tool_called",
                "tool_called = ['scripts']",
                good(),
                wrong_call(),
            ),
            (
                "tool_not_called",
                "tool_not_called = ['media']",
                good(),
                wrong_call(),
            ),
            (
                "tool_args_contains",
                "tool_args_contains = ['\"action\":\"list\"']",
                good(),
                wrong_call(),
            ),
            (
                "max_tool_calls",
                "max_tool_calls = 0",
                Observed {
                    answer: "hello".to_string(),
                    calls: Vec::new(),
                },
                good(),
            ),
            (
                "no_invalid_tool_calls",
                "no_invalid_tool_calls = true",
                good(),
                observed("", "scripts", "", "missing required field: action"),
            ),
        ] {
            let task: EvalTask =
                toml::from_str(&format!("prompt = 'Hi'\n[check]\n{checks}\n")).unwrap();
            assert!(check(&task, &passing).is_empty(), "{field}: should pass");
            let failures = check(&task, &failing);
            assert_eq!(failures.len(), 1, "{field}: {failures:?}");
            assert!(failures[0].contains(field), "{failures:?}");
        }
    }

    #[test]
    fn check_collects_failures_and_classifies_invalid_calls() {
        let task: EvalTask = toml::from_str(
            "prompt = 'Hi'\n[check]\nanswer_contains = ['hello', 'world']\nno_invalid_tool_calls = true\nmax_tool_calls = 1\n",
        )
        .unwrap();
        let mut outcome = observed("", "unknown", "", "unknown tool: unknown");
        outcome.calls.push(ObservedCall {
            name: "scripts".to_string(),
            arguments: String::new(),
            result: "invalid action for scripts: nope".to_string(),
        });
        assert_eq!(check(&task, &outcome).len(), 5);
        let task: EvalTask =
            toml::from_str("prompt = 'Hi'\n[check]\nno_invalid_tool_calls = true\n").unwrap();
        for result in ["unknown tool: nope", "invalid tool arguments: bad JSON"] {
            assert_eq!(check(&task, &observed("", "nope", "", result)).len(), 1);
        }
        for result in [
            "search failed: offline",
            "hash abc not found",
            "unknown file: missing.txt",
            "file.txt:\nmissing required field: this is file content",
        ] {
            assert!(check(&task, &observed("", "files", "", result)).is_empty());
        }
    }

    fn message(role: &str, content: String) -> Message {
        Message {
            role: role.to_string(),
            content,
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
    fn observed_from_messages_extracts_tool_calls() {
        let arguments = "{ \"action\": \"list\" }";
        let stored = serde_json::json!({
            "id": "call-1",
            "reasoning_content": "I should check scripts",
            "assistant_content": "Checking",
            "name": "scripts",
            "arguments": arguments,
            "result": "no scripts",
        })
        .to_string();
        let turn = vec![
            message("user", "List scripts".to_string()),
            message("tool_call", stored.clone()),
            message("assistant", stored),
            message(
                "tool_call",
                serde_json::json!({
                    "name": "search",
                    "arguments": "{\"query\":\"Rust\"}",
                    "result": "results",
                })
                .to_string(),
            ),
        ];
        let outcome = observed_from_messages("None found", &turn);
        assert_eq!(outcome.answer, "None found");
        assert_eq!(outcome.calls.len(), 2);
        assert_eq!(outcome.calls[0].name, "scripts");
        assert_eq!(outcome.calls[0].arguments, arguments);
        assert_eq!(outcome.calls[0].result, "no scripts");
        assert_eq!(outcome.calls[1].name, "search");
        assert_eq!(outcome.calls[1].arguments, r#"{"query":"Rust"}"#);
        assert_eq!(outcome.calls[1].result, "results");
    }

    #[test]
    fn starter_tasks_parse() {
        let tasks = load_tasks(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../evals"
        )))
        .unwrap();
        let names: Vec<_> = tasks.iter().map(|task| task.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "knows-the-date",
                "lists-scripts",
                "no-tools-for-small-talk",
                "searches-for-recent-facts",
            ]
        );
    }
}
