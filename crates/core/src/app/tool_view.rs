//! Frontend-independent, best-effort presentation of workspace tool calls.

use std::fmt::Write as _;

use serde_json::Value;

/// How a tool call ended, inferred only from the tool's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Ok,
    Failed,
    Denied,
    Blocked,
}

/// The readable parts of a tool card, without any terminal styling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolView {
    /// Empty when the call uses the legacy generic summary.
    pub verb: String,
    pub target: String,
    pub stats: Vec<String>,
    pub added: Option<usize>,
    pub deleted: Option<usize>,
    pub outcome: ToolOutcome,
    pub diff: Option<String>,
    pub output: Option<String>,
    /// Web search hits in result order, for a sources list.
    pub sources: Vec<Source>,
    /// Rendered slides the result refers to, as `(alt, path under the space files)`.
    pub images: Vec<(String, String)>,
    /// A call to an MCP connector (or its search): drawn with the connector glyph.
    pub connector: bool,
    /// Sub-calls in argument order; missing capped results are retained as such.
    pub batch: Vec<BatchEntry>,
}

/// One operation inside a batch, using the same presentation as a standalone call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchEntry {
    pub tool: String,
    /// The tool's action or search mode, if supplied.
    pub action: String,
    pub target: String,
    /// None when the combined result cap omitted this operation's result.
    pub status: Option<ToolOutcome>,
    /// Short statistics on success, or the first error line otherwise.
    pub outcome: String,
    pub arguments: Value,
    pub result: Option<String>,
    pub truncated: bool,
    pub view: ToolView,
}

/// One web search hit: what a reader scans for in a results list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Provider-supplied thumbnail; never discovered by fetching the page.
    pub image: Option<String>,
    pub title: String,
    pub url: String,
    /// The host, without `www.` (`en.wikipedia.org`).
    pub domain: String,
    /// The first few lines of the hit's text, joined.
    pub snippet: String,
}

impl ToolView {
    /// The same one-line header used by every frontend.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.verb.is_empty() {
            return self.target.clone();
        }
        let mut text = format!("{} {}", self.verb, self.target).trim().to_string();
        if let Some(added) = self.added {
            let _ = write!(text, "  +{added}");
        }
        if let Some(deleted) = self.deleted {
            let _ = write!(text, " −{deleted}");
        }
        for stat in &self.stats {
            let _ = write!(text, " · {stat}");
        }
        text.chars()
            .map(|c| match c {
                '\t' => "    ".to_string(),
                c if c.is_control() => c.escape_unicode().to_string(),
                c => c.to_string(),
            })
            .collect()
    }
}

/// Parse emitted tool results, retaining the generic summary on malformed input.
#[must_use]
pub fn tool_call_view(name: &str, args: &str, result: &str) -> ToolView {
    parse(name, args, result).unwrap_or_else(|| ToolView {
        verb: String::new(),
        target: super::tool_call_summary(name, args, result),
        stats: Vec::new(),
        added: None,
        deleted: None,
        outcome: outcome(result),
        diff: None,
        output: None,
        sources: Vec::new(),
        images: Vec::new(),
        connector: false,
        batch: Vec::new(),
    })
}

fn outcome(result: &str) -> ToolOutcome {
    let first = result.lines().next().unwrap_or_default();
    if first.starts_with("denied by the user") || first.starts_with("denied by policy") {
        return ToolOutcome::Denied;
    }
    if first.contains("blocked")
        || first.starts_with("path escapes")
        || first.starts_with("path denied:")
        || first.starts_with("needs approval")
        || first.contains("not available")
        || first.contains("needs an agent")
        || first.contains("needs the terminal UI")
        || first.contains("only available")
        || first.contains("confinement unavailable")
        || result
            .lines()
            .any(|line| line.starts_with("privilege escalation is blocked"))
    {
        return ToolOutcome::Blocked;
    }
    let exit = first
        .strip_prefix("exit ")
        .and_then(|text| text.split_whitespace().next())
        .and_then(|code| code.parse::<i32>().ok());
    let proc_exit = first
        .split(" · ")
        .find_map(|part| part.strip_prefix("exited "))
        .and_then(|code| code.parse::<i32>().ok());
    if exit.or(proc_exit).is_some_and(|code| code != 0)
        || first.starts_with("error:")
        || first.contains(" error:")
        || first.contains(" failed:")
        || first.contains("changed since your last read")
        || first.starts_with("timed out")
        || first.starts_with("killed")
        || first.contains("exited by signal")
        || first.contains("killed: output exceeded")
        || first.starts_with("output exceeded")
        || first.starts_with("fs.")
        || crate::tools::classify_tool_result(result, "").is_some()
    {
        ToolOutcome::Failed
    } else {
        ToolOutcome::Ok
    }
}

