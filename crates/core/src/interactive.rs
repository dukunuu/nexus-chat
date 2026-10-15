//! Bounded, model-authored interactive UI documents. This is data, not code:
//! scalar state, a small expression language, controls and transactional actions.
//! No filesystem, network, HTML, processes, timers or provider access exist here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Model instructions advertised only by clients with an interactive renderer.
pub const INSTRUCTIONS: &str = include_str!("../assets/interactive-ui.md");

/// Maximum serialized UI document size.
pub const MAX_SOURCE: usize = 16 * 1024;
/// Maximum transcript height, including title and interaction hint.
pub const ROWS: u16 = 24;
const MAX_TEXT: usize = 4096;
const MAX_VALUE: usize = 1024;

/// Semantic text emphasis, supplied by the frontend's theme.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Ordinary foreground text.
    #[default]
    Normal,
    /// Secondary information.
    Muted,
    /// Highlighted information.
    Accent,
    /// Positive result.
    Success,
    /// Caution or attention.
    Warning,
    /// Negative result or error.
    Error,
}

/// An action control; labels are templates, never markup.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    /// Visible label (a sending action gets an additional frontend badge).
    pub label: String,
    /// Identifier in the document's actions map.
    pub action: String,
    /// Optional plain-text artwork or descriptive lines.
    #[serde(default)]
    pub body: Vec<String>,
}

/// Declarative presentation nodes. Their declared rows never grow with state.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// Plain text with `{{state_key}}` substitutions.
    Text {
        /// Plain-text template.
        text: String,
        /// Theme-provided semantic emphasis.
        #[serde(default)]
        tone: Tone,
        /// Declared region height.
        #[serde(default = "one")]
        rows: u16,
    },
    /// A row of locally executed action buttons.
    Buttons {
        /// Controls arranged within the declared region.
        items: Vec<Button>,
        /// Declared region height.
        #[serde(default = "three")]
        rows: u16,
    },
    /// A single-line string state editor.
    Input {
        /// Bound string state field.
        key: String,
        /// Visible field label.
        label: String,
    },
}
const fn one() -> u16 {
    1
}
const fn three() -> u16 {
    3
}
impl Node {
    /// Stable space occupied by this node.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        match self {
            Self::Text { rows, .. } | Self::Buttons { rows, .. } => *rows,
            Self::Input { .. } => 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Set { key: String, value: Value },
    Call { action: String },
    Send { text: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    version: u8,
    title: String,
    state: Map<String, Value>,
    view: Vec<Node>,
    actions: BTreeMap<String, Vec<Step>>,
}

/// A validated version-1 UI document. Construction is through `parse` only.
#[derive(Clone, Debug)]
pub struct Document {
    version: u8,
    title: String,
    state: Map<String, Value>,
    view: Vec<Node>,
    actions: BTreeMap<String, Vec<Step>>,
    sending: BTreeSet<String>,
}

impl Document {
    /// Parse and validate all controls, expressions and action paths.
    /// # Errors
    /// Rejects unknown constructs, oversized data, cycles and invalid bindings.
    pub fn parse(source: &str) -> Result<Self> {
        ensure!(source.len() <= MAX_SOURCE, "UI document exceeds 16 KiB");
        let Definition {
            version,
            title,
            state,
            view,
            actions,
        } = serde_json::from_str(source)?;
        let mut doc = Self {
            version,
            title,
            state,
            view,
            actions,
            sending: BTreeSet::new(),
        };
        ensure!(doc.version == 1, "unsupported UI version");
        line_ok(&doc.title, 120)?;
        ensure!(!doc.title.is_empty(), "UI title is required");
        ensure!(
            doc.state.len() <= 32 && doc.actions.len() <= 32,
            "too many UI state fields or actions"
        );
        let mut budget = 1024;
        for (key, value) in &doc.state {
            identifier(key)?;
            ensure!(
                !value.is_object() || value.get("random").is_some(),
                "initial state must be scalar or a random choice"
            );
            validate_expr(value, &doc.state, &mut budget, 0)?;
        }
        doc.validate_view()?;
        let mut steps = 0;
        for (name, action) in &doc.actions {
            identifier(name)?;
            steps += action.len();
            for step in action {
                match step {
                    Step::Set { key, value } => {
                        ensure!(doc.state.contains_key(key), "unknown state field: {key}");
                        validate_expr(value, &doc.state, &mut budget, 0)?;
                    }
                    Step::Call { action } => {
                        ensure!(doc.actions.contains_key(action), "unknown action: {action}");
                    }
                    Step::Send { text } => doc.template_ok(text)?,
                }
            }
        }
        ensure!(steps <= 128, "too many action steps");
        for name in doc.actions.keys() {
            if doc.action_sends(name, &mut BTreeSet::new(), 0, &mut budget)? {
                doc.sending.insert(name.clone());
            }
        }
        Ok(doc)
    }

