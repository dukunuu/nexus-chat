//! Golden-task loading and deterministic answer/tool-call checks for `nexus eval`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::db::Message;
use crate::tools::classify_tool_result;

/// One golden task, loaded from a TOML file.
#[derive(Debug, Deserialize)]
#[serde(try_from = "TaskInput")]
pub struct EvalTask {
    /// Display name; defaults to the task file's stem when omitted.
    #[serde(default)]
    pub name: String,
    /// Question sent to the model.
    pub prompt: String,
    /// Ordered prompts sharing a session, instead of `prompt`.
    pub turns: Vec<String>,
    /// Fixture directory, resolved inside the eval directory by `load_tasks`.
    pub workspace: Option<PathBuf>,
    /// Shell command prefixes permitted in a disposable workspace.
    pub allow_shell: Vec<String>,
    /// Enable search-first web mode for this task.
    #[serde(default)]
    pub web: bool,
    /// Deterministic expectations for the answer and tool calls.
    #[serde(default)]
    pub check: EvalChecks,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskInput {
    #[serde(default)]
    name: String,
    prompt: Option<String>,
    turns: Option<Vec<String>>,
    workspace: Option<PathBuf>,
    #[serde(default)]
    allow_shell: Vec<String>,
    #[serde(default)]
    web: bool,
    #[serde(default)]
    check: EvalChecks,
}

impl TryFrom<TaskInput> for EvalTask {
    type Error = anyhow::Error;

    fn try_from(input: TaskInput) -> Result<Self> {
        let (prompt, turns) = match (input.prompt, input.turns) {
            (Some(prompt), None) => (prompt, Vec::new()),
            (None, Some(turns)) if !turns.is_empty() => (String::new(), turns),
            _ => bail!("task must give exactly one of prompt or non-empty turns"),
        };
        Ok(Self {
            name: input.name,
            prompt,
            turns,
            workspace: input.workspace,
            allow_shell: input.allow_shell,
            web: input.web,
            check: input.check,
        })
    }
}

impl EvalTask {
    /// Prompts to send in order in the same session.
    #[must_use]
    pub fn prompts(&self) -> &[String] {
        if self.turns.is_empty() {
            std::slice::from_ref(&self.prompt)
        } else {
            &self.turns
        }
    }
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
    pub file_contains: Vec<[String; 2]>,
    pub file_not_contains: Vec<[String; 2]>,
    pub shell_succeeded: Vec<String>,
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
            && self.file_contains.is_empty()
            && self.file_not_contains.is_empty()
            && self.shell_succeeded.is_empty()
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
        if let Some(workspace) = &task.workspace {
            let eval_root = dir.canonicalize()?;
            let fixture = dir
                .join(workspace)
                .canonicalize()
                .with_context(|| format!("{}: opening workspace", path.display()))?;
            if workspace.is_absolute() || !fixture.starts_with(&eval_root) || !fixture.is_dir() {
                bail!(
                    "{}: workspace must be a directory inside the eval directory",
                    path.display()
                );
            }
            task.workspace = Some(fixture);
        }
        if task.workspace.is_none()
            && (!task.check.file_contains.is_empty() || !task.check.file_not_contains.is_empty())
        {
            bail!("{}: file checks require a workspace", path.display());
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

/// The model's final answer and the calls made across all observed turns.
#[derive(Debug)]
pub struct Observed {
    pub answer: String,
    pub calls: Vec<ObservedCall>,
}

/// Return every unmet expectation as a human-readable failure; empty means pass.
#[must_use]
pub fn check(task: &EvalTask, observed: &Observed) -> Vec<String> {
    check_workspace(task, observed, None)
}

/// Check the final answer, all turns' calls, and files in the disposable copy.
#[must_use]
pub fn check_workspace(task: &EvalTask, observed: &Observed, root: Option<&Path>) -> Vec<String> {
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
    check_shell(&task.check, &observed.calls, &mut failures);
    check_files(&task.check, root, &mut failures);
    failures
}

fn check_shell(checks: &EvalChecks, calls: &[ObservedCall], failures: &mut Vec<String>) {
    for prefix in &checks.shell_succeeded {
        if !calls.iter().any(|call| {
            let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments) else {
                return false;
            };
            (call.name == "shell_run" || (call.name == "shell" && args["action"] == "run"))
                && args["cmd"]
                    .as_str()
                    .is_some_and(|cmd| cmd.starts_with(prefix))
                && call
                    .result
                    .strip_prefix("exit 0")
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        }) {
            failures.push(format!("shell_succeeded: {prefix:?} did not return exit 0"));
        }
    }
}

fn check_files(checks: &EvalChecks, root: Option<&Path>, failures: &mut Vec<String>) {
    for (kind, pairs, expected) in [
        ("file_contains", &checks.file_contains, true),
        ("file_not_contains", &checks.file_not_contains, false),
    ] {
        for [path, needle] in pairs {
            let content = (|| -> Result<String> {
                let root = root
                    .context("file check needs a workspace")?
                    .canonicalize()?;
                let target = root.join(path).canonicalize()?;
                if Path::new(path).is_absolute() || !target.starts_with(&root) {
                    bail!("file check path escapes workspace");
                }
                Ok(fs::read_to_string(target)?)
            })();
            match content {
                Ok(content) if content.contains(needle) == expected => {}
                Ok(_) => failures.push(format!("{kind}: {path:?}, {needle:?}")),
                Err(error) => failures.push(format!("{kind}: {path:?}: {error:#}")),
            }
        }
    }
}

/// Disposable fixture copy. Dropping it removes edits and runtime logs.
pub struct EvalWorkspace {
    dir: PathBuf,
}

impl EvalWorkspace {
    /// Copy a fixture's regular files and directories; symlinks are rejected.
    ///
    /// # Errors
    /// Returns filesystem errors or unsupported fixture entry types.
    pub fn copy_from(fixture: &Path) -> Result<Self> {
        let copy = Self {
            dir: std::env::temp_dir().join(format!("nexus-eval-{}", uuid::Uuid::new_v4())),
        };
        copy_fixture(fixture, &copy.root())?;
        Ok(copy)
    }