// One arm per emitted workspace-tool format, with a common fallback.
#[allow(clippy::too_many_lines)]
pub(super) fn parse(name: &str, args: &str, result: &str) -> Option<ToolView> {
    let args: Value = serde_json::from_str(args).ok()?;
    args.as_object()?;
    let field = |key: &str| args.get(key).and_then(Value::as_str);
    let mut view = ToolView {
        verb: String::new(),
        target: String::new(),
        stats: Vec::new(),
        added: None,
        deleted: None,
        outcome: outcome(result),
        diff: None,
        output: None,
        sources: Vec::new(),
        images: Vec::new(),
        connector: false,
        batch: Vec::new(),
    };
    if let Some(connector_view) = connector_view(name, &args, result, view.clone()) {
        return Some(connector_view);
    }
    let first = result.lines().next().unwrap_or_default();
    let (tool, action) = name
        .split_once('_')
        .filter(|(tool, _)| matches!(*tool, "fs" | "shell" | "proc"))
        .unwrap_or((name, field("action").unwrap_or("")));
    match tool {
        "batch" => return batch_view(&args, result, view),
        "fs" => {
            if !matches!(
                action,
                "read" | "grep" | "glob" | "list" | "edit" | "write" | "delete" | "move"
            ) {
                return None;
            }
            view.verb = action.into();
            let path = field("path")
                .or_else(|| matches!(action, "grep" | "glob" | "list").then_some("."))?;
            view.target = path.into();
            match action {
                "read" => {
                    if let Some((_, range)) = first.rsplit_once(" (lines ") {
                        let range = range.strip_suffix("):")?;
                        view.stats
                            .push(format!("lines {}", range.replace('-', "–")));
                    } else if first.starts_with("binary file (")
                        || first.starts_with("file too large (")
                    {
                        view.stats.push(first.split(" · ").next()?.into());
                    } else if view.outcome == ToolOutcome::Ok {
                        return None;
                    }
                }
                "grep" => {
                    let pattern = field("pattern")?;
                    if view.outcome == ToolOutcome::Ok
                        && !result.contains(":\n")
                        && !result.starts_with("no matches")
                    {
                        return None;
                    }
                    view.target = format!("\"{}\" in {path}", pattern.replace('"', "\\\""));
                    let files = result
                        .lines()
                        .filter(|line| !line.starts_with(' ') && line.ends_with(':'))
                        .count();
                    let shown = result
                        .lines()
                        .filter(|line| {
                            line.starts_with("  ")
                                && line
                                    .split_once(": ")
                                    .is_some_and(|(_, text)| text.contains(pattern))
                        })
                        .count();
                    let omitted = result
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("… ")?
                                .strip_suffix(" more")?
                                .parse::<usize>()
                                .ok()
                        })
                        .fold(0_usize, usize::saturating_add);
                    let matches = shown.saturating_add(omitted);
                    view.stats.push(if omitted > 0 {
                        format!("{matches} matches · {files} files shown")
                    } else {
                        format!("{matches} matches in {files} files")
                    });
                }
                "list" | "glob" => {
                    if action == "glob" {
                        view.target = format!("{} in {path}", field("pattern")?);
                    }
                    let count = result
                        .lines()
                        .enumerate()
                        .fold(0_usize, |count, (index, line)| {
                            if let Some(more) = line
                                .strip_prefix("… ")
                                .and_then(|line| line.strip_suffix(" more"))
                            {
                                return count.saturating_add(more.parse::<usize>().unwrap_or(0));
                            }
                            let summarized = line.rsplit_once("/ (").and_then(|(_, count)| {
                                count.strip_suffix(" entries)")?.parse::<usize>().ok()
                            });
                            let entry = usize::from(
                                action == "glob"
                                    || index > 0
                                    || (!line.ends_with('/') && summarized.is_none()),
                            );
                            count
                                .saturating_add(entry)
                                .saturating_add(summarized.unwrap_or(0))
                        });
                    view.stats.push(format!("{count} entries"));
                }
                "move" => view.target = format!("{path} → {}", field("to")?),
                _ => {
                    if let Some((_, counts)) = first.rsplit_once(" (+") {
                        let (added, deleted) = counts.strip_suffix(')')?.split_once(" −")?;
                        view.added = Some(added.parse().ok()?);
                        view.deleted = Some(deleted.parse().ok()?);
                    } else if view.outcome == ToolOutcome::Ok {
                        return None;
                    }
                    if let Some(start) = result.find("\n--- ") {
                        view.diff = Some(result[start + 1..].to_string());
                    }
                }
            }
        }
        "shell" | "proc" => {
            match (tool, action) {
                ("shell", "run") => {
                    view.verb = "$".into();
                    view.target = field("cmd")?.into();
                    if view.outcome == ToolOutcome::Ok
                        && first
                            .strip_prefix("exit ")?
                            .split_whitespace()
                            .next()?
                            .parse::<i32>()
                            .is_err()
                    {
                        return None;
                    }
                }
                ("shell", "log") => {
                    view.verb = "log".into();
                    view.target = field("id")?.into();
                }
                ("proc", "start") => {
                    view.verb = "start".into();
                    view.target = field("cmd")?.into();
                }
                ("proc", "read" | "kill") => {
                    view.verb = action.into();
                    view.target = format!("process {}", field("id")?);
                }
                ("proc", "list") => {
                    view.verb = "list".into();
                    view.target = "processes".into();
                }
                _ => return None,
            }
            for part in first.split(" · ") {
                if part.starts_with("exit ")
                    || part.starts_with("exited ")
                    || part.starts_with("running")
                    || part.starts_with("ready")
                    || part
                        .strip_suffix('s')
                        .is_some_and(|n| n.parse::<f64>().is_ok())
                {
                    view.stats.push(part.into());
                }
            }
            view.output = if matches!((tool, action), ("shell", "log") | ("proc", "list")) {
                Some(result.to_string())
            } else {
                result
                    .split_once('\n')
                    .map(|(_, output)| output.to_string())
            };
        }
        "task" => {
            view.verb = "task".into();
            view.target = format!(
                "{} · {}",
                field("tools").unwrap_or("read-only"),
                field("prompt")?.lines().next().unwrap_or_default()
            );
            if let Some(report) = first
                .strip_prefix("sub-agent report (")
                .and_then(|text| text.strip_suffix("):"))
            {
                let (steps, tokens) = report.split_once(", ")?;
                let count = tokens.strip_suffix(" tokens")?.parse::<u32>().ok()?;
                view.stats.push(steps.into());
                view.stats.push(if count >= 1000 {
                    format!("{:.1}k tokens", f64::from(count) / 1000.0)
                } else {
                    tokens.into()
                });
            } else if view.outcome == ToolOutcome::Ok {
                return None;
            }
        }
        "search" | "web_search" => {
            let query = field("query")?;
            view.verb = match field("mode") {
                Some("news") => "news",
                Some("academic") => "papers",
                Some("discussion" | "discussions") => "discussions",
                Some("images") => "images",
                _ => "search",
            }
            .into();
            view.target = format!("\"{}\"", query.replace('"', "\\\""));
            view.sources = search_sources(result);
            if view.sources.is_empty() {
                if view.outcome == ToolOutcome::Ok {
                    view.stats.push("no results".into());
                }
            } else {
                let count = view.sources.len();
                view.stats.push(format!(
                    "{count} result{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        "fetch_url" => {
            let url = field("url")?;
            view.verb = "fetch".into();
            view.target = short_url(url);
            if view.outcome == ToolOutcome::Ok {
                let chars = result.chars().count();
                view.stats.push(if chars >= 1000 {
                    format!(
                        "{:.1}k chars",
                        f64::from(u32::try_from(chars).unwrap_or(u32::MAX)) / 1000.0
                    )
                } else {
                    format!("{chars} chars")
                });
            }
        }
        "deck" => deck_view(&mut view, field("action")?, field("path"), result)?,
        "request_workspace" => {
            view.verb = "workspace".into();
            view.target = field("root")?.into();
        }
        "propose_command" => {
            view.verb = "propose $".into();
            view.target = field("cmd")?.into();
        }
        _ => return None,
    }
    match view.outcome {
        ToolOutcome::Denied => view.stats.push("denied".into()),
        ToolOutcome::Blocked => view.stats.push("blocked".into()),
        ToolOutcome::Failed if !view.stats.iter().any(|stat| stat.starts_with("exit")) => {
            view.stats.push("failed".into());
        }
        _ => {}
    }
    Some(view)
}

/// Parse both the original `[i/N] label` output and byte-framed results.
/// Byte lengths keep header-looking fetched content inside its own result.
#[allow(clippy::too_many_lines)] // argument/result alignment, then aggregate counts
fn batch_view(args: &Value, result: &str, mut view: ToolView) -> Option<ToolView> {
    let calls: Vec<_> = args
        .get("calls")?
        .as_array()?
        .iter()
        .filter_map(|call| {
            let tool = call.get("tool")?.as_str().filter(|tool| !tool.is_empty())?;
            Some((
                tool,
                call.get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            ))
        })
        .collect();
    if calls.is_empty() || calls.iter().any(|(tool, _)| *tool == "batch") {
        return None;
    }
    let headers: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (tool, arguments))| {
            let label: String = crate::tools::batch_call_label(tool, arguments)
                .chars()
                .take(120)
                .collect();
            format!("[{}/{}] {label}\n", index + 1, calls.len())
        })
        .collect();
    let mut remaining = result;
    for (index, (tool, arguments)) in calls.into_iter().enumerate() {
        let (text, truncated) = take_batch_result(
            &mut remaining,
            &headers[index],
            headers.get(index + 1).map(String::as_str),
        );
        let call_view = tool_call_view(tool, &arguments.to_string(), text.unwrap_or_default());
        let status = text.map(|_| match call_view.outcome {
            ToolOutcome::Denied => ToolOutcome::Blocked,
            other => other,
        });
        let outcome = match status {
            None => "result not shown".to_string(),
            Some(ToolOutcome::Ok) if !call_view.stats.is_empty() => call_view.stats.join(" · "),
            _ => text
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect(),
        };
        let target = if call_view.verb.is_empty() {
            batch_target(&arguments)
        } else {
            call_view.target.clone()
        };
        view.batch.push(BatchEntry {
            tool: tool.into(),
            action: arguments
                .get("action")
                .or_else(|| arguments.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            target,
            status,
            outcome,
            arguments,
            result: text.map(str::to_string),
            truncated,
            view: call_view,
        });
    }
    view.verb = "batch".into();
    let count = |status| {
        view.batch
            .iter()
            .filter(|entry| entry.status == status)
            .count()
    };
    view.stats = vec![
        format!("{} calls", view.batch.len()),
        format!("{} ok", count(Some(ToolOutcome::Ok))),
    ];
    for (status, label) in [
        (Some(ToolOutcome::Failed), "failed"),
        (Some(ToolOutcome::Blocked), "blocked"),
        (None, "not shown"),
    ] {
        let n = count(status);
        if n > 0 {
            view.stats.push(format!("{n} {label}"));
        }
    }
    let truncated = view.batch.iter().filter(|entry| entry.truncated).count();
    if truncated > 0 {
        view.stats.push(format!("{truncated} truncated"));
    }
    // A batch's first header hides errors in later operations from `outcome`.
    view.outcome = if view
        .batch
        .iter()
        .any(|entry| entry.status == Some(ToolOutcome::Failed))
    {
        ToolOutcome::Failed
    } else if view
        .batch
        .iter()
        .any(|entry| entry.status == Some(ToolOutcome::Blocked))
    {
        ToolOutcome::Blocked
    } else {
        ToolOutcome::Ok
    };
    if !result.is_empty() && view.batch.iter().all(|entry| entry.result.is_none()) {
        view.output = Some(result.into());
        view.outcome = outcome(result);
        if view.outcome == ToolOutcome::Ok
            && !result.starts_with(crate::tools::TOOL_RESULT_OMITTED_PREFIX)
        {
            view.outcome = ToolOutcome::Failed;
        }
    }
    Some(view)
}