    fn validate_view(&self) -> Result<()> {
        ensure!(
            !self.view.is_empty() && self.view.len() <= 16,
            "UI needs 1–16 view nodes"
        );
        let mut rows = 0;
        let mut controls = 0;
        for node in &self.view {
            ensure!((1..=8).contains(&node.rows()), "node rows must be 1–8");
            rows += node.rows();
            match node {
                Node::Text { text, .. } => self.template_ok(text)?,
                Node::Buttons { items, .. } => {
                    ensure!(
                        !items.is_empty() && items.len() <= 6,
                        "a button row needs 1–6 buttons"
                    );
                    for button in items {
                        line_ok(&button.label, 120)?;
                        self.template_ok(&button.label)?;
                        ensure!(
                            self.actions.contains_key(&button.action),
                            "unknown button action: {}",
                            button.action
                        );
                        ensure!(button.body.len() <= 8, "too many button artwork lines");
                        for line in &button.body {
                            line_ok(line, 120)?;
                        }
                    }
                    controls += items.len();
                }
                Node::Input { key, label } => {
                    ensure!(self.state.contains_key(key), "unknown input field: {key}");
                    line_ok(label, 120)?;
                    let value = &self.state[key];
                    if let Some(values) = value.get("random").and_then(Value::as_array) {
                        for value in values {
                            input_ok(value)?;
                        }
                    } else {
                        input_ok(value)?;
                    }
                    controls += 1;
                }
            }
        }
        ensure!(
            rows <= ROWS - 3 && controls <= 32,
            "UI exceeds its fixed display budget"
        );
        Ok(())
    }

    fn template_ok(&self, text: &str) -> Result<()> {
        text_ok(text, MAX_TEXT)?;
        let mut rest = text;
        while let Some((_, tail)) = rest.split_once("{{") {
            let (key, after) = tail
                .split_once("}}")
                .ok_or_else(|| anyhow::anyhow!("unclosed UI template"))?;
            ensure!(
                self.state.contains_key(key.trim()),
                "unknown template field: {}",
                key.trim()
            );
            rest = after;
        }
        Ok(())
    }

    fn action_sends(
        &self,
        action: &str,
        seen: &mut BTreeSet<String>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<bool> {
        spend(budget)?;
        ensure!(
            depth < 8 && seen.insert(action.into()),
            "cyclic or deeply nested UI action"
        );
        let mut sends = false;
        for step in &self.actions[action] {
            spend(budget)?;
            match step {
                Step::Send { .. } => sends = true,
                Step::Call { action } => {
                    sends |= self.action_sends(action, seen, depth + 1, budget)?;
                }
                Step::Set { .. } => {}
            }
        }
        seen.remove(action);
        Ok(sends)
    }

    /// Document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    /// Immutable, validated presentation nodes.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.view
    }
    /// Fixed height for this document; local state changes cannot reflow it.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.view.iter().map(Node::rows).sum::<u16>() + 3
    }
    /// Whether activation can send a message, for explicit frontend labelling.
    #[must_use]
    pub fn sends(&self, action: &str) -> bool {
        self.sending.contains(action)
    }
}

/// Local runtime for one document. Frontends own its lifetime and identity.
pub struct Session {
    document: Document,
    state: Map<String, Value>,
}