    /// Workspace root, separate from disposable runtime logs.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.dir.join("workspace")
    }
}

impl Drop for EvalWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn copy_fixture(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = target.join(entry.file_name());
        if kind.is_dir() {
            copy_fixture(&entry.path(), &dest)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), dest)?;
        } else {
            bail!(
                "fixture entries must be regular files or directories: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
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

    #[test]
    fn multi_turn_loading_and_new_checks() {
        let dir = TempDir::new();
        fs::create_dir(dir.0.join("fixture")).unwrap();
        let path = dir.0.join("task.toml");
        fs::write(&path, "turns = ['read', 'edit']\nworkspace = 'fixture'\nallow_shell = ['ls']\n[check]\nfile_contains = [['a', 'yes']]\nfile_not_contains = [['a', 'no']]\nshell_succeeded = ['ls']\n").unwrap();
        let tasks = load_tasks(&dir.0).unwrap();
        assert_eq!(tasks[0].prompts(), ["read", "edit"]);
        assert_eq!(
            tasks[0].workspace.as_ref().unwrap(),
            &dir.0.join("fixture").canonicalize().unwrap()
        );
        assert_eq!(tasks[0].allow_shell, ["ls"]);
        assert_eq!(tasks[0].check.file_contains, [["a", "yes"]]);
        assert_eq!(tasks[0].check.file_not_contains, [["a", "no"]]);
        assert_eq!(tasks[0].check.shell_succeeded, ["ls"]);
        for input in [
            "prompt = 'hi'\nturns = ['hi']",
            "turns = []",
            "",
            "prompt = 'hi'\nworkspace = '..'",
            "prompt = 'hi'\nworkspace = '/tmp'",
            "prompt = 'hi'\nworkspace = 'task.toml'",
        ] {
            fs::write(&path, format!("{input}\n[check]\nmax_tool_calls = 0\n")).unwrap();
            assert!(load_tasks(&dir.0).is_err(), "{input}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_symlink_must_stay_in_eval_dir() {
        let dir = TempDir::new();
        let outside = TempDir::new();
        std::os::unix::fs::symlink(&outside.0, dir.0.join("fixture")).unwrap();
        fs::write(
            dir.0.join("task.toml"),
            "prompt = 'hi'\nworkspace = 'fixture'\n[check]\nmax_tool_calls = 0",
        )
        .unwrap();
        assert!(load_tasks(&dir.0).is_err());
    }

    #[test]
    fn final_answer_and_all_turns_tools_are_checked() {
        let task: EvalTask = toml::from_str("turns = ['first', 'second']\n[check]\nanswer_contains = ['final']\nanswer_not_contains = ['earlier']\ntool_called = ['fs', 'shell']\ntool_not_called = ['search']\ntool_args_contains = ['notes']\nmax_tool_calls = 2\nno_invalid_tool_calls = true\nshell_succeeded = ['sh test.sh']").unwrap();
        let first = vec![message(
            "tool_call",
            serde_json::json!({"name":"fs", "arguments":"notes", "result":"old detail"})
                .to_string(),
        )];
        let last = vec![message("tool_call", serde_json::json!({"name":"shell", "arguments":r#"{"action":"run","cmd":"sh test.sh"}"#, "result":"exit 0\nok"}).to_string())];
        let mut observed = observed_from_messages("earlier", &first);
        let final_turn = observed_from_messages("final", &last);
        observed.answer = final_turn.answer;
        observed.calls.extend(final_turn.calls);
        assert!(check(&task, &observed).is_empty());
        observed.calls[0].result = "unknown tool: fs".into();
        assert!(
            check(&task, &observed)
                .iter()
                .any(|failure| failure.contains("no_invalid_tool_calls"))
        );
        observed.calls[0].name = "search".into();
        observed.calls.push(ObservedCall {
            name: "fs".into(),
            arguments: String::new(),
            result: String::new(),
        });
        let failures = check(&task, &observed);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("tool_not_called"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("max_tool_calls"))
        );
        for result in ["exit 01", "exit 1\nexit 0", "blocked", "text exit 0"] {
            observed.calls[1].result = result.into();
            assert!(
                check(&task, &observed)
                    .iter()
                    .any(|failure| failure.contains("shell_succeeded"))
            );
        }
    }

    #[test]
    fn file_checks_and_disposable_copy() {
        let dir = TempDir::new();
        fs::write(dir.0.join("notes"), "original").unwrap();
        fs::create_dir(dir.0.join("nested")).unwrap();
        fs::write(dir.0.join("nested/one"), "one").unwrap();
        let copy = EvalWorkspace::copy_from(&dir.0).unwrap();
        let root = copy.root();
        assert_eq!(fs::read_to_string(root.join("nested/one")).unwrap(), "one");
        fs::write(root.join("notes"), "changed detail").unwrap();
        let task: EvalTask = toml::from_str("prompt = 'hi'\n[check]\nfile_contains = [['notes', 'detail']]\nfile_not_contains = [['notes', 'original']]").unwrap();
        let observed = Observed {
            answer: String::new(),
            calls: Vec::new(),
        };
        assert!(check_workspace(&task, &observed, Some(&root)).is_empty());
        fs::write(root.join("notes"), "original").unwrap();
        assert_eq!(check_workspace(&task, &observed, Some(&root)).len(), 2);
        fs::remove_file(root.join("notes")).unwrap();
        assert_eq!(check_workspace(&task, &observed, Some(&root)).len(), 2);
        assert_eq!(fs::read_to_string(dir.0.join("notes")).unwrap(), "original");
        let escape: EvalTask =
            toml::from_str("prompt = 'hi'\n[check]\nfile_contains = [['../../notes', 'original']]")
                .unwrap();
        assert_eq!(check_workspace(&escape, &observed, Some(&root)).len(), 1);
        drop(copy);
        assert!(!root.parent().unwrap().exists());
    }

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
                "agent-release-followup",
                "chat-recall-six-turns",
                "knows-the-date",
                "lists-scripts",
                "no-tools-for-small-talk",
                "searches-for-recent-facts",
            ]
        );
    }
}