/// Length-framed results cannot be confused with headers in page/file text.
/// Old sessions use the exact next argument-derived header as a best effort.
fn take_batch_result<'a>(
    remaining: &mut &'a str,
    header: &str,
    next: Option<&str>,
) -> (Option<&'a str>, bool) {
    let frame = remaining
        .strip_prefix("[batch-result: ")
        .and_then(|body| body.split_once(" bytes]\n"))
        .and_then(|(bytes, body)| Some((bytes.parse::<usize>().ok()?, body)));
    let (bytes, body) = frame.map_or((None, *remaining), |(bytes, body)| (Some(bytes), body));
    let Some(body) = body.strip_prefix(header) else {
        return (None, false);
    };
    let (text, truncated) = if let Some(bytes) = bytes {
        if let Some(rest) = body
            .get(bytes..)
            .and_then(|rest| rest.strip_prefix("\n[/batch-result]"))
        {
            *remaining = rest.strip_prefix('\n').unwrap_or(rest);
            (&body[..bytes], false)
        } else {
            *remaining = "";
            (
                body.strip_suffix("\n... (tool result truncated)")
                    .unwrap_or(body),
                true,
            )
        }
    } else if let Some(end) = next.and_then(|next| body.find(&format!("\n{next}"))) {
        *remaining = &body[end + 1..];
        (&body[..end], false)
    } else {
        *remaining = "";
        (body, next.is_some())
    };
    (
        Some(text),
        truncated || text.ends_with("... (tool result truncated)"),
    )
}

