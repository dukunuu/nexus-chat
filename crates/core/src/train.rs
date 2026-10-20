//! Offline training-data export from scrubbed model-call traces.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use chrono::NaiveDate;
use serde::Serialize;
use serde_json::{Value, json};

use crate::eval;
use crate::tools::classify_tool_result;
use crate::trace::{Scrubber, TraceStore};

#[cfg(test)]
mod tests;

/// Export destination and selection. An absent space selects every space.
pub struct ExportOptions {
    pub format: Format,
    pub filter: Filter,
    pub models: Vec<String>,
    pub since: Option<NaiveDate>,
    pub space_id: Option<String>,
    pub evals: PathBuf,
    pub out: PathBuf,
    pub split: bool,
}

/// Supported training-data encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    ChatJsonl,
    Sharegpt,
    Dpo,
}

impl Format {
    /// Filename stem and CLI spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ChatJsonl => "chat-jsonl",
            Self::Sharegpt => "sharegpt",
            Self::Dpo => "dpo",
        }
    }
}

/// Whether to require successful outcomes (rejections are never chosen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Success,
    All,
}

/// Counts are per turn except `steps_read`; scrub flags overlap kept turns.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub steps_read: usize,
    pub turns_kept: usize,
    pub dropped: BTreeMap<String, usize>,
    pub scrub_flagged: usize,
    pub warnings: Vec<String>,
}

impl Report {
    fn drop_turn(&mut self, reason: &str) {
        *self.dropped.entry(reason.into()).or_default() += 1;
    }
}

