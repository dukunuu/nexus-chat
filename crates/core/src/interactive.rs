//! Bounded, model-authored interactive UI documents. This is data, not code:
//! scalar state, a small expression language, controls and transactional actions.
//! No filesystem, network, HTML, processes, timers or provider access exist here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Model instructions advertised only by clients with an interactive renderer.
pub const INSTRUCTIONS: &str = include_str!("../assets/prompts/interactive-ui.md");
/// Display-only layout guidance, advertised according to reply style.
pub const LAYOUTS: &str = include_str!("../assets/prompts/layouts.md");

/// Maximum serialized UI document size.
pub const MAX_SOURCE: usize = 16 * 1024;
/// Validation row budget, including title and interaction hint.
pub const ROWS: u16 = 24;
const LAYOUT_ROWS: u16 = 48;
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

/// Optional document border style.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    /// Rounded corners (the legacy default).
    #[default]
    Rounded,
    /// Double-line border.
    Double,
    /// Heavy border.
    Heavy,
    /// Dashed border.
    Dashed,
    /// Title rule without an enclosing border.
    Plain,
}

/// Theme role for document highlights; never an arbitrary color.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    /// Primary theme accent.
    Accent,
    /// Assistant accent (the default).
    #[default]
    Accent2,
    /// Positive emphasis.
    Success,
    /// Caution.
    Warning,
    /// Negative emphasis.
    Error,
    /// Subdued emphasis.
    Muted,
}

/// Semantic marker on a comparison item.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    /// Positive item.
    Ok,
    /// Negative item.
    No,
    /// Neutral bullet.
    Dot,
    /// No marker.
    #[default]
    None,
}

/// A comparison item, with plain-text template content.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnItem {
    /// Item text.
    pub text: String,
    /// Optional semantic marker.
    #[serde(default)]
    pub mark: Mark,
}

/// One comparison column.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// Column title template.
    pub title: String,
    /// Up to eight items.
    pub items: Vec<ColumnItem>,
}

/// A bordered display card.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    /// Title template.
    pub title: String,
    /// Body template.
    pub text: String,
    /// Optional badge template.
    pub badge: Option<String>,
    /// Title emphasis; absent uses the document accent.
    pub tone: Option<Tone>,
}

/// A labelled metric.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stat {
    /// Value template.
    pub value: String,
    /// Dim label template.
    pub label: String,
    /// Signed delta template, for example "+12%".
    pub delta: Option<String>,
    /// Value emphasis; absent uses the document accent.
    pub tone: Option<Tone>,
}

/// Progress along a display-only timeline.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Completed step.
    Done,
    /// Active step.
    Current,
    /// Future step.
    #[default]
    Todo,
}

/// One display-only timeline step (not an executable action).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayStep {
    /// Title template.
    pub title: String,
    /// Optional description template.
    pub text: Option<String>,
    /// Completion state.
    #[serde(default)]
    pub state: StepState,
}

/// Semantic color for a callout rail.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalloutTone {
    /// Informational theme accent.
    Info,
    /// Tip in the document accent.
    Tip,
    /// Warning theme role.
    Warning,
    /// Error theme role.
    Danger,
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
    /// Prefer this item as the document's primary action.
    #[serde(default)]
    pub primary: bool,
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

