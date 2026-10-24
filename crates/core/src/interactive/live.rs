//! Bounded, explicitly activated data calls. Parsing and rendering never execute tools.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tools::{ApprovalRequest, Approver, Decision, Gate, ToolExecutor};
use crate::trace::Scrubber;

/// Lifetime call budget for one panel (including failures and denied calls).
pub const PANEL_CALL_CAP: usize = 100;
/// Lifetime call budget for one conversation in this app process.
pub const SESSION_CALL_CAP: usize = 500;
const DEBOUNCE: Duration = Duration::from_millis(300);
const IDLE: Duration = Duration::from_mins(30);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    pub id: String,
    pub tool: String,
    pub query: bool,
    pub args: Value,
    pub defaults: Value,
    pub refresh: Option<f64>,
    pub dependencies: Vec<String>,
}

#[derive(Default)]
struct State {
    dependencies: Option<Vec<Value>>,
    due: Option<Instant>,
    refresh: Option<Instant>,
    requested: bool,
    loading: bool,
    data: Option<Value>,
    error: Option<String>,
    token: u64,
}

#[derive(Default)]
pub(super) struct Panel {
    active: bool,
    consent: bool,
    interaction: Option<Instant>,
    count: usize,
    serial: u64,
    states: BTreeMap<String, State>,
}

/// An already-budgeted call. Only a host's activated panel may create one.
#[derive(Clone, Debug)]
pub struct Call {
    /// Statement identifier, not a tool-supplied identifier.
    pub id: String,
    /// Tool name from the program.
    pub tool: String,
    /// Evaluated JSON object, never host credentials.
    pub arguments: String,
    query: bool,
    token: u64,
}

/// Completed tool output, bounded and scrubbed before rendering or logging.
pub struct Outcome {
    /// Compact text for the normal conversation tool card.
    pub text: String,
    /// Whether the tool completed successfully.
    pub success: bool,
    value: Value,
}

impl Panel {
    pub fn new(definitions: &[Definition]) -> Self {
        Self {
            states: definitions
                .iter()
                .map(|d| (d.id.clone(), State::default()))
                .collect(),
            ..Self::default()
        }
    }

    pub fn revise(&mut self, definitions: &[Definition]) {
        self.active = false;
        self.consent = false;
        self.states = Self::new(definitions).states;
    }

    pub fn interact(&mut self, now: Instant) {
        self.active = true;
        self.interaction = Some(now);
    }

    pub fn load(&mut self, now: Instant) {
        self.interact(now);
        self.consent = true;
    }

    pub fn needs_load(&self, definitions: &[Definition], state: &Map<String, Value>) -> bool {
        definitions.iter().any(|d| {
            let args = super::eval(&d.args, state, &mut 4096).unwrap_or(Value::Null);
            d.query && (!self.active || paid(&d.tool, &args) && !self.consent)
        })
    }

    /// Last successful query results, for persistence. Mutation status is
    /// transient and never saved.
    pub fn query_data(&self, definitions: &[Definition]) -> BTreeMap<String, Value> {
        definitions
            .iter()
            .filter(|d| d.query)
            .filter_map(|d| Some((d.id.clone(), self.states.get(&d.id)?.data.clone()?)))
            .collect()
    }

    /// Install (or clear) cached query results without scheduling any call:
    /// a restored panel stays inert until the person activates it.
    pub fn set_query_data(&mut self, definitions: &[Definition], data: &BTreeMap<String, Value>) {
        for definition in definitions.iter().filter(|d| d.query) {
            let mut value = data.get(&definition.id).cloned();
            if let Some(value) = &mut value {
                bound(value, &mut 4096, 0);
            }
            self.states.entry(definition.id.clone()).or_default().data = value;
        }
    }

    pub fn request(&mut self, id: &str) -> Result<()> {
        let state = self.states.entry(id.into()).or_default();
        ensure!(
            !state.loading && !state.requested,
            "data call is already loading"
        );
        state.requested = true;
        Ok(())
    }

    pub fn values(&self) -> Map<String, Value> {
        let mut values = Map::new();
        let mut loading = Map::new();
        let mut refetching = Map::new();
        let mut errors = Map::new();
        for (id, state) in &self.states {
            loading.insert(id.clone(), json!(state.loading && state.data.is_none()));
            refetching.insert(id.clone(), json!(state.loading && state.data.is_some()));
            errors.insert(id.clone(), json!(state.error));
            if let Some(value) = &state.data {
                values.insert(id.clone(), value.clone());
            }
        }
        values.insert("__openui_loading".into(), loading.into());
        values.insert("__openui_refetching".into(), refetching.into());
        values.insert("__openui_errors".into(), errors.into());
        values
    }