/// Export without fetching models or executing tools. Catalog keys are bare
/// `OpenRouter` model IDs; values retain the distinction between missing metadata
/// and an explicit false `is_trainable_text` flag. Offline callers with no
/// catalog must supply an empty map, never infer permission from a model name.
pub fn export(
    store: &TraceStore,
    options: &ExportOptions,
    scrubber: &Scrubber,
    catalog: &BTreeMap<String, Option<bool>>,
) -> Result<Report> {
    if options.split && options.format != Format::ChatJsonl {
        bail!("--split is only supported for chat-jsonl");
    }
    // A missing eval directory is an error, not permission to skip decontamination.
    let eval_texts: Vec<_> = eval::load_tasks(&options.evals)?
        .iter()
        .flat_map(eval::EvalTask::prompts)
        .map(|text| normalize(text))
        .filter(|text| !text.is_empty())
        .collect();
    let since = options.since.map(|date| format!("{date}T00:00:00+00:00"));
    let ids = store.steps_for_export(options.space_id.as_deref(), since.as_deref())?;
    let mut report = Report {
        steps_read: ids.len(),
        dropped: [
            "not_successful",
            "license",
            "stale_schema",
            "eval_match",
            "no_retry_pair",
        ]
        .into_iter()
        .map(|reason| (reason.into(), 0))
        .collect(),
        ..Report::default()
    };
    if options
        .models
        .iter()
        .any(|pattern| pattern == "distillable")
        && !catalog.values().any(Option::is_some)
    {
        report.warnings.push(
            "no distillable metadata in the model catalog · pass explicit model patterns".into(),
        );
    }
    report.warnings.push(
        "license checking is the user's responsibility, including the model license and provider's terms; explicit model patterns override the default exclusions".into(),
    );
    let turns = load_turns(store, &ids)?;
    let mut records = Vec::new();
    let mut rejected = HashMap::new();
    for turn in &turns {
        let final_step = turn.last().context("empty recorded turn")?;
        let session = text(final_step, "session_id");
        let previous = rejected.remove(session);
        if is_rejected(turn) {
            if turn
                .iter()
                .any(|step| has_label(step, "rejected", Some("retry")))
            {
                rejected.insert(session, turn);
            }
            report.drop_turn("not_successful");
            continue;
        }
        if let Some(reason) = drop_reason(turn, options, catalog, &eval_texts) {
            report.drop_turn(reason);
            continue;
        }
        let chat = conversation(turn).context("validated tool schemas")?;
        let record = match options.format {
            Format::ChatJsonl => chat,
            Format::Sharegpt => sharegpt(&chat),
            Format::Dpo => {
                let Some(previous) = previous.filter(|previous| {
                    prompt(&previous[0]) == prompt(&turn[0])
                        && safety_reason(previous, options, catalog, &eval_texts).is_none()
                }) else {
                    report.drop_turn("no_retry_pair");
                    continue;
                };
                dpo(turn, previous).context("validated DPO pair")?
            }
        };
        let (record, flagged) = rescrub(record, scrubber);
        report.scrub_flagged += usize::from(flagged);
        records.push(record);
    }
    report.turns_kept = records.len();
    fs::create_dir_all(&options.out)?;
    write_jsonl(
        &options.out.join(format!("{}.jsonl", options.format.name())),
        &records,
    )?;
    if options.split {
        let valid = records.len() / 10;
        let split = records.len() - valid;
        write_jsonl(&options.out.join("train.jsonl"), &records[..split])?;
        write_jsonl(&options.out.join("valid.jsonl"), &records[split..])?;
    }
    fs::write(
        options.out.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

fn load_turns(store: &TraceStore, ids: &[String]) -> Result<Vec<Vec<Value>>> {
    let mut positions = HashMap::new();
    let mut turns: Vec<Vec<Value>> = Vec::new();
    for id in ids {
        let step = store.load_step(id)?;
        let next = turns.len();
        let position = *positions
            .entry(text(&step, "turn_id").to_string())
            .or_insert(next);
        if position == next {
            turns.push(Vec::new());
        }
        turns[position].push(step);
    }
    Ok(turns)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

fn has_label(step: &Value, kind: &str, value: Option<&str>) -> bool {
    array(step, "labels")
        .iter()
        .any(|label| label["kind"] == kind && value.is_none_or(|value| label["value"] == value))
}

fn is_rejected(turn: &[Value]) -> bool {
    turn.iter().any(|step| has_label(step, "rejected", None))
}

fn successful(turn: &[Value]) -> bool {
    !is_rejected(turn)
        && turn.last().is_some_and(|step| step["finish"] == "stop")
        && !turn
            .iter()
            .flat_map(|step| array(step, "tool_results"))
            .any(|result| {
                ["seen", "full"].iter().any(|key| {
                    let result = text(result, key);
                    let line = result.trim_start().to_lowercase();
                    // Trace results do not retain the status string. Restore the
                    // blocked status for denial/refusal prefixes before classifying.
                    let status = if ["denied", "blocked", "refused", "path denied:"]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
                    {
                        "blocked"
                    } else {
                        ""
                    };
                    matches!(
                        classify_tool_result(result, status),
                        Some("blocked" | "confinement")
                    )
                })
            })
}

fn drop_reason(
    turn: &[Value],
    options: &ExportOptions,
    catalog: &BTreeMap<String, Option<bool>>,
    evals: &[String],
) -> Option<&'static str> {
    if (options.filter == Filter::Success || options.format == Format::Dpo) && !successful(turn) {
        return Some("not_successful");
    }
    safety_reason(turn, options, catalog, evals)
}

fn safety_reason(
    turn: &[Value],
    options: &ExportOptions,
    catalog: &BTreeMap<String, Option<bool>>,
    evals: &[String],
) -> Option<&'static str> {
    if !turn
        .iter()
        .all(|step| licensed(step, &options.models, catalog))
    {
        Some("license")
    } else if turn.iter().any(|step| mapped_step(step).is_none()) {
        Some("stale_schema")
    } else if turn
        .iter()
        .flat_map(|step| array(step, "messages"))
        .filter(|message| message["role"] == "user")
        .any(|message| eval_match(&message_text(&message["content"]), evals))
    {
        Some("eval_match")
    } else {
        None
    }
}

fn backend_name(backend: &str) -> String {
    match backend.to_ascii_lowercase().as_str() {
        "openrouter" => "openrouter",
        "openai" => "openai",
        "codex" => "codex",
        "opencode go" | "opencode" => "opencode",
        "local" => "local",
        _ => backend,
    }
    .to_string()
}

fn licensed(step: &Value, patterns: &[String], catalog: &BTreeMap<String, Option<bool>>) -> bool {
    let backend = backend_name(text(step, "backend"));
    let model = text(step, "model");
    let prefix = format!("{backend}:");
    let model = model.strip_prefix(&prefix).unwrap_or(model);
    let qualified = format!("{prefix}{model}");
    patterns.iter().any(|pattern| {
        if pattern == "distillable" {
            backend == "openrouter" && catalog.get(model) == Some(&Some(true))
        } else if pattern.contains(':') {
            glob(pattern, &qualified)
        } else {
            glob(pattern, model)
        }
    })
}

// Only '*' and '?' are needed for model-ID globs; no filesystem expansion.
fn glob(pattern: &str, value: &str) -> bool {
    let pattern: Vec<_> = pattern.chars().collect();
    let mut matches = vec![false; pattern.len() + 1];
    matches[0] = true;
    for index in 0..pattern.len() {
        matches[index + 1] = matches[index] && pattern[index] == '*';
    }
    for ch in value.chars() {
        let mut previous = matches[0];
        matches[0] = false;
        for index in 0..pattern.len() {
            let old = matches[index + 1];
            matches[index + 1] = if pattern[index] == '*' {
                matches[index] || old
            } else {
                previous && (pattern[index] == '?' || pattern[index] == ch)
            };
            previous = old;
        }
    }
    matches[pattern.len()]
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn grams(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .windows(8)
        .map(|words| words.join(" "))
        .collect()
}

fn eval_match(user: &str, evals: &[String]) -> bool {
    let user = normalize(user);
    let user_grams = grams(&user);
    evals.iter().any(|eval| {
        let eval_grams = grams(eval);
        user.contains(eval)
            || (!user_grams.is_empty()
                && user_grams.intersection(&eval_grams).count() * 2 >= user_grams.len())
    })
}

fn message_text(content: &Value) -> String {
    content.as_str().map_or_else(
        || {
            content.as_array().map_or_else(String::new, |parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        },
        str::to_string,
    )
}

fn prompt(step: &Value) -> &[Value] {
    let messages = array(step, "messages");
    messages
        .iter()
        .rposition(|message| message["role"] == "user")
        .map_or(&[], |index| &messages[..=index])
}

fn is_current_tool(name: &str) -> bool {
    crate::tools::current_tools()
        .iter()
        .any(|(tool, _)| *tool == name)
}

fn map_call(name: &str, args: &mut Value) -> Option<String> {
    let name = if is_current_tool(name) {
        name.to_string()
    } else {
        let (tool, _, mapped) = crate::tools::migrate_tool_call(name, args.take())?;
        *args = mapped;
        tool.to_string()
    };
    if name == "batch" {
        for call in args["calls"].as_array_mut()? {
            let tool = text(call, "tool").to_string();
            if call.get("arguments").is_none() {
                call["arguments"] = json!({});
            }
            call["tool"] = json!(map_call(&tool, &mut call["arguments"])?);
        }
    }
    Some(name)
}

fn map_definition(mut def: Value) -> Option<Value> {
    let name = text(&def, "name");
    if is_current_tool(name) {
        return Some(def);
    }
    let migrations = if let Some(mapped) = crate::tools::migrate_tool_call(name, json!({})) {
        vec![mapped]
    } else {
        def["parameters"]["properties"]["action"]["enum"]
            .as_array()?
            .iter()
            .map(|action| crate::tools::migrate_tool_call(name, json!({"action": action})))
            .collect::<Option<Vec<_>>>()?
    };
    let (tool, selector, _) = migrations.first()?;
    let mut actions = Vec::new();
    for (target, key, args) in &migrations {
        if target != tool || key != selector {
            return None;
        }
        actions.push(args[*selector].clone());
    }
    def["name"] = json!(tool);
    def["parameters"]["properties"][*selector] = json!({"type": "string", "enum": actions});
    if def["parameters"].get("required").is_none() {
        def["parameters"]["required"] = json!([]);
    }
    let required = def["parameters"]["required"].as_array_mut()?;
    if !required.contains(&json!(selector)) {
        required.push(json!(selector));
    }
    Some(def)
}

fn map_wire_calls(message: &mut Value) -> Option<()> {
    if let Some(calls) = message.get_mut("tool_calls") {
        for call in calls.as_array_mut()? {
            let function = &mut call["function"];
            let name = text(function, "name").to_string();
            let mut args = serde_json::from_str::<Value>(text(function, "arguments")).ok()?;
            function["name"] = json!(map_call(&name, &mut args)?);
            function["arguments"] = json!(serde_json::to_string(&args).ok()?);
        }
    }
    Some(())
}

fn mapped_step(step: &Value) -> Option<(Vec<Value>, Vec<Value>)> {
    let mut defs: Vec<Value> = Vec::new();
    for def in array(step, "tools") {
        let def = map_definition(def.clone())?;
        if let Some(existing) = defs
            .iter_mut()
            .find(|existing| existing["name"] == def["name"])
        {
            merge_definition(existing, &def)?;
        } else {
            defs.push(def);
        }
    }
    let mut messages = array(step, "messages").to_vec();
    for message in &mut messages {
        map_wire_calls(message)?;
    }
    // Raw responses store flat ToolCall values, unlike request messages.
    for call in array(&step["response"], "tool_calls") {
        let mut args = serde_json::from_str::<Value>(text(call, "arguments")).ok()?;
        map_call(text(call, "name"), &mut args)?;
    }
    Some((messages, defs))
}

fn merge_definition(existing: &mut Value, def: &Value) -> Option<()> {
    let parameters = &mut existing["parameters"];
    for (key, property) in def["parameters"]["properties"].as_object()? {
        let old = &mut parameters["properties"][key];
        if let (Some(old_values), Some(values)) = (old.get_mut("enum"), property.get("enum")) {
            let old_values = old_values.as_array_mut()?;
            for value in values.as_array()? {
                if !old_values.contains(value) {
                    old_values.push(value.clone());
                }
            }
        } else if old.is_null() {
            *old = property.clone();
        }
    }
    if let Some(required) = parameters["required"].as_array_mut() {
        required.retain(|key| array(&def["parameters"], "required").contains(key));
    }
    Some(())
}

fn conversation(turn: &[Value]) -> Option<Value> {
    let step = turn.last()?;
    let (mut messages, defs) = mapped_step(step)?;
    // Preserve the final request's result shortening/dedup notes exactly. The
    // final request already contains preceding rounds; never rebuild from full.
    messages.insert(0, json!({"role": "system", "content": step["system"]}));
    let calls = array(&step["response"], "tool_calls")
        .iter()
        .map(|call| {
            json!({"id": call["id"], "type": "function", "function": {
                "name": call["name"], "arguments": call["arguments"],
            }})
        })
        .collect::<Vec<_>>();
    let mut response = json!({"role": "assistant", "content": step["response"]["content"]});
    if !calls.is_empty() {
        response["tool_calls"] = json!(calls);
    }
    if let Some(reasoning) = step["response"]["reasoning"].as_str() {
        response["reasoning_content"] = json!(reasoning);
    }
    map_wire_calls(&mut response)?;
    messages.push(response);
    let tools: Vec<_> = defs
        .into_iter()
        .map(|def| json!({"type": "function", "function": def}))
        .collect();
    Some(json!({"messages": messages, "tools": tools}))
}

fn dpo(turn: &[Value], rejected: &[Value]) -> Option<Value> {
    let final_step = turn.last()?;
    let (messages, _) = mapped_step(&turn[0])?;
    let prompt_end = messages
        .iter()
        .rposition(|message| message["role"] == "user")?;
    let mut messages = messages[..=prompt_end].to_vec();
    messages.insert(
        0,
        json!({"role": "system", "content": final_step["system"]}),
    );
    Some(json!({
        "prompt": messages,
        "chosen": final_step["response"]["content"],
        "rejected": rejected.last()?["response"]["content"],
    }))
}

fn sharegpt(chat: &Value) -> Value {
    let conversations: Vec<_> = array(chat, "messages")
        .iter()
        .map(|message| {
            let role = match text(message, "role") {
                "user" => "human",
                "assistant" => "gpt",
                other => other,
            };
            let value = if message.get("tool_calls").is_some() {
                message.to_string()
            } else {
                message_text(&message["content"])
            };
            json!({"from": role, "value": value})
        })
        .collect();
    json!({"conversations": conversations})
}

fn rescrub(value: Value, scrubber: &Scrubber) -> (Value, bool) {
    let mut flagged = false;
    let value = match value {
        Value::String(mut text) => {
            // Arguments and ShareGPT calls contain JSON inside JSON strings.
            // Decode those too, so escaped credentials cannot evade redaction.
            if let Ok(encoded @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(&text)
            {
                let (clean, changed) = rescrub(encoded, scrubber);
                if changed {
                    text = clean.to_string();
                    flagged = true;
                }
            }
            let clean = scrubber.scrub(&text);
            flagged |= clean != text;
            Value::String(clean)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| {
                    let (value, changed) = rescrub(value, scrubber);
                    flagged |= changed;
                    value
                })
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let clean = scrubber.scrub(&key);
                    let (value, changed) = rescrub(value, scrubber);
                    flagged |= changed || clean != key;
                    (clean, value)
                })
                .collect(),
        ),
        other => other,
    };
    (value, flagged)
}

fn write_jsonl(path: &std::path::Path, records: &[Value]) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    for record in records {
        serde_json::to_writer(&mut writer, record)?;
        writeln!(writer)?;
    }
    writer.flush()?;
    Ok(())
}