/// Declarative presentation nodes. Declared rows count toward validation caps.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// A prominent title, optional dim subtitle and badge.
    Hero {
        /// Title template.
        title: String,
        /// Subtitle template.
        subtitle: Option<String>,
        /// Badge template.
        badge: Option<String>,
    },
    /// Two or three comparison columns, stacked on narrow terminals.
    Columns {
        /// Columns with up to eight items each.
        items: Vec<Column>,
    },
    /// One to six bordered sub-cards in a responsive grid.
    Cards {
        /// Display cards.
        items: Vec<Card>,
    },
    /// One to six labelled metrics.
    Stats {
        /// Display metrics.
        items: Vec<Stat>,
    },
    /// One to eight steps in a row or connected timeline.
    Steps {
        /// Display steps, not executable actions.
        items: Vec<DisplayStep>,
    },
    /// A semantic rail alongside short text.
    Callout {
        /// Rail emphasis.
        tone: CalloutTone,
        /// Optional title template.
        title: Option<String>,
        /// Body template.
        text: String,
    },
    /// A quotation with optional attribution.
    Quote {
        /// Quoted text template.
        text: String,
        /// Attribution template.
        by: Option<String>,
    },
    /// A separator rule, optionally labelled.
    Divider {
        /// Label template.
        label: Option<String>,
    },
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
        #[serde(alias = "rows")]
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
    fn is_display(&self) -> bool {
        matches!(
            self,
            Self::Hero { .. }
                | Self::Columns { .. }
                | Self::Cards { .. }
                | Self::Stats { .. }
                | Self::Steps { .. }
                | Self::Callout { .. }
                | Self::Quote { .. }
                | Self::Divider { .. }
        )
    }

    /// Declared validation budget, independent of frontend layout.
    #[must_use]
    pub fn rows(&self) -> u16 {
        match self {
            Self::Columns { items } => items
                .iter()
                .map(|item| bounded_rows(item.items.len() + 1))
                .sum(),
            Self::Cards { items } => bounded_rows(items.len().saturating_mul(4)),
            Self::Steps { items } => bounded_rows(items.len().saturating_mul(2)),
            Self::Callout { .. } | Self::Quote { .. } => 2,
            Self::Text { rows, .. } | Self::Buttons { rows, .. } | Self::Sparkline { rows, .. } => {
                *rows
            }
            Self::Hero { .. } | Self::Stats { .. } | Self::Input { .. } => 3,
            Self::Kv { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Table { rows_data, .. } => {
                u16::try_from(rows_data.len().saturating_add(1)).unwrap_or(u16::MAX)
            }
            Self::Bars { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Checklist { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Divider { .. }
            | Self::Progress { .. }
            | Self::Select { .. }
            | Self::Toggle { .. } => 1,
        }
    }
}

fn bounded_rows(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Set {
        key: String,
        value: Value,
    },
    Call {
        action: String,
    },
    Send {
        text: String,
    },
    /// Sends the clicked button's label. Models often add the label here
    /// too; it is accepted and ignored — the button's label is what's sent.
    Answer {
        #[serde(default)]
        #[allow(dead_code)]
        label: Option<Value>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    version: u8,
    title: String,
    #[serde(default)]
    frame: Frame,
    #[serde(default)]
    accent: Accent,
    #[serde(default)]
    state: Map<String, Value>,
    view: Vec<Node>,
    #[serde(default)]
    actions: BTreeMap<String, Vec<Step>>,
}

/// A located problem in a model-authored UI document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckError {
    /// JSON location, or `$` for the entire source.
    pub path: String,
    /// Explanation including valid alternatives for unknown constructs.
    pub message: String,
}

/// Valid presentation node types, shared by validation and tool reports.
pub const NODE_TYPES: &str = "hero, columns, cards, stats, steps, callout, quote, divider, text, buttons, input, kv, table, progress, bars, sparkline, checklist, select, toggle";
const ACTION_OPS: &str = "set, call, send, answer";
const EXPR_OPS: &str = "get, random, not, number, if, add, sub, mul, div, mod, eq, lt, gt, and, or";
const MAX_ERRORS: usize = 10;

fn check_result(errors: &mut Vec<CheckError>, path: &str, result: Result<()>) {
    if let Err(error) = result
        && errors.len() < MAX_ERRORS
    {
        errors.push(CheckError {
            path: path.into(),
            message: error.to_string(),
        });
    }
}

// Check fields independently so serde's first failure doesn't hide later ones.
// Optional fields use `?`; their defaults/requiredness remain owned by serde.
#[allow(clippy::too_many_lines)]
fn check_shape(value: &Value, path: &str, shape: &str, errors: &mut Vec<CheckError>) {
    if errors.len() >= MAX_ERRORS {
        return;
    }
    if let Some(inner) = shape.strip_prefix('?') {
        if !value.is_null() {
            check_shape(value, path, inner, errors);
        }
        return;
    }
    if let Some(inner) = shape.strip_suffix("[]") {
        if let Some(items) = value.as_array() {
            for (index, item) in items.iter().enumerate() {
                check_shape(item, &format!("{path}[{index}]"), inner, errors);
            }
        } else {
            check_result(
                errors,
                path,
                Err(anyhow::anyhow!("expected an array of {inner}")),
            );
        }
        return;
    }
    macro_rules! typed {
        ($ty:ty) => {{
            check_result(
                errors,
                path,
                serde_json::from_value::<$ty>(value.clone())
                    .map(|_| ())
                    .map_err(Into::into),
            );
            return;
        }};
    }
    match shape {
        "string" => typed!(String),
        "bool" => typed!(bool),
        "u8" => typed!(u8),
        "u16" => typed!(u16),
        "tone" => typed!(Tone),
        "frame" => typed!(Frame),
        "accent" => typed!(Accent),
        "mark" => typed!(Mark),
        "step_state" => typed!(StepState),
        "callout_tone" => typed!(CalloutTone),
        "pair" => typed!([String; 2]),
        "value" => return,
        _ => {}
    }
    let Some(object) = value.as_object() else {
        check_result(errors, path, Err(anyhow::anyhow!("expected an object")));
        return;
    };
    let (owner, fields) = match shape {
        "document" => (
            shape,
            "version:u8 title:string frame:frame accent:accent state:state view:node[] actions:actions",
        ),
        "state" | "actions" => {
            for (key, value) in object {
                check_shape(
                    value,
                    &format!("{path}.{key}"),
                    if shape == "state" { "value" } else { "step[]" },
                    errors,
                );
            }
            return;
        }
        "node" | "step" => {
            let (tag, valid, kind) = if shape == "node" {
                ("type", NODE_TYPES, "node type")
            } else {
                ("op", ACTION_OPS, "action op")
            };
            let Some(name) = object.get(tag).and_then(Value::as_str) else {
                check_result(
                    errors,
                    &format!("{path}.{tag}"),
                    Err(anyhow::anyhow!("expected {tag} string — valid: {valid}")),
                );
                return;
            };
            let fields = match (shape, name) {
                ("node", "hero") => "title:string subtitle:?string badge:?string",
                ("node", "columns") => "items:column[]",
                ("node", "cards") => "items:card[]",
                ("node", "stats") => "items:stat[]",
                ("node", "steps") => "items:display_step[]",
                ("node", "callout") => "tone:callout_tone title:?string text:string",
                ("node", "quote") => "text:string by:?string",
                ("node", "divider") => "label:?string",
                ("node", "text") => "text:string tone:tone rows:u16",
                ("node", "buttons") => "items:button[] rows:u16",
                ("node", "input" | "toggle") => "key:string label:string",
                ("node", "kv") => "items:pair[]",
                ("node", "table") => {
                    "columns:string[] rows_data:string[][] rows:string[][] select:?string"
                }
                ("node", "progress") => "label:string value:value max:value",
                ("node", "bars") => "items:bar[]",
                ("node", "sparkline") => "values:value[] rows:u16",
                ("node", "checklist") => "items:check[]",
                ("node", "select") => "key:string label:string options:string[]",
                ("step", "set") => "key:string value:value",
                ("step", "call") => "action:string",
                ("step", "send") => "text:string",
                ("step", "answer") => "",
                _ => {
                    check_result(
                        errors,
                        path,
                        Err(anyhow::anyhow!("unknown {kind} {name:?} — valid: {valid}")),
                    );
                    return;
                }
            };
            (name, fields)
        }
        "button" => (
            shape,
            "label:string action:string body:string[] primary:bool",
        ),
        "column" => (shape, "title:string items:column_item[]"),
        "column_item" => (shape, "text:string mark:mark"),
        "card" => (shape, "title:string text:string badge:?string tone:?tone"),
        "stat" => (shape, "value:string label:string delta:?string tone:?tone"),
        "display_step" => (shape, "title:string text:?string state:step_state"),
        "bar" => (shape, "label:string value:value"),
        "check" => (shape, "label:string key:string"),
        _ => unreachable!("known schema shape"),
    };
    let fields: Vec<_> = fields
        .split_whitespace()
        .filter_map(|field| field.split_once(':'))
        .collect();
    for (key, value) in object {
        if (shape == "node" && key == "type") || (shape == "step" && key == "op") {
            continue;
        }
        if let Some((_, field_shape)) = fields.iter().find(|(name, _)| name == key) {
            check_shape(value, &format!("{path}.{key}"), field_shape, errors);
        } else {
            let alternatives = if fields.is_empty() {
                format!("{owner} takes no fields")
            } else {
                format!(
                    "valid: {}",
                    fields
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            check_result(
                errors,
                path,
                Err(anyhow::anyhow!(
                    "unknown field {key:?} on {owner} — {alternatives}"
                )),
            );
        }
    }
    let required = match owner {
        "document" => "version title view",
        "hero" | "column" => {
            if owner == "hero" {
                "title"
            } else {
                "title items"
            }
        }
        "columns" | "cards" | "stats" | "steps" | "buttons" | "kv" | "bars" | "checklist" => {
            "items"
        }
        "callout" => "tone text",
        "quote" | "text" | "column_item" | "send" => "text",
        "input" | "toggle" | "check" => "key label",
        "table" => "columns rows_data",
        "progress" | "bar" => "label value",
        "sparkline" => "values",
        "select" => "key label options",
        "set" => "key value",
        "call" => "action",
        "button" => "label action",
        "card" => "title text",
        "stat" => "value label",
        "display_step" => "title",
        _ => "",
    };
    for field in required.split_whitespace() {
        if !(object.contains_key(field)
            || owner == "table" && field == "rows_data" && object.contains_key("rows"))
        {
            check_result(
                errors,
                &format!("{path}.{field}"),
                Err(anyhow::anyhow!("missing required field {field:?}")),
            );
        }
    }
    // Deserialize every node/step independently, preserving serde's schema as
    // the authority if a future field is added without a diagnostic shape.
    let result = match shape {
        "document" => serde_json::from_value::<Definition>(value.clone()).map(|_| ()),
        "node" => serde_json::from_value::<Node>(value.clone()).map(|_| ()),
        "step" => serde_json::from_value::<Step>(value.clone()).map(|_| ()),
        "button" => serde_json::from_value::<Button>(value.clone()).map(|_| ()),
        "column" => serde_json::from_value::<Column>(value.clone()).map(|_| ()),
        "column_item" => serde_json::from_value::<ColumnItem>(value.clone()).map(|_| ()),
        "card" => serde_json::from_value::<Card>(value.clone()).map(|_| ()),
        "stat" => serde_json::from_value::<Stat>(value.clone()).map(|_| ()),
        "display_step" => serde_json::from_value::<DisplayStep>(value.clone()).map(|_| ()),
        "bar" => serde_json::from_value::<BarItem>(value.clone()).map(|_| ()),
        "check" => serde_json::from_value::<CheckItem>(value.clone()).map(|_| ()),
        _ => return,
    };
    if let Err(error) = result {
        let message = error.to_string();
        if !errors.iter().any(|error| error.path.starts_with(path)) {
            let location = message
                .strip_prefix("missing field `")
                .and_then(|s| s.strip_suffix('`'))
                .map_or_else(|| path.to_string(), |field| format!("{path}.{field}"));
            check_result(errors, &location, Err(error.into()));
        }
    }
}

/// A validated version-1 UI document. Construction is through `parse` or `check`.
#[derive(Clone, Debug)]
pub struct Document {
    version: u8,
    title: String,
    frame: Frame,
    accent: Accent,
    state: Map<String, Value>,
    view: Vec<Node>,
    actions: BTreeMap<String, Vec<Step>>,
    sending: BTreeSet<String>,
    answering: BTreeSet<String>,
}

impl Document {
    /// Parse and validate all controls, expressions and action paths.
    /// # Errors
    /// Rejects unknown constructs, oversized data, cycles and invalid bindings.
    pub fn parse(source: &str) -> Result<Self> {
        Self::check(source).map_err(|errors| {
            anyhow::anyhow!(
                errors
                    .iter()
                    .map(|error| format!("{}: {}", error.path, error.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
    }

    /// Check authored JSON, reporting up to ten located errors without executing actions.
    /// # Errors
    /// Returns syntax, schema, binding or budget errors.
    pub fn check(source: &str) -> std::result::Result<Self, Vec<CheckError>> {
        let mut errors = Vec::new();
        check_result(
            &mut errors,
            "$",
            (|| {
                ensure!(source.len() <= MAX_SOURCE, "UI document exceeds 16 KiB");
                Ok(())
            })(),
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut stream = serde_json::Deserializer::from_str(source).into_iter::<Value>();
        let mut value = match stream
            .next()
            .unwrap_or_else(|| serde_json::from_str(source))
        {
            Ok(value) => value,
            Err(error) => {
                return Err(vec![CheckError {
                    path: "$".into(),
                    message: error.to_string(),
                }]);
            }
        };
        if !source[stream.byte_offset()..]
            .chars()
            .all(|ch| ch.is_whitespace() || matches!(ch, '}' | ']'))
        {
            return Err(vec![CheckError {
                path: "$".into(),
                message: "trailing characters after UI document".into(),
            }]);
        }
        if let Some(actions) = value.get_mut("actions").and_then(Value::as_object_mut) {
            for steps in actions.values_mut().filter_map(Value::as_array_mut) {
                for step in steps {
                    if step.get("op").and_then(Value::as_str) == Some("answer")
                        && let Some(object) = step.as_object_mut()
                    {
                        object.remove("label");
                    }
                }
            }
        }
        check_shape(&value, "$", "document", &mut errors);
        // Top-level fields use their natural JSON paths, without a `$` prefix.
        for error in &mut errors {
            if let Some(path) = error.path.strip_prefix("$.") {
                error.path = path.into();
            }
        }
        // Keep valid elements available for semantic checks even when another
        // element has a schema error. Neutral placeholders preserve JSON indices;
        // they can never escape because the collected errors reject the document.
        let definition =
            serde_json::from_value::<Definition>(value.clone()).unwrap_or_else(|_| Definition {
                version: serde_json::from_value(value["version"].clone()).unwrap_or(1),
                title: serde_json::from_value(value["title"].clone())
                    .unwrap_or_else(|_| "UI".into()),
                frame: serde_json::from_value(value["frame"].clone()).unwrap_or_default(),
                accent: serde_json::from_value(value["accent"].clone()).unwrap_or_default(),
                state: serde_json::from_value(value["state"].clone()).unwrap_or_default(),
                view: value["view"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|node| {
                        serde_json::from_value(node.clone())
                            .unwrap_or(Node::Divider { label: None })
                    })
                    .collect(),
                actions: value["actions"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, steps)| {
                        (
                            name.clone(),
                            steps
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|step| {
                                    serde_json::from_value(step.clone())
                                        .unwrap_or(Step::Answer { label: None })
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            });
        Self::check_definition(definition, errors)
    }

    #[allow(clippy::too_many_lines)]
    fn check_definition(
        definition: Definition,
        mut errors: Vec<CheckError>,
    ) -> std::result::Result<Self, Vec<CheckError>> {
        let Definition {
            version,
            title,
            frame,
            accent,
            state,
            view,
            actions,
        } = definition;
        let mut doc = Self {
            version,
            title,
            frame,
            accent,
            state,
            view,
            actions,
            sending: BTreeSet::new(),
            answering: BTreeSet::new(),
        };
        check_result(
            &mut errors,
            "version",
            (|| {
                ensure!(doc.version == 1, "unsupported UI version — valid: 1");
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "title",
            (|| {
                line_ok(&doc.title, 120)?;
                ensure!(!doc.title.is_empty(), "UI title is required");
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "state",
            (|| {
                ensure!(
                    doc.state.len() <= 32,
                    "too many UI state fields — maximum: 32"
                );
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "actions",
            (|| {
                ensure!(doc.actions.len() <= 32, "too many UI actions — maximum: 32");
                Ok(())
            })(),
        );
        let mut budget = 1024;
        for (key, value) in &doc.state {
            check_result(
                &mut errors,
                &format!("state.{key}"),
                (|| {
                    identifier(key)?;
                    ensure!(
                        !value.is_object() || value.get("random").is_some(),
                        "initial state must be scalar or a random choice"
                    );
                    validate_expr(value, &doc.state, &mut budget, 0)
                })(),
            );
        }
        doc.check_view(&mut errors);
        let mut steps = 0;
        for (name, action) in &doc.actions {
            check_result(&mut errors, &format!("actions.{name}"), identifier(name));
            steps += action
                .iter()
                .filter(|step| !matches!(step, Step::Answer { .. }))
                .count()
                + usize::from(
                    action
                        .iter()
                        .any(|step| matches!(step, Step::Answer { .. })),
                );
            for (index, step) in action.iter().enumerate() {
                check_result(
                    &mut errors,
                    &format!("actions.{name}[{index}]"),
                    (|| {
                        match step {
                            Step::Set { key, value } => {
                                ensure!(
                                    doc.state.contains_key(key),
                                    "unknown state field: {key} — valid: {}",
                                    doc.state.keys().cloned().collect::<Vec<_>>().join(", ")
                                );
                                validate_expr(value, &doc.state, &mut budget, 0)?;
                            }
                            Step::Call { action } => {
                                ensure!(
                                    doc.actions.contains_key(action),
                                    "unknown action: {action} — valid: {}",
                                    doc.actions.keys().cloned().collect::<Vec<_>>().join(", ")
                                );
                            }
                            Step::Send { text } => Self::template_ok(text)?,
                            Step::Answer { .. } => {}
                        }
                        Ok(())
                    })(),
                );
            }
        }
        check_result(
            &mut errors,
            "actions",
            (|| {
                ensure!(steps <= 128, "too many action steps — maximum: 128");
                Ok(())
            })(),
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        // A repeated `answer` in one action is a model slip, not a second
        // message: keep the first, after checking the original JSON indices.
        for steps in doc.actions.values_mut() {
            let mut answered = false;
            steps.retain(|step| {
                let answer = matches!(step, Step::Answer { .. });
                let keep = !(answer && answered);
                answered |= answer;
                keep
            });
        }
        for name in doc.actions.keys() {
            let (sends, answers) =
                match doc.action_sends(name, &mut BTreeSet::new(), 0, &mut budget) {
                    Ok(flags) => flags,
                    Err(error) => {
                        check_result(&mut errors, &format!("actions.{name}"), Err(error));
                        continue;
                    }
                };
            if sends {
                doc.sending.insert(name.clone());
            }
            if answers {
                doc.answering.insert(name.clone());
            }
        }
        if errors.is_empty() {
            Ok(doc)
        } else {
            Err(errors)
        }
    }

    fn check_view(&self, errors: &mut Vec<CheckError>) {
        if !errors.iter().any(|error| {
            error.path == "view"
                || error.path.starts_with("view[")
                || error.path.starts_with("view.")
        }) {
            check_result(errors, "view", self.validate_view_budget());
        }
        let mut budget = 1024;
        for (index, node) in self.view.iter().enumerate() {
            check_result(
                errors,
                &format!("view[{index}]"),
                self.validate_node(node, &mut budget).map(|_| ()),
            );
        }
    }

    fn validate_view_budget(&self) -> Result<()> {
        ensure!(
            !self.view.is_empty() && self.view.len() <= 16,
            "UI needs 1–16 view nodes"
        );
        let rows: u32 = self.view.iter().map(|node| u32::from(node.rows())).sum();
        let controls = self.control_count();
        let cap = if self.view.iter().any(Node::is_display) {
            LAYOUT_ROWS
        } else {
            ROWS
        };
        ensure!(
            rows <= u32::from(cap - 3) && controls <= 32,
            "UI exceeds its fixed display budget"
        );
        Ok(())
    }

    fn validate_node(&self, node: &Node, budget: &mut usize) -> Result<usize> {
        let max_rows = match node {
            Node::Columns { .. } => 27,
            Node::Cards { .. } => 24,
            Node::Steps { .. } => 16,
            Node::Table { .. } => 9,
            _ => 8,
        };
        ensure!(
            (1..=max_rows).contains(&node.rows()),
            "node height exceeds its limit"
        );
        let mut controls = 0;
        match node {
            Node::Text { text, .. } => Self::template_ok(text)?,
            Node::Buttons { items, .. } => {
                ensure!(
                    !items.is_empty() && items.len() <= 8,
                    "a button row needs 1–8 buttons"
                );
                for button in items {
                    line_ok(&button.label, 120)?;
                    Self::template_ok(&button.label)?;
                    ensure!(
                        self.actions.contains_key(&button.action),
                        "unknown button action: {} — valid: {}",
                        button.action,
                        self.actions.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                    ensure!(button.body.len() <= 8, "too many button artwork lines");
                    for line in &button.body {
                        line_ok(line, 120)?;
                    }
                }
                controls += items.len();
            }
            Node::Input { key, label } => {
                ensure!(
                    self.state.contains_key(key),
                    "unknown input field: {key} — valid: {}",
                    self.state.keys().cloned().collect::<Vec<_>>().join(", ")
                );
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
            _ => controls += self.validate_component(node, budget)?,
        }
        Ok(controls)
    }

    fn validate_display(node: &Node) -> Result<()> {
        match node {
            Node::Hero {
                title,
                subtitle,
                badge,
            } => {
                Self::template_ok(title)?;
                for text in subtitle.iter().chain(badge) {
                    Self::template_ok(text)?;
                }
            }
            Node::Columns { items } => {
                item_count(items.len(), 2, 3)?;
                for column in items {
                    Self::template_ok(&column.title)?;
                    item_count(column.items.len(), 0, 8)?;
                    for item in &column.items {
                        Self::template_ok(&item.text)?;
                    }
                }
            }
            Node::Cards { items } => {
                item_count(items.len(), 1, 6)?;
                for item in items {
                    Self::template_ok(&item.title)?;
                    Self::template_ok(&item.text)?;
                    if let Some(badge) = &item.badge {
                        Self::template_ok(badge)?;
                    }
                }
            }
            Node::Stats { items } => {
                item_count(items.len(), 1, 6)?;
                for item in items {
                    Self::template_ok(&item.value)?;
                    Self::template_ok(&item.label)?;
                    if let Some(delta) = &item.delta {
                        Self::template_ok(delta)?;
                    }
                }
            }
            Node::Steps { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.title)?;
                    if let Some(text) = &item.text {
                        Self::template_ok(text)?;
                    }
                }
            }
            Node::Callout { title, text, .. } => {
                Self::template_ok(text)?;
                if let Some(title) = title {
                    Self::template_ok(title)?;
                }
            }
            Node::Quote { text, by } => {
                Self::template_ok(text)?;
                if let Some(by) = by {
                    Self::template_ok(by)?;
                }
            }
            Node::Divider { label: Some(label) } => Self::template_ok(label)?,
            _ => {}
        }
        Ok(())
    }

    fn validate_component(&self, node: &Node, budget: &mut usize) -> Result<usize> {
        if node.is_display() {
            Self::validate_display(node)?;
            return Ok(0);
        }
        match node {
            Node::Kv { items } => {
                item_count(items.len(), 1, 8)?;
                for pair in items {
                    for text in pair {
                        Self::template_ok(text)?;
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
                    Self::template_ok(header)?;
                }
                for row in rows_data {
                    ensure!(row.len() == columns.len(), "table row width mismatch");
                    for cell in row {
                        Self::template_ok(cell)?;
                    }
                }
                if let Some(key) = select {
                    binding_ok(&self.state, key, Value::is_number)?;
                    return Ok(1);
                }
            }
            Node::Progress { label, value, max } => {
                Self::template_ok(label)?;
                self.numeric_ok(value, budget)?;
                self.numeric_ok(max, budget)?;
            }
            Node::Bars { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.label)?;
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
                    Self::template_ok(&item.label)?;
                    binding_ok(&self.state, &item.key, Value::is_boolean)?;
                }
                return Ok(items.len());
            }
            Node::Select {
                key,
                label,
                options,
            } => {
                Self::template_ok(label)?;
                item_count(options.len(), 2, 8)?;
                for option in options {
                    line_ok(option, 64)?;
                    Self::template_ok(option)?;
                }
                binding_ok(&self.state, key, |value| {
                    value
                        .as_str()
                        .is_some_and(|text| options.iter().any(|s| s == text))
                })?;
                return Ok(1);
            }
            Node::Toggle { key, label } => {
                Self::template_ok(label)?;
                binding_ok(&self.state, key, Value::is_boolean)?;
                return Ok(1);
            }
            _ => {}
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

    fn template_ok(text: &str) -> Result<()> {
        text_ok(text, MAX_TEXT)?;
        let mut rest = text;
        while let Some((_, tail)) = rest.split_once("{{") {
            let (_, after) = tail
                .split_once("}}")
                .ok_or_else(|| anyhow::anyhow!("unclosed UI template"))?;
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
    ) -> Result<(bool, bool)> {
        spend(budget)?;
        ensure!(
            depth < 8 && seen.insert(action.into()),
            "cyclic or deeply nested UI action"
        );
        let mut sends = false;
        let mut answers = false;
        for step in &self.actions[action] {
            spend(budget)?;
            let (message, answer) = match step {
                Step::Send { .. } => (true, false),
                Step::Answer { .. } => (true, true),
                Step::Call { action } => self.action_sends(action, seen, depth + 1, budget)?,
                Step::Set { .. } => (false, false),
            };
            // An answer can't be combined with another message. Two `send`s
            // stay a runtime error (documents written before answers parse
            // unchanged).
            ensure!(
                !(sends && message && (answers || answer)),
                "an action may send only one message"
            );
            sends |= message;
            answers |= answer;
        }
        seen.remove(action);
        Ok((sends, answers))
    }

    /// Document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    /// Document frame style.
    #[must_use]
    pub const fn frame(&self) -> Frame {
        self.frame
    }
    /// Theme role for highlights.
    #[must_use]
    pub const fn accent(&self) -> Accent {
        self.accent
    }
    /// Number of independently focusable controls.
    #[must_use]
    pub fn control_count(&self) -> usize {
        self.view
            .iter()
            .map(|node| match node {
                Node::Buttons { items, .. } => items.len(),
                Node::Checklist { items } => items.len(),
                Node::Input { .. }
                | Node::Select { .. }
                | Node::Toggle { .. }
                | Node::Table {
                    select: Some(_), ..
                } => 1,
                _ => 0,
            })
            .sum()
    }
    /// Number of named actions.
    #[must_use]
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }
    /// Whether this is a display-only layout, with neither actions nor controls.
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.actions.is_empty()
            && !self.view.iter().any(|node| {
                matches!(
                    node,
                    Node::Buttons { .. }
                        | Node::Input { .. }
                        | Node::Checklist { .. }
                        | Node::Select { .. }
                        | Node::Toggle { .. }
                        | Node::Table {
                            select: Some(_),
                            ..
                        }
                )
            })
    }
    /// Readable plain rendering using initial state, without JSON or markup.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let Ok(session) = Session::new(self.clone()) else {
            return self.title.clone();
        };
        let mut lines = vec![self.title.clone()];
        for node in &self.view {
            lines.extend(session.plain_node(node));
        }
        lines.join("\n")
    }
    /// Immutable, validated presentation nodes.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.view
    }
    /// Declared validation budget; frontends measure their actual layout.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.view.iter().map(Node::rows).sum::<u16>() + 3
    }
    /// Whether activation can send a message, for explicit frontend labelling.
    #[must_use]
    pub fn sends(&self, action: &str) -> bool {
        self.sending.contains(action)
    }
    /// Whether activation immediately answers with the clicked button's label.
    #[must_use]
    pub fn answers(&self, action: &str) -> bool {
        self.answering.contains(action)
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
    fn plain_node(&self, node: &Node) -> Vec<String> {
        let text = |s: &str| self.text(s).unwrap_or_default();
        match node {
            Node::Hero {
                title,
                subtitle,
                badge,
            } => std::iter::once(text(title))
                .chain(subtitle.iter().chain(badge).map(|s| text(s)))
                .collect(),
            Node::Columns { items } => items
                .iter()
                .flat_map(|column| {
                    std::iter::once(text(&column.title)).chain(column.items.iter().map(|item| {
                        let mark = match item.mark {
                            Mark::Ok => "✓ ",
                            Mark::No => "✗ ",
                            Mark::Dot => "● ",
                            Mark::None => "",
                        };
                        format!("{mark}{}", text(&item.text))
                    }))
                })
                .collect(),
            Node::Cards { items } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.title))
                        .chain(item.badge.iter().map(|s| text(s)))
                        .chain(std::iter::once(text(&item.text)))
                })
                .collect(),
            Node::Stats { items } => items
                .iter()
                .map(|item| {
                    let delta = item
                        .delta
                        .as_ref()
                        .map_or_else(String::new, |s| format!(" ({})", text(s)));
                    format!("{}: {}{delta}", text(&item.label), text(&item.value))
                })
                .collect(),
            Node::Steps { items } => items
                .iter()
                .flat_map(|item| {
                    let mark = match item.state {
                        StepState::Done => "●",
                        StepState::Current => "◉",
                        StepState::Todo => "○",
                    };
                    std::iter::once(format!("{mark} {}", text(&item.title)))
                        .chain(item.text.iter().map(|s| text(s)))
                })
                .collect(),
            Node::Callout {
                title, text: body, ..
            } => title
                .iter()
                .map(|s| text(s))
                .chain(std::iter::once(text(body)))
                .collect(),
            Node::Quote { text: body, by } => std::iter::once(format!("\"{}\"", text(body)))
                .chain(by.iter().map(|s| format!("— {}", text(s))))
                .collect(),
            Node::Divider { label } => {
                vec![label.as_ref().map_or_else(|| "---".into(), |s| text(s))]
            }
            _ => self.plain_component(node),
        }
    }

    fn plain_component(&self, node: &Node) -> Vec<String> {
        let text = |s: &str| self.text(s).unwrap_or_default();
        match node {
            Node::Text { text: body, .. } => vec![text(body)],
            Node::Buttons { items, .. } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.label)).chain(item.body.iter().map(|s| text(s)))
                })
                .collect(),
            Node::Input { key, label }
            | Node::Select { key, label, .. }
            | Node::Toggle { key, label } => {
                vec![format!("{}: {}", text(label), self.value(key))]
            }
            Node::Kv { items } => items
                .iter()
                .map(|pair| format!("{}: {}", text(&pair[0]), text(&pair[1])))
                .collect(),
            Node::Table {
                columns, rows_data, ..
            } => std::iter::once(columns)
                .chain(rows_data)
                .map(|row| row.iter().map(|s| text(s)).collect::<Vec<_>>().join(" | "))
                .collect(),
            Node::Progress { label, value, max } => {
                let (value, max) = self.progress(value, max).unwrap_or_default();
                vec![format!("{}: {value}/{max}", text(label))]
            }
            Node::Bars { items } => items
                .iter()
                .map(|item| {
                    format!(
                        "{}: {}",
                        text(&item.label),
                        self.number(&item.value).unwrap_or_default()
                    )
                })
                .collect(),
            Node::Sparkline { values, .. } => vec![
                values
                    .iter()
                    .map(|v| self.number(v).unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ],
            Node::Checklist { items } => items
                .iter()
                .map(|item| {
                    format!(
                        "{} {}",
                        if self.value(&item.key) == "true" {
                            "✓"
                        } else {
                            "○"
                        },
                        text(&item.label)
                    )
                })
                .collect(),
            _ => Vec::new(),
        }
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
        ensure!(
            !self.document.answers(action),
            "answer actions require a button"
        );
        self.activate_action(action)
    }
    /// Activate a button, capturing its rendered label before any state changes.
    /// Answer messages are single-line labels, without the review-message prefix.
    /// # Errors
    /// Rejects non-buttons, invalid rendered answer labels or failed actions.
    pub fn activate_button(&mut self, node: usize, item: usize) -> Result<Option<String>> {
        let Some(Node::Buttons { items, .. }) = self.document.view.get(node) else {
            bail!("answer actions require a button");
        };
        let button = items
            .get(item)
            .ok_or_else(|| anyhow::anyhow!("unknown button"))?;
        let action = button.action.clone();
        let answer = if self.document.answers(&action) {
            let label = self.text(&button.label)?;
            line_ok(&label, 120)?;
            Some(label)
        } else {
            None
        };
        let message = self.activate_action(&action)?;
        Ok(answer.or(message))
    }
    fn activate_action(&mut self, action: &str) -> Result<Option<String>> {
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
                Step::Answer { .. } => {}
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
    let value = state.get(key).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown bound field: {key} — valid: {}",
            state.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
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
            "unknown expression field — valid: {}",
            state.keys().cloned().collect::<Vec<_>>().join(", ")
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
        _ => bail!("unsupported UI operator: {op} — valid: {EXPR_OPS}"),
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
        result.push_str(head);
        if let Some(value) = state.get(key.trim()) {
            result.push_str(&scalar_text(value));
        }
        ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
        rest = after;
    }
    result.push_str(rest);
    ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
    Ok(result)
}

#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "interactive/answer_tests.rs"]
mod answer_tests;

#[cfg(test)]
#[path = "interactive/layouts_tests.rs"]
mod layouts_tests;

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;