fn batch_target(args: &Value) -> String {
    for key in [
        "query", "url", "path", "name", "video_id", "image_id", "source", "app", "prompt",
    ] {
        if let Some(target) = args
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return if key == "query" {
                format!("{target:?}")
            } else {
                target.chars().take(60).collect()
            };
        }
    }
    String::new()
}

/// The `deck` tool: `created deck`, `edited slide 3`, `viewed 4 slides`,
/// `commented on slide 2`, with the rendered slides the result shows.
fn deck_view(view: &mut ToolView, action: &str, path: Option<&str>, result: &str) -> Option<()> {
    let first = result.lines().next().unwrap_or_default();
    let lead = first.split(" · ").next().unwrap_or_default();
    view.target = path.unwrap_or_default().into();
    if view.outcome != ToolOutcome::Ok {
        view.verb = match action {
            "create" => "create deck",
            "read" => "read deck",
            "view" | "edit" | "comment" | "resolve" => action,
            _ => return None,
        }
        .into();
        view.output = Some(result.into());
        return Some(());
    }
    view.verb = match action {
        "read" => "read deck".into(),
        "resolve" => "resolved thread".into(),
        "comment" if lead.starts_with("replied in thread") => "replied in thread".into(),
        "create" | "view" | "edit" | "comment" => lead.into(),
        _ => return None,
    };
    if matches!(action, "create" | "read")
        && let Some(count) = first.split(" · ").find(|part| {
            part.split_once(' ').is_some_and(|(n, unit)| {
                n.parse::<usize>().is_ok() && matches!(unit, "slide" | "slides")
            })
        })
    {
        view.stats.push(count.into());
    }
    let mut notes = Vec::new();
    for line in result.lines().skip(1) {
        match line
            .strip_prefix("![")
            .and_then(|inner| inner.split_once("]("))
            .and_then(|(alt, file)| Some((alt, file.strip_suffix(')')?)))
        {
            Some((alt, file)) => view.images.push((alt.into(), file.into())),
            None => notes.push(line),
        }
    }
    view.output = if action == "read" {
        Some(result.into())
    } else {
        (!notes.is_empty()).then(|| notes.join("\n"))
    };
    Some(())
}

/// A connector tool call (`<connector>__<tool>`) or the `connectors`
/// meta-tool: the tool's title (or name) is the verb, the connector the
/// target, with the arguments and the result's first line as detail.
fn connector_view(name: &str, args: &Value, result: &str, mut view: ToolView) -> Option<ToolView> {
    let (exposed, inner) = if name == "connectors" {
        match args.get("action")?.as_str()? {
            "search" => {
                view.connector = true;
                view.verb = "search".into();
                view.target = "connectors".into();
                if let Some(query) = args.get("query").and_then(Value::as_str) {
                    view.stats.push(format!("{query:?}"));
                }
                finish_connector_view(&mut view, result);
                return Some(view);
            }
            "call" => (
                args.get("tool")?.as_str()?.to_string(),
                args.get("arguments").cloned().unwrap_or(Value::Null),
            ),
            _ => return None,
        }
    } else {
        (name.to_string(), args.clone())
    };
    let (connector, tool) = exposed.split_once("__")?;
    if connector.is_empty() || tool.is_empty() {
        return None;
    }
    view.connector = true;
    view.verb = crate::connectors::display_title(&exposed).unwrap_or_else(|| tool.to_string());
    view.target = crate::connectors::registry::display_connector(&exposed)
        .unwrap_or_else(|| connector.to_string());
    let preview = argument_preview(&inner);
    if !preview.is_empty() {
        view.stats.push(preview);
    }
    finish_connector_view(&mut view, result);
    Some(view)
}

