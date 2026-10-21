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
}

/// One web search hit: what a reader scans for in a results list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
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
    };
    let first = result.lines().next().unwrap_or_default();
    let (tool, action) = name
        .split_once('_')
        .filter(|(tool, _)| matches!(*tool, "fs" | "shell" | "proc"))
        .unwrap_or((name, field("action").unwrap_or("")));
    match tool {
        "ui_check" => {
            view.verb = "ui check".into();
            let source = crate::tools::ui_check_source(field("source")?);
            let title = serde_json::from_str::<Value>(source)
                .ok()
                .and_then(|value| {
                    value
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            view.target = format!("{:?}", title.as_deref().unwrap_or("UI"));
            if first.starts_with("ok · ") {
                view.stats.push("ok".into());
            } else if let Some(count) = first
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<usize>().ok())
            {
                view.stats.push(format!(
                    "{count} error{}",
                    if count == 1 { "" } else { "s" }
                ));
                view.outcome = ToolOutcome::Failed;
            } else {
                view.stats.push(first.into());
            }
            view.output = Some(result.into());
        }
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
        ToolOutcome::Failed
            if tool != "ui_check" && !view.stats.iter().any(|stat| stat.starts_with("exit")) =>
        {
            view.stats.push("failed".into());
        }
        _ => {}
    }
    Some(view)
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
        while let Some(text) = lines.next_if(|line| !line.starts_with('[')) {
            let text = text.trim();
            if text.is_empty() || text == title {
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

    use super::*;
    use crate::app::tool_call_summary;

    #[test]
    fn ui_check_cards_name_the_document_and_mark_errors_as_failed() {
        let args = serde_json::json!({"source":"{\"title\":\"Quiz\"}"}).to_string();
        for (result, summary, expected) in [
            (
                "ok · \"Quiz\" · 0 controls, 0 actions · static\npreview:\nQuiz",
                "ui check \"Quiz\" · ok",
                ToolOutcome::Ok,
            ),
            (
                "2 errors\nview[0]: unknown node type",
                "ui check \"Quiz\" · 2 errors",
                ToolOutcome::Failed,
            ),
        ] {
            let view = tool_call_view("ui_check", &args, result);
            assert_eq!(view.summary(), summary);
            assert_eq!(tool_call_summary("ui_check", &args, result), summary);
            assert_eq!(view.outcome, expected);
            assert!(!view.summary().contains('◇'));
        }
        let view = tool_call_view("ui_check", r#"{"source":"{"}"#, "1 error\n$: EOF");
        assert_eq!(view.outcome, ToolOutcome::Failed);
        assert_eq!(view.summary(), "ui check \"UI\" · 1 error");
    }
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