    pub fn hint(&self) -> Option<&str> {
        if self.states.values().any(|s| s.loading) {
            Some("Loading data")
        } else if self.states.values().any(|s| s.error.is_some()) {
            Some("Data error · press Run to retry")
        } else if self.count >= PANEL_CALL_CAP {
            Some("Panel call limit reached")
        } else {
            None
        }
    }

    pub fn next(
        &mut self,
        definitions: &[Definition],
        state: &Map<String, Value>,
        now: Instant,
        session_calls: &mut usize,
    ) -> Result<Option<Call>> {
        if self.states.values().any(|state| state.loading)
            || !self.active
            || self
                .interaction
                .is_none_or(|last| now.duration_since(last) >= IDLE)
        {
            return Ok(None);
        }
        for definition in definitions {
            let entry = self.states.entry(definition.id.clone()).or_default();
            if definition.query {
                let dependencies = definition
                    .dependencies
                    .iter()
                    .map(|key| state.get(key).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                if entry.dependencies.as_ref() != Some(&dependencies) {
                    entry.due = Some(if entry.dependencies.is_none() {
                        now
                    } else {
                        now + DEBOUNCE
                    });
                    entry.dependencies = Some(dependencies);
                }
            }
            if entry.loading
                || !(entry.requested
                    || definition.query
                        && (entry.due.is_some_and(|due| due <= now)
                            || entry.refresh.is_some_and(|due| due <= now)))
            {
                continue;
            }
            let args = super::eval(&definition.args, state, &mut 4096)?;
            if definition.query && paid(&definition.tool, &args) && !self.consent {
                continue;
            }
            ensure!(
                self.count < PANEL_CALL_CAP && *session_calls < SESSION_CALL_CAP,
                "UI data call limit reached"
            );
            ensure!(args.is_object(), "data arguments must be an object");
            let arguments = args.to_string();
            ensure!(
                arguments.len() <= super::MAX_SOURCE,
                "data arguments exceed 16 KiB"
            );
            self.serial += 1;
            self.count += 1;
            *session_calls += 1;
            entry.loading = true;
            entry.error = None;
            entry.requested = false;
            entry.due = None;
            entry.refresh = None;
            entry.token = self.serial;
            if !definition.query {
                entry.data = Some(json!({"status":"loading","data":null,"error":null}));
            }
            return Ok(Some(Call {
                id: definition.id.clone(),
                tool: definition.tool.clone(),
                arguments,
                query: definition.query,
                token: self.serial,
            }));
        }
        Ok(None)
    }

    pub fn finish(
        &mut self,
        call: &Call,
        outcome: &Outcome,
        current: Option<&Definition>,
        state: &Map<String, Value>,
        now: Instant,
    ) -> Result<bool> {
        let Some(entry) = self
            .states
            .get_mut(&call.id)
            .filter(|s| s.token == call.token)
        else {
            return Ok(false);
        };
        entry.loading = false;
        let Some(current) = current else {
            return Ok(false);
        };
        let args = super::eval(&current.args, state, &mut 4096)?;
        if call.query
            && (current.tool != call.tool
                || args != serde_json::from_str::<Value>(&call.arguments)?)
        {
            return Ok(false);
        }
        entry.error = (!outcome.success).then(|| bounded_text(&outcome.text));
        if call.query {
            if outcome.success {
                entry.data = Some(outcome.value.clone());
            }
            entry.refresh = current
                .refresh
                .and_then(|seconds| now.checked_add(Duration::from_secs_f64(seconds)));
        } else {
            entry.data = Some(
                json!({"status": if outcome.success {"success"} else {"error"}, "data": if outcome.success { outcome.value.clone() } else { Value::Null }, "error":entry.error}),
            );
        }
        Ok(true)
    }
}

fn paid(tool: &str, args: &Value) -> bool {
    // Backend pricing and connector billing are not represented by ToolExecutor.
    // Treat these as paid even when the selected backend happens to be free.
    !matches!(tool, "fetch_url" | "fs" | "deck" | "files")
        || tool == "files" && !matches!(args["action"].as_str(), Some("read" | "pdf_page"))
}

fn query_allowed(tool: &str, args: &str, executor: &dyn ToolExecutor) -> bool {
    (matches!(
        tool,
        "search" | "fetch_url" | "files" | "fs" | "deck" | "connectors"
    ) || tool.contains("__"))
        && executor.is_read_only(tool, args)
}

/// Execute a budgeted call with the existing tool and approval seams.
/// Queries accept only the advertised read-only allow-list. Mutations always
/// ask the person, except for free read-only calls; grants never skip this ask.
pub async fn execute(
    call: &Call,
    panel: &str,
    executor: Arc<dyn ToolExecutor>,
    approver: Option<Arc<dyn Approver>>,
    scrubber: &Scrubber,
) -> Outcome {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let available = executor.defs().iter().any(|d| d.name == call.tool);
    let denied = if !available {
        Some("tool is not available")
    } else if call.query && !query_allowed(&call.tool, &call.arguments, executor.as_ref()) {
        Some("Query requires an allowed read-only tool")
    } else if contains_credentials(&args)
        || serde_json::from_str::<Value>(&redacted_arguments(&call.arguments, scrubber))
            .ok()
            .as_ref()
            != Some(&args)
    {
        Some("credentials are not allowed in panel arguments")
    } else {
        None
    };
    if let Some(error) = denied {
        return outcome(error, false, scrubber);
    }
    if !call.query
        && (!executor.is_read_only(&call.tool, &call.arguments) || paid(&call.tool, &args))
    {
        let request = ApprovalRequest {
            tool: call.tool.clone(),
            action: "panel mutation".into(),
            summary: format!("Panel {panel}\n{}", scrubber.scrub(&call.arguments)),
            class: Some("panel mutation".into()),
            detail: None,
            allow_session: false,
        };
        let decision = match approver {
            Some(approver) => {
                tokio::time::timeout(Duration::from_secs(120), approver.request(request))
                    .await
                    .unwrap_or(Decision::Deny)
            }
            None => Decision::Deny,
        };
        if decision != Decision::Allow {
            return outcome("denied by the user", false, scrubber);
        }
    }
    match tokio::time::timeout(
        Duration::from_secs(30),
        executor.run_gated(&call.tool, &call.arguments, Gate::Approved),
    )
    .await
    {
        Ok((text, status)) => {
            let success = !matches!(status.as_str(), "blocked" | "error" | "failed")
                && crate::tools::classify_tool_result(&text, &status).is_none();
            outcome(&text, success, scrubber)
        }
        Err(_) => outcome("panel tool timed out", false, scrubber),
    }
}

/// Redact both recognizable secrets and credential-shaped fields in tool cards.
#[must_use]
pub fn redacted_arguments(arguments: &str, scrubber: &Scrubber) -> String {
    let mut value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    scrub_value(&mut value, scrubber);
    value.to_string()
}

fn scrub_value(value: &mut Value, scrubber: &Scrubber) {
    match value {
        Value::Object(fields) => {
            for (key, mut value) in std::mem::take(fields) {
                if credential_key(&key) {
                    value = "[REDACTED]".into();
                } else {
                    scrub_value(&mut value, scrubber);
                }
                fields.insert(scrubber.scrub(&key), value);
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_value(item, scrubber);
            }
        }
        Value::String(text) => *text = scrubber.scrub(text),
        _ => {}
    }
}

fn credential_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "api_key" | "apikey" | "authorization" | "password" | "token" | "secret" | "headers"
    )
}