fn finish_connector_view(view: &mut ToolView, result: &str) {
    if view.outcome == ToolOutcome::Ok
        && let Some(line) = result.lines().map(str::trim).find(|line| !line.is_empty())
    {
        let line: String = line.chars().take(80).collect();
        view.stats.push(line);
    }
    match view.outcome {
        ToolOutcome::Denied => view.stats.push("denied".into()),
        ToolOutcome::Blocked => view.stats.push("blocked".into()),
        ToolOutcome::Failed => view.stats.push("failed".into()),
        ToolOutcome::Ok => {}
    }
}

/// `key=value` pairs for the first few scalar arguments, short enough for a
/// card header.
fn argument_preview(args: &Value) -> String {
    let Some(object) = args.as_object() else {
        return String::new();
    };
    let mut parts = Vec::new();
    for (key, value) in object.iter().take(3) {
        let shown = match value {
            Value::String(text) => {
                let text: String = text.chars().take(24).collect();
                format!("{text:?}")
            }
            Value::Number(_) | Value::Bool(_) => value.to_string(),
            Value::Null => continue,
            _ => "…".to_string(),
        };
        parts.push(format!("{key}={shown}"));
    }
    parts.join(" ").chars().take(60).collect()
}

/// Parse the search tools' `[N] Title` / indented URL / text blocks.
fn search_sources(result: &str) -> Vec<Source> {
    let mut sources: Vec<Source> = Vec::new();
    let mut lines = result.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(title) = line
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
            .filter(|(number, _)| number.parse::<usize>().is_ok())
            .map(|(_, title)| title.trim())
        else {
            continue;
        };
        let url = lines
            .next_if(|line| line.trim_start().starts_with("http"))
            .map(str::trim)
            .unwrap_or_default();
        let mut snippet = String::new();
        let mut image = None;
        while let Some(text) = lines.next_if(|line| !line.starts_with('[')) {
            let text = text.trim();
            if let Some(url) = text.strip_prefix("image: ") {
                image = Some(url.to_string());
                continue;
            }
            if text.starts_with("image · ") {
                image = Some(url.to_string());
            }
            if text.is_empty()
                || text == title
                || text == "discussions:"
                || (text.starts_with("(dropped ") && text.ends_with(" off-topic results)"))
            {
                continue;
            }
            if snippet.chars().count() > 240 {
                continue;
            }
            if !snippet.is_empty() {
                snippet.push(' ');
            }
            snippet.push_str(text);
        }
        sources.push(Source {
            image,
            title: title.to_string(),
            url: url.to_string(),
            domain: domain(url),
            snippet,
        });
    }
    sources
}