impl Session {
    /// Initialize scalar state; random choices happen once, not during drawing.
    /// # Errors
    /// Returns invalid initial bindings or values.
    pub fn new(document: Document) -> Result<Self> {
        let mut state = Map::new();
        for (key, value) in &document.state {
            state.insert(key.clone(), eval(value, &Map::new(), &mut 1024)?);
        }
        let session = Self { document, state };
        session.inputs_ok(&session.state)?;
        Ok(session)
    }
    /// Validated view description.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }
    /// Scalar display value; substitution is single-pass, never recursive.
    #[must_use]
    pub fn value(&self, key: &str) -> String {
        self.state.get(key).map_or_else(String::new, scalar_text)
    }
    /// Interpolate bounded plain text.
    /// # Errors
    /// Fails when expansion exceeds the text budget or a field is unknown.
    pub fn text(&self, template: &str) -> Result<String> {
        expand(template, &self.state)
    }
    /// Edit only a declared string input. No markup or terminal escapes.
    /// # Errors
    /// Rejects undeclared inputs and oversized/invalid text.
    pub fn input(&mut self, key: &str, value: String) -> Result<()> {
        ensure!(
            self.document
                .view
                .iter()
                .any(|n| matches!(n, Node::Input { key: k, .. } if k == key)),
            "not an input field"
        );
        line_ok(&value, MAX_VALUE)?;
        self.state.insert(key.into(), Value::String(value));
        Ok(())
    }
    /// Execute an action transactionally. At most one explicit message is
    /// returned; the frontend decides when/how to send it as plain user text.
    /// # Errors
    /// Bad arithmetic, excessive work or invalid resulting state rolls back.
    pub fn activate(&mut self, action: &str) -> Result<Option<String>> {
        ensure!(self.document.actions.contains_key(action), "unknown action");
        let mut next = self.state.clone();
        let mut message = None;
        self.run(action, &mut next, &mut message, &mut 1024)?;
        self.inputs_ok(&next)?;
        self.state = next;
        Ok(message)
    }
    fn run(
        &self,
        action: &str,
        state: &mut Map<String, Value>,
        message: &mut Option<String>,
        budget: &mut usize,
    ) -> Result<()> {
        for step in &self.document.actions[action] {
            spend(budget)?;
            match step {
                Step::Set { key, value } => {
                    let value = eval(value, state, budget)?;
                    state.insert(key.clone(), value);
                }
                Step::Call { action } => self.run(action, state, message, budget)?,
                Step::Send { text } => {
                    ensure!(message.is_none(), "an action may send only one message");
                    *message = Some(format!(
                        "UI response — {}\n{}",
                        self.document.title,
                        expand(text, state)?
                    ));
                }
            }
        }
        Ok(())
    }
    fn inputs_ok(&self, state: &Map<String, Value>) -> Result<()> {
        for node in &self.document.view {
            if let Node::Input { key, .. } = node {
                input_ok(&state[key])?;
            }
        }
        Ok(())
    }
}