fn contains_credentials(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| credential_key(key) || contains_credentials(value)),
        Value::Array(items) => items.iter().any(contains_credentials),
        _ => false,
    }
}

fn outcome(text: &str, success: bool, scrubber: &Scrubber) -> Outcome {
    let text = scrubber.scrub(text);
    let mut parsed = serde_json::from_str::<Value>(&text).ok();
    if let Some(value) = &mut parsed {
        scrub_value(value, scrubber);
    }
    let text = crate::tools::cap_tool_result(parsed.as_ref().map_or(text, Value::to_string));
    let mut value = parsed
        .unwrap_or_else(|| json!({"text":text,"lines":text.lines().take(200).collect::<Vec<_>>()}));
    bound(&mut value, &mut 4096, 0);
    Outcome {
        text,
        success,
        value,
    }
}

fn bounded_text(text: &str) -> String {
    let mut text: String = text
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .take(1024)
        .collect();
    let mut end = text.len().min(1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn bound(value: &mut Value, budget: &mut usize, depth: usize) {
    if *budget == 0 || depth > 12 {
        *value = Value::Null;
        return;
    }
    *budget -= 1;
    match value {
        Value::String(text) => *text = bounded_text(text),
        Value::Array(items) => {
            items.truncate(200);
            let mut count = 0;
            for item in items.iter_mut() {
                if *budget == 0 {
                    break;
                }
                bound(item, budget, depth + 1);
                count += 1;
            }
            items.truncate(count);
        }
        Value::Object(fields) => {
            let keys: Vec<_> = fields.keys().cloned().collect();
            for (index, key) in keys.into_iter().enumerate() {
                if index >= 200
                    || key.len() > 1024
                    || key.chars().any(char::is_control)
                    || *budget == 0
                {
                    fields.remove(&key);
                } else if let Some(value) = fields.get_mut(&key) {
                    bound(value, budget, depth + 1);
                }
            }
        }
        Value::Number(number) if number.as_f64().is_some_and(|n| n.abs() > 1e12) => {
            *value = Value::Null;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