/// The host of `url`, without a `www.` prefix.
fn domain(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// `host/path` for a header, without the scheme, `www.` or a long tail.
fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest
        .strip_prefix("www.")
        .unwrap_or(rest)
        .trim_end_matches('/');
    if rest.chars().count() > 60 {
        format!("{}…", rest.chars().take(59).collect::<String>())
    } else {
        rest.to_string()
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_mixed_batches_reuse_standalone_cards() {
        let calls = json!({"calls": [
            {"tool":"search", "arguments":{"mode":"web", "query":"jev decision api"}},
            {"tool":"fetch_url", "arguments":{"url":"https://arxiv.org/abs/123"}},
            {"tool":"files", "arguments":{"action":"read", "name":"us_1.jpg"}},
            {"tool":"deck", "arguments":{"action":"create", "path":"decks/a.deck.json"}},
            {"tool":"media", "arguments":{"action":"generate_image", "prompt":"a landscape"}}
        ]});
        let result = "[1/5] search web \"jev decision api\"\n[1] Decision API\n    https://example.org/api\n    A decision endpoint.\n[2/5] fetch_url https://arxiv.org/abs/123\nPaper preview\n[3/5] files/read us_1.jpg\n![page](us_1.jpg)\n[4/5] deck\ncreated deck · decks/a.deck.json · 3 slides\n![3 slides](decks/.renders/a.png)\n[5/5] media/generate_image a landscape\n![landscape](landscape.png)";
        let view = tool_call_view("batch", &calls.to_string(), result);
        assert_eq!(view.summary(), "batch · 5 calls · 5 ok");
        assert_eq!(
            crate::app::tool_call_summary("batch", &calls.to_string(), result),
            view.summary()
        );
        assert_eq!(view.batch.len(), 5);
        for (entry, call) in view.batch.iter().zip(calls["calls"].as_array().unwrap()) {
            assert_eq!(
                entry.view,
                tool_call_view(
                    &entry.tool,
                    &call["arguments"].to_string(),
                    entry.result.as_deref().unwrap()
                )
            );
            assert_eq!(entry.status, Some(ToolOutcome::Ok));
        }
        assert_eq!(view.batch[0].view.sources[0].domain, "example.org");
        assert_eq!(view.batch[1].target, "arxiv.org/abs/123");
        assert_eq!(view.batch[2].target, "us_1.jpg");
        assert_eq!(view.batch[3].outcome, "3 slides");
        assert_eq!(view.batch[3].view.images[0].1, "decks/.renders/a.png");
        assert_eq!(view.batch[4].action, "generate_image");
        assert_eq!(
            view.batch[4].result.as_deref(),
            Some("![landscape](landscape.png)")
        );
    }

    #[test]
    fn batch_failures_blocks_and_unknown_tools_are_not_successes() {
        let args = json!({"calls": [
            {"tool":"not_a_tool"},
            {"tool":"skills", "arguments":{"action":"load", "name":"secret"}},
            {"tool":"fetch_url", "arguments":{"url":"https://example.org"}}
        ]});
        let result = "[1/3] not_a_tool\nunknown tool: not_a_tool\n[2/3] skills/load secret\ndenied by the user\n[3/3] fetch_url https://example.org\nfetch failed: unavailable\ntry again later";
        let view = tool_call_view("batch", &args.to_string(), result);
        assert_eq!(
            view.summary(),
            "batch · 3 calls · 0 ok · 2 failed · 1 blocked"
        );
        assert_eq!(view.outcome, ToolOutcome::Failed);
        assert_eq!(view.batch[0].outcome, "unknown tool: not_a_tool");
        assert_eq!(view.batch[1].status, Some(ToolOutcome::Blocked));
        assert_eq!(view.batch[1].target, "secret");
        assert_eq!(view.batch[2].outcome, "fetch failed: unavailable");
        let blocked = tool_call_view(
            "batch",
            r#"{"calls":[{"tool":"files","arguments":{"action":"read","name":"x"}}]}"#,
            "[1/1] files/read x\npath escapes the space directory",
        );
        assert_eq!(blocked.batch[0].status, Some(ToolOutcome::Blocked));
        assert_eq!(blocked.summary(), "batch · 1 calls · 0 ok · 1 blocked");
    }

    #[test]
    fn byte_frames_do_not_split_header_looking_page_text() {
        let args = json!({"calls": [
            {"tool":"fetch_url", "arguments":{"url":"https://example.org"}},
            {"tool":"files", "arguments":{"action":"read", "name":"x"}}
        ]});
        let page =
            "Unicode: 日本語\n[2/2] files/read x\n[batch-result: 3 bytes]\nnot a separate result";
        let result = format!(
            "[batch-result: {} bytes]\n[1/2] fetch_url https://example.org\n{page}\n[/batch-result]\n[batch-result: 4 bytes]\n[2/2] files/read x\nreal\n[/batch-result]",
            page.len()
        );
        let view = tool_call_view("batch", &args.to_string(), &result);
        assert_eq!(view.batch[0].result.as_deref(), Some(page));
        assert_eq!(view.batch[1].result.as_deref(), Some("real"));
        assert_eq!(view.summary(), "batch · 2 calls · 2 ok");
    }

    #[test]
    fn capped_batches_keep_partial_text_and_do_not_invent_missing_successes() {
        let args = json!({"calls": [
            {"tool":"files", "arguments":{"action":"read", "name":"one"}},
            {"tool":"files", "arguments":{"action":"read", "name":"two"}},
            {"tool":"files", "arguments":{"action":"read", "name":"three"}}
        ]});
        for result in [
            "[batch-result: 5 bytes]\n[1/3] files/read one\nfirst\n[/batch-result]\n[batch-result: 200 bytes]\n[2/3] files/read two\npartial 日本語\n... (tool result truncated)",
            "[1/3] files/read one\nfirst\n[2/3] files/read two\npartial 日本語\n... (tool result truncated)",
        ] {
            let view = tool_call_view("batch", &args.to_string(), result);
            assert_eq!(view.batch.len(), 3);
            assert_eq!(view.batch[0].result.as_deref(), Some("first"));
            assert!(
                view.batch[1]
                    .result
                    .as_deref()
                    .unwrap()
                    .contains("partial 日本語")
            );
            assert!(view.batch[1].truncated);
            assert_eq!(view.batch[2].status, None);
            assert_eq!(view.batch[2].result, None);
            assert_eq!(view.batch[2].outcome, "result not shown");
            assert!(view.summary().contains("1 not shown · 1 truncated"));
        }
    }

    #[test]
    fn a_whole_batch_denial_is_not_split_or_recursed() {
        let args = r#"{"calls":[{"tool":"files","arguments":{"action":"read","name":"x"}}]}"#;
        let view = tool_call_view("batch", args, "denied by policy");
        assert_eq!(view.outcome, ToolOutcome::Denied);
        assert_eq!(view.output.as_deref(), Some("denied by policy"));
        assert_eq!(view.batch[0].status, None);
        let nested = tool_call_view(
            "batch",
            r#"{"calls":[{"tool":"batch"}]}"#,
            "nested batch calls are not allowed",
        );
        assert!(nested.batch.is_empty());
    }
}

#[cfg(test)]
mod deck_tests {
    use super::*;

    fn view(action: &str, result: &str) -> ToolView {
        tool_call_view(
            "deck",
            &format!(r#"{{"action":"{action}","path":"decks/a.deck.json"}}"#),
            result,
        )
    }

    #[test]
    fn deck_calls_read_as_verbs_with_their_slides() {
        let created = view(
            "create",
            "created deck · decks/a.deck.json · 2 slides · revision abc\n![2 slides](decks/.renders/a/x.png)",
        );
        assert_eq!(
            created.summary(),
            "created deck decks/a.deck.json · 2 slides"
        );
        assert_eq!(
            created.images,
            [("2 slides".to_string(), "decks/.renders/a/x.png".to_string())]
        );

        let edited = view(
            "edit",
            "edited slide 3 · decks/a.deck.json · revision abc\n- update_block b1 on slide 3: text\n![slide 3](decks/.renders/a/y.png)",
        );
        assert_eq!(edited.verb, "edited slide 3");
        assert_eq!(
            edited.output.as_deref(),
            Some("- update_block b1 on slide 3: text")
        );
        assert_eq!(edited.images.len(), 1);
        assert_eq!(edited.outcome, ToolOutcome::Ok);

        assert_eq!(
            view(
                "view",
                "viewed 4 slides · decks/a.deck.json · revision abc\n![4 slides](x.png)"
            )
            .verb,
            "viewed 4 slides"
        );
        assert_eq!(
            view(
                "comment",
                "commented on slide 2 · decks/a.deck.json\nthread t"
            )
            .verb,
            "commented on slide 2"
        );
        assert_eq!(
            view("comment", "replied in thread t · decks/a.deck.json").verb,
            "replied in thread"
        );
        assert_eq!(
            view("resolve", "resolved thread t · decks/a.deck.json").verb,
            "resolved thread"
        );
        let read = view(
            "read",
            "decks/a.deck.json · \"A\" · 1 slide · revision abc\nslide 1",
        );
        assert_eq!(
            (read.verb.as_str(), read.stats.as_slice()),
            ("read deck", ["1 slide".to_string()].as_slice())
        );
    }

    #[test]
    fn failed_deck_calls_keep_their_error_and_show_no_slides() {
        let failed = view(
            "edit",
            "deck failed: ops[0]: no block \"x\". Nothing was written; read the deck and correct that operation.",
        );
        assert_eq!(failed.outcome, ToolOutcome::Failed);
        assert_eq!(failed.verb, "edit");
        assert!(failed.stats.contains(&"failed".to_string()));
        assert!(failed.images.is_empty());
        assert!(
            failed
                .output
                .as_deref()
                .is_some_and(|o| o.contains("Nothing was written"))
        );
    }
}

#[cfg(test)]
mod image_source_tests {
    use super::*;

    #[test]
    fn image_search_results_are_thumbnail_sources() {
        let result = "[1] LeBron James\n    https://upload.wikimedia.org/lebron.jpg\n    image · en.wikipedia.org · page https://en.wikipedia.org/wiki/LeBron_James";
        let view = tool_call_view(
            "search",
            r#"{"mode":"images","query":"LeBron James"}"#,
            result,
        );
        assert_eq!(view.sources.len(), 1);
        assert_eq!(
            view.sources[0].image.as_deref(),
            Some("https://upload.wikimedia.org/lebron.jpg")
        );
        assert_eq!(
            view.sources[0].url,
            "https://upload.wikimedia.org/lebron.jpg"
        );
        assert!(view.sources[0].snippet.contains("en.wikipedia.org"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn web_searches_list_their_sources_and_fetches_name_the_page() {
        let result = "[1] Mongolian Parliament Passes the 2026 Budget\n    https://mongolia.substack.com/p/budget\n    Mongolian Parliament Passes the 2026 Budget\n\nMongolia's 2026 budget passed on time.\n\n[2] World population\n    https://en.wikipedia.org/wiki/World_population\n    World population is the total number of people.";
        let view = tool_call_view(
            "search",
            r#"{"mode":"web","query":"Mongolia budget"}"#,
            result,
        );
        assert_eq!(view.summary(), "search \"Mongolia budget\" · 2 results");
        assert_eq!(view.sources.len(), 2);
        assert_eq!(view.sources[0].domain, "mongolia.substack.com");
        assert_eq!(
            view.sources[0].snippet,
            "Mongolia's 2026 budget passed on time."
        );
        assert_eq!(view.sources[1].domain, "en.wikipedia.org");
        let none = tool_call_view(
            "search",
            r#"{"mode":"news","query":"x"}"#,
            "no results for x",
        );
        assert_eq!(none.summary(), "news \"x\" · no results");
        let page = tool_call_view(
            "fetch_url",
            r#"{"url":"https://www.example.com/docs/page/"}"#,
            &"x".repeat(1500),
        );
        assert_eq!(page.summary(), "fetch example.com/docs/page · 1.5k chars");
    }

    #[test]
    fn search_sources_pick_up_thumbnail_lines_without_leaking_them_into_snippets() {
        let result = "[1] With picture\n    https://a.example/post\n    The snippet.\n    image: https://cdn.example/a.jpg\n\n[2] Plain\n    https://b.example/post\n    Another snippet.\n\ndiscussions:\n[3] Thread\n    https://www.reddit.com/r/rust/comments/abc/t/\n    r/rust · 2026-01-02 · u/x\n    Chat about it\n    image: https://preview.redd.it/p.jpg";
        let view = tool_call_view("search", r#"{"mode":"web","query":"q"}"#, result);
        assert_eq!(view.sources.len(), 3, "{:?}", view.sources);
        assert_eq!(
            view.sources[0].image.as_deref(),
            Some("https://cdn.example/a.jpg")
        );
        assert_eq!(view.sources[0].snippet, "The snippet.");
        assert!(view.sources[1].image.is_none());
        assert_eq!(view.sources[1].snippet, "Another snippet.");
        assert_eq!(
            view.sources[2].image.as_deref(),
            Some("https://preview.redd.it/p.jpg")
        );
        assert!(!view.sources[2].snippet.contains("image:"));
    }

    use super::*;
    use crate::app::tool_call_summary;

    use serde_json::json;

    #[test]
    fn filesystem_headers_and_diff_are_readable() {
        for (action, extra, result, expected) in [
            (
                "read",
                json!({}),
                "src/a (lines 1-200 of 831):\n1:abc|x",
                "read src/a · lines 1–200 of 831",
            ),
            (
                "grep",
                json!({"pattern":"fn parse"}),
                "src/a:\n  1: fn parse()\nsrc/b:\n  2: fn parse()",
                "grep \"fn parse\" in src/a · 2 matches in 2 files",
            ),
            (
                "glob",
                json!({"pattern":"*.rs"}),
                "a.rs\nb.rs",
                "glob *.rs in src/a · 2 entries",
            ),
            (
                "list",
                json!({}),
                "src/a/\nsrc/a/b\nsrc/a/c",
                "list src/a · 2 entries",
            ),
            (
                "edit",
                json!({}),
                "edited src/a (+3 −1)\n--- a/src/a\n+++ b/src/a\n@@ -1 +1 @@\n-old\n+new",
                "edit src/a  +3 −1",
            ),
            (
                "write",
                json!({}),
                "wrote src/a (+1 −0)",
                "write src/a  +1 −0",
            ),
            (
                "delete",
                json!({}),
                "deleted src/a (+0 −2)",
                "delete src/a  +0 −2",
            ),
            (
                "move",
                json!({"to":"src/b"}),
                "moved src/a -> src/b",
                "move src/a → src/b",
            ),
        ] {
            let mut args = json!({"action":action,"path":"src/a"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let view = tool_call_view("fs", &args.to_string(), result);
            assert_eq!(view.summary(), expected);
            assert_eq!(tool_call_summary("fs", &args.to_string(), result), expected);
            if action == "edit" {
                assert!(view.diff.unwrap().starts_with("--- a/src/a"));
            }
        }
    }

    #[test]
    fn command_outcomes_output_and_agent_stats() {
        for (result, outcome, suffix) in [
            (
                "exit 0 · 4.2s · shell action=log id=abc\nok",
                ToolOutcome::Ok,
                " · exit 0 · 4.2s",
            ),
            (
                "exit 101 · 9.8s\nerror",
                ToolOutcome::Failed,
                " · exit 101 · 9.8s",
            ),
            ("denied by the user · ask", ToolOutcome::Denied, " · denied"),
            (
                "privilege escalation is blocked: sudo",
                ToolOutcome::Blocked,
                " · blocked",
            ),
            (
                "shell write confinement unavailable",
                ToolOutcome::Blocked,
                " · blocked",
            ),
            ("error: broken", ToolOutcome::Failed, " · failed"),
        ] {
            let args = r#"{"action":"run","cmd":"cargo test"}"#;
            let view = tool_call_view("shell", args, result);
            assert_eq!(view.outcome, outcome);
            assert_eq!(
                tool_call_summary("shell", args, result),
                format!("$ cargo test{suffix}")
            );
            assert_eq!(
                view.output.as_deref(),
                result.split_once('\n').map(|(_, text)| text)
            );
        }
        for (args, result, expected) in [
            (
                r#"{"action":"start","cmd":"server"}"#,
                "abc · server · running 0m01s\nready",
                "start server · running 0m01s",
            ),
            (
                r#"{"action":"read","id":"abc"}"#,
                "abc · server · exited 1\nfailed",
                "read process abc · exited 1",
            ),
        ] {
            assert_eq!(tool_call_summary("proc", args, result), expected);
        }
        assert_eq!(
            tool_call_summary(
                "task",
                r#"{"prompt":"find callers"}"#,
                "sub-agent report (7 steps, 12400 tokens):\nreport"
            ),
            "task read-only · find callers · 7 steps · 12.4k tokens"
        );
        assert_eq!(
            tool_call_summary(
                "shell",
                r#"{"action":"run","cmd":"echo\nnext\u001b[2J"}"#,
                "exit 0"
            ),
            "$ echo\\u{a}next\\u{1b}[2J · exit 0"
        );
        assert_eq!(
            tool_call_summary("request_workspace", r#"{"root":"src"}"#, "approved"),
            "workspace src"
        );
        assert_eq!(
            tool_call_summary("propose_command", r#"{"cmd":"cargo test"}"#, "proposed"),
            "propose $ cargo test"
        );
    }

    #[test]
    fn odd_input_falls_back_without_panicking() {
        for name in ["fs", "shell", "proc", "task", "unknown"] {
            for args in ["not json", "[]", "null", "{}", r#"{"action":"surprise"}"#] {
                let view = tool_call_view(name, args, "garbage");
                assert!(view.verb.is_empty());
                assert_eq!(view.summary(), tool_call_summary(name, args, "garbage"));
            }
        }
        assert!(
            tool_call_view(
                "fs",
                r#"{"action":"edit","path":"a"}"#,
                "edited a (+oops −1)"
            )
            .verb
            .is_empty()
        );
        assert!(
            tool_call_view("shell", r#"{"action":"run","cmd":"true"}"#, "garbage")
                .verb
                .is_empty()
        );
    }
}