fn identifier(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 32
            && key
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-')),
        "invalid UI identifier"
    );
    Ok(())
}
fn text_ok(text: &str, max: usize) -> Result<()> {
    ensure!(
        text.len() <= max && !text.chars().any(|ch| ch.is_control() && ch != '\n'),
        "oversized text or terminal controls in UI"
    );
    Ok(())
}
fn line_ok(text: &str, max: usize) -> Result<()> {
    text_ok(text, max)?;
    ensure!(!text.contains('\n'), "expected single-line UI text");
    Ok(())
}
fn input_ok(value: &Value) -> Result<()> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("input state must be a string"))?;
    line_ok(text, MAX_VALUE)
}
fn scalar_ok(value: &Value) -> Result<()> {
    match value {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::String(text) => text_ok(text, MAX_VALUE),
        Value::Number(number) => {
            ensure!(
                number
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.abs() <= 1e12),
                "number exceeds UI budget"
            );
            Ok(())
        }
        _ => bail!("UI state must be scalar"),
    }
}
fn spend(budget: &mut usize) -> Result<()> {
    ensure!(*budget > 0, "UI work budget exceeded");
    *budget -= 1;
    Ok(())
}
fn operator(value: &Value) -> Result<(&str, &Value)> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid expression"))?;
    ensure!(obj.len() == 1, "expressions need one operator");
    obj.iter()
        .next()
        .map(|(k, v)| (k.as_str(), v))
        .ok_or_else(|| anyhow::anyhow!("empty expression"))
}
fn validate_expr(
    value: &Value,
    state: &Map<String, Value>,
    budget: &mut usize,
    depth: usize,
) -> Result<()> {
    spend(budget)?;
    ensure!(depth <= 12, "UI expression is too deep");
    if !value.is_object() {
        return scalar_ok(value);
    }
    let (op, arg) = operator(value)?;
    match op {
        "get" => ensure!(
            arg.as_str().is_some_and(|key| state.contains_key(key)),
            "unknown expression field"
        ),
        "random" => {
            let values = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("random needs a choice array"))?;
            ensure!(
                !values.is_empty() && values.len() <= 32,
                "random needs 1–32 scalar choices"
            );
            for value in values {
                scalar_ok(value)?;
            }
        }
        "not" | "number" => validate_expr(arg, state, budget, depth + 1)?,
        "add" | "sub" | "mul" | "div" | "mod" | "eq" | "lt" | "gt" | "and" | "or" | "if" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("operator needs an argument array"))?;
            ensure!(
                args.len() == if op == "if" { 3 } else { 2 },
                "wrong expression arity"
            );
            for value in args {
                validate_expr(value, state, budget, depth + 1)?;
            }
        }
        _ => bail!("unsupported UI operator: {op}"),
    }
    Ok(())
}
fn eval(value: &Value, state: &Map<String, Value>, budget: &mut usize) -> Result<Value> {
    spend(budget)?;
    if !value.is_object() {
        scalar_ok(value)?;
        return Ok(value.clone());
    }
    let (op, arg) = operator(value)?;
    match op {
        "get" => {
            return state
                .get(arg.as_str().unwrap_or(""))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown state field"));
        }
        "random" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid random choices"))?;
            ensure!(!args.is_empty(), "empty random choices");
            return Ok(args[rand::random_range(0..args.len())].clone());
        }
        "not" => return Ok(json!(!boolean(&eval(arg, state, budget)?)?)),
        "number" => return parse_number(&eval(arg, state, budget)?),
        _ => {}
    }
    let args = arg
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid expression arguments"))?;
    let left = eval(&args[0], state, budget)?;
    if op == "if" {
        return eval(&args[if boolean(&left)? { 1 } else { 2 }], state, budget);
    }
    if op == "and" && !boolean(&left)? {
        return Ok(json!(false));
    }
    if op == "or" && boolean(&left)? {
        return Ok(json!(true));
    }
    let right = eval(&args[1], state, budget)?;
    let result = match op {
        "eq" => json!(if left.is_number() && right.is_number() {
            number(&left)?
                .partial_cmp(&number(&right)?)
                .is_some_and(std::cmp::Ordering::is_eq)
        } else {
            left == right
        }),
        "and" => json!(boolean(&left)? && boolean(&right)?),
        "or" => json!(boolean(&left)? || boolean(&right)?),
        "lt" => json!(number(&left)? < number(&right)?),
        "gt" => json!(number(&left)? > number(&right)?),
        "add" => json!(number(&left)? + number(&right)?),
        "sub" => json!(number(&left)? - number(&right)?),
        "mul" => json!(number(&left)? * number(&right)?),
        "div" | "mod" => {
            let divisor = number(&right)?;
            ensure!(
                divisor.classify() != std::num::FpCategory::Zero,
                "division by zero"
            );
            json!(if op == "div" {
                number(&left)? / divisor
            } else {
                number(&left)? % divisor
            })
        }
        _ => bail!("unsupported operator"),
    };
    ensure!(!result.is_null(), "non-finite UI arithmetic");
    scalar_ok(&result)?;
    Ok(result)
}
fn parse_number(value: &Value) -> Result<Value> {
    let parsed = if let Some(text) = value.as_str() {
        text.trim().parse::<f64>()?
    } else {
        number(value)?
    };
    let result = json!(parsed);
    ensure!(!result.is_null(), "expected a finite number");
    scalar_ok(&result)?;
    Ok(result)
}
fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("expected number"))
}
fn boolean(value: &Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("expected boolean"))
}
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string().trim_end_matches(".0").to_string(),
    }
}
fn expand(template: &str, state: &Map<String, Value>) -> Result<String> {
    let mut result = String::new();
    let mut rest = template;
    while let Some((head, tail)) = rest.split_once("{{") {
        let (key, after) = tail
            .split_once("}}")
            .ok_or_else(|| anyhow::anyhow!("unclosed template"))?;
        let value = state
            .get(key.trim())
            .ok_or_else(|| anyhow::anyhow!("unknown template field"))?;
        result.push_str(head);
        result.push_str(&scalar_text(value));
        ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
        rest = after;
    }
    result.push_str(rest);
    ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
    Ok(result)
}

#[cfg(test)]
mod tests;
