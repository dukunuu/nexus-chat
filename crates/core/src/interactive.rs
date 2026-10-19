//! Bounded, model-authored interactive UI documents. This is data, not code:
//! scalar state, a small expression language, controls and transactional actions.
//! No filesystem, network, HTML, processes, timers or provider access exist here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Model instructions advertised only by clients with an interactive renderer.
pub const INSTRUCTIONS: &str = include_str!("../assets/prompts/interactive-ui.md");

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

/// One labelled numeric expression in a bar chart.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarItem {
    /// Plain-text label template.
    pub label: String,
    /// Numeric expression.
    pub value: Value,
}

/// One locally toggleable boolean in a checklist.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckItem {
    /// Plain-text label template.
    pub label: String,
    /// Bound boolean state field.
    pub key: String,
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
    /// Aligned label/value template pairs.
    Kv {
        /// One to eight pairs.
        items: Vec<[String; 2]>,
    },
    /// A fixed table with optional local row selection.
    Table {
        /// One to six header templates.
        columns: Vec<String>,
        /// Up to eight rows of cell templates.
        rows_data: Vec<Vec<String>>,
        /// Optional numeric state field holding the selected index.
        #[serde(default)]
        select: Option<String>,
    },
    /// A clamped numeric bar with a label.
    Progress {
        /// Plain-text label template.
        label: String,
        /// Numeric expression.
        value: Value,
        /// Numeric maximum; defaults to one.
        #[serde(default = "unit")]
        max: Value,
    },
    /// Horizontal bars scaled to their largest nonnegative value.
    Bars {
        /// One to eight labelled numeric expressions.
        items: Vec<BarItem>,
    },
    /// A fixed-height series of numeric expressions.
    Sparkline {
        /// One to sixty-four samples.
        values: Vec<Value>,
        /// Declared height, one to three rows.
        #[serde(default = "one")]
        rows: u16,
    },
    /// Independently toggleable boolean fields.
    Checklist {
        /// One to eight boolean controls.
        items: Vec<CheckItem>,
    },
    /// A string choice, cycled locally without actions.
    Select {
        /// Bound string state field.
        key: String,
        /// Plain-text label template.
        label: String,
        /// Two to eight allowed string values.
        options: Vec<String>,
    },
    /// A locally toggleable boolean field.
    Toggle {
        /// Bound boolean state field.
        key: String,
        /// Plain-text label template.
        label: String,
    },
}
fn unit() -> Value {
    json!(1)
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
    pub fn rows(&self) -> u16 {
        match self {
            Self::Text { rows, .. } | Self::Buttons { rows, .. } | Self::Sparkline { rows, .. } => {
                *rows
            }
            Self::Input { .. } => 3,
            Self::Kv { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Table { rows_data, .. } => {
                u16::try_from(rows_data.len().saturating_add(1)).unwrap_or(u16::MAX)
            }
            Self::Bars { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Checklist { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Progress { .. } | Self::Select { .. } | Self::Toggle { .. } => 1,
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
        let mut budget = 1024;
        for node in &self.view {
            let max_rows = if matches!(node, Node::Table { .. }) {
                9
            } else {
                8
            };
            ensure!(
                (1..=max_rows).contains(&node.rows()),
                "node height exceeds its limit"
            );
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
                _ => controls += self.validate_component(node, &mut budget)?,
            }
        }
        ensure!(
            rows <= ROWS - 3 && controls <= 32,
            "UI exceeds its fixed display budget"
        );
        Ok(())
    }

    fn validate_component(&self, node: &Node, budget: &mut usize) -> Result<usize> {
        match node {
            Node::Kv { items } => {
                item_count(items.len(), 1, 8)?;
                for pair in items {
                    for text in pair {
                        self.template_ok(text)?;
                    }
                }
            }
            Node::Table {
                columns,
                rows_data,
                select,
            } => {
                item_count(columns.len(), 1, 6)?;
                item_count(rows_data.len(), 0, 8)?;
                for header in columns {
                    self.template_ok(header)?;
                }
                for row in rows_data {
                    ensure!(row.len() == columns.len(), "table row width mismatch");
                    for cell in row {
                        self.template_ok(cell)?;
                    }
                }
                if let Some(key) = select {
                    binding_ok(&self.state, key, Value::is_number)?;
                    return Ok(1);
                }
            }
            Node::Progress { label, value, max } => {
                self.template_ok(label)?;
                self.numeric_ok(value, budget)?;
                self.numeric_ok(max, budget)?;
            }
            Node::Bars { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    self.template_ok(&item.label)?;
                    self.numeric_ok(&item.value, budget)?;
                }
            }
            Node::Sparkline { values, rows } => {
                item_count(values.len(), 1, 64)?;
                ensure!((1..=3).contains(rows), "sparkline rows must be 1–3");
                for value in values {
                    self.numeric_ok(value, budget)?;
                }
            }
            Node::Checklist { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    self.template_ok(&item.label)?;
                    binding_ok(&self.state, &item.key, Value::is_boolean)?;
                }
                return Ok(items.len());
            }
            Node::Select {
                key,
                label,
                options,
            } => {
                self.template_ok(label)?;
                item_count(options.len(), 2, 8)?;
                for option in options {
                    line_ok(option, 64)?;
                    self.template_ok(option)?;
                }
                binding_ok(&self.state, key, |value| {
                    value
                        .as_str()
                        .is_some_and(|text| options.iter().any(|s| s == text))
                })?;
                return Ok(1);
            }
            Node::Toggle { key, label } => {
                self.template_ok(label)?;
                binding_ok(&self.state, key, Value::is_boolean)?;
                return Ok(1);
            }
            Node::Text { .. } | Node::Buttons { .. } | Node::Input { .. } => {}
        }
        Ok(0)
    }

    fn numeric_ok(&self, value: &Value, budget: &mut usize) -> Result<()> {
        validate_expr(value, &self.state, budget, 0)?;
        ensure!(
            numeric_expr(value, &self.state),
            "expected numeric expression"
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
        let mut session = Self { document, state };
        let mut state = session.state.clone();
        session.bindings_ok(&mut state)?;
        session.state = state;
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
        let mut next = self.state.clone();
        next.insert(key.into(), Value::String(value));
        self.bindings_ok(&mut next)?;
        self.state = next;
        Ok(())
    }
    /// Evaluate a numeric presentation expression against current state.
    /// # Errors
    /// Rejects invalid arithmetic or nonnumeric results.
    pub fn number(&self, expression: &Value) -> Result<f64> {
        number(&eval(expression, &self.state, &mut 1024)?)
    }
    /// Evaluate progress, clamped to a nonnegative maximum.
    /// # Errors
    /// Rejects invalid numeric expressions.
    pub fn progress(&self, value: &Value, max: &Value) -> Result<(f64, f64)> {
        let max = self.number(max)?.max(0.0);
        Ok((self.number(value)?.clamp(0.0, max), max))
    }
    /// Flip a declared checklist or toggle field, without running an action.
    /// # Errors
    /// Rejects undeclared controls or invalid resulting bindings.
    pub fn toggle(&mut self, key: &str) -> Result<()> {
        ensure!(
            self.document.view.iter().any(|node| match node {
                Node::Toggle { key: k, .. } => k == key,
                Node::Checklist { items } => items.iter().any(|item| item.key == key),
                _ => false,
            }),
            "not a boolean control"
        );
        let mut next = self.state.clone();
        next.insert(key.into(), json!(!boolean(&self.state[key])?));
        self.bindings_ok(&mut next)?;
        self.state = next;
        Ok(())
    }
    /// Cycle one select node, wrapping within its fixed options.
    /// # Errors
    /// Rejects non-select nodes or invalid resulting bindings.
    pub fn cycle(&mut self, node: usize, forward: bool) -> Result<()> {
        let Some(Node::Select { key, options, .. }) = self.document.view.get(node) else {
            bail!("not a select control");
        };
        let index = options
            .iter()
            .position(|s| Some(s.as_str()) == self.state[key].as_str())
            .ok_or_else(|| anyhow::anyhow!("invalid select value"))?;
        let index = (1..options.len())
            .map(|offset| {
                (index
                    + if forward {
                        offset
                    } else {
                        options.len() - offset
                    })
                    % options.len()
            })
            .find(|next| options[*next] != options[index])
            .unwrap_or(index);
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(options[index]));
        self.bindings_ok(&mut next)?;
        self.state = next;
        Ok(())
    }
    /// Current normalized table index (zero for an empty table).
    #[must_use]
    pub fn selected_row(&self, key: &str) -> usize {
        self.state
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|row| usize::try_from(row).ok())
            .unwrap_or(0)
    }
    /// Select a table row locally, clamping to the table's data bounds.
    /// # Errors
    /// Rejects nodes without row selection or invalid resulting bindings.
    pub fn select_row(&mut self, node: usize, row: usize) -> Result<()> {
        let Some(Node::Table {
            select: Some(key),
            rows_data,
            ..
        }) = self.document.view.get(node)
        else {
            bail!("not a selectable table");
        };
        let mut next = self.state.clone();
        next.insert(
            key.clone(),
            json!(row.min(rows_data.len().saturating_sub(1))),
        );
        self.bindings_ok(&mut next)?;
        self.state = next;
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
        self.bindings_ok(&mut next)?;
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
    fn bindings_ok(&self, state: &mut Map<String, Value>) -> Result<()> {
        for node in &self.document.view {
            match node {
                Node::Input { key, .. } => input_ok(&state[key])?,
                Node::Toggle { key, .. } => {
                    boolean(&state[key])?;
                }
                Node::Checklist { items } => {
                    for item in items {
                        boolean(&state[&item.key])?;
                    }
                }
                Node::Select { key, options, .. } => {
                    ensure!(
                        state[key]
                            .as_str()
                            .is_some_and(|s| options.iter().any(|o| o == s)),
                        "select value must be an option"
                    );
                }
                Node::Table {
                    select: Some(key),
                    rows_data,
                    ..
                } => {
                    let max = u32::try_from(rows_data.len().saturating_sub(1))?;
                    let value = number(&state[key])?.clamp(0.0, f64::from(max));
                    let row = (0..=max)
                        .rev()
                        .find(|row| f64::from(*row) <= value)
                        .unwrap_or(0);
                    state.insert(key.clone(), json!(row));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn item_count(count: usize, min: usize, max: usize) -> Result<()> {
    ensure!(
        (min..=max).contains(&count),
        "component needs {min}–{max} items"
    );
    Ok(())
}
fn binding_ok(state: &Map<String, Value>, key: &str, valid: impl Fn(&Value) -> bool) -> Result<()> {
    let value = state
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("unknown bound field: {key}"))?;
    if let Some(values) = value.get("random").and_then(Value::as_array) {
        ensure!(
            values.iter().all(valid),
            "invalid bound field type or value: {key}"
        );
    } else {
        ensure!(valid(value), "invalid bound field type or value: {key}");
    }
    Ok(())
}
fn numeric_expr(value: &Value, state: &Map<String, Value>) -> bool {
    if !value.is_object() {
        return value.is_number();
    }
    let Ok((op, arg)) = operator(value) else {
        return false;
    };
    match op {
        "get" => arg
            .as_str()
            .is_some_and(|key| binding_ok(state, key, Value::is_number).is_ok()),
        "random" => arg
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_number)),
        "if" => arg
            .as_array()
            .is_some_and(|args| args[1..].iter().all(|v| numeric_expr(v, state))),
        "add" | "sub" | "mul" | "div" | "mod" => arg
            .as_array()
            .is_some_and(|args| args.iter().all(|v| numeric_expr(v, state))),
        "number" => true,
        _ => false,
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

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;
