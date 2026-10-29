//! Positional contracts for the terminal component library. Argument order is
//! persistent data: append optional parameters, never reorder existing ones.

use super::syntax::Expr;
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

// Name, internal node tag (empty for data items), ordered parameters.
const COMPONENTS: &[(&str, &str, &str)] = &[
    ("Panel", "panel", "title view frame? accent?"),
    ("Hero", "hero", "title subtitle? badge? image? alt?"),
    ("Text", "text", "text tone? rows?"),
    ("Image", "image", "src alt? rows?"),
    ("Columns", "columns", "items"),
    ("Column", "", "title items"),
    ("Item", "", "text mark?"),
    ("Cards", "cards", "items"),
    ("Card", "", "title text badge? tone? image? alt?"),
    ("Stats", "stats", "items"),
    ("Stat", "", "value label delta? tone?"),
    ("Steps", "steps", "items"),
    ("Step", "", "title text? state?"),
    ("Callout", "callout", "tone text title?"),
    ("Quote", "quote", "text by?"),
    ("Divider", "divider", "label?"),
    ("Buttons", "buttons", "items rows?"),
    ("Button", "", "label action primary? body? image? alt?"),
    ("Input", "input", "key label rules?"),
    ("Kv", "kv", "items"),
    (
        "Table",
        "table",
        "columns rows_data select? sortable? page_size? search?",
    ),
    ("Progress", "progress", "label value max?"),
    ("Bars", "bars", "items"),
    ("Bar", "", "label value"),
    ("Sparkline", "sparkline", "values rows?"),
    ("Checklist", "checklist", "items"),
    ("Check", "", "label key"),
    ("Select", "select", "key label options"),
    ("Stepper", "stepper", "key label min max step"),
    ("Toggle", "toggle", "key label"),
    ("Tabs", "tabs", "items key?"),
    ("Tab", "", "label view"),
    ("Accordion", "accordion", "items"),
    ("Section", "", "title view open?"),
    ("Chart", "chart", "kind series labels? rows?"),
    ("Series", "", "name values tone?"),
    ("Form", "form", "title fields submit_label action"),
    ("Timeline", "timeline", "items"),
    ("Event", "", "time title text? tone?"),
    ("Rating", "rating", "key label max?"),
    ("Code", "code", "source lang?"),
    ("Badge", "badge", "text tone?"),
    ("Meter", "meter", "label value max? tone?"),
    ("Slider", "slider", "key label min max step"),
    ("Editor", "editor", "key label lang? rows?"),
    ("Choice", "choice", "key label options answer?"),
    ("Quiz", "quiz", "items key?"),
    ("Question", "", "title view? answer? explain?"),
];

pub(super) fn signatures() -> String {
    COMPONENTS
        .iter()
        .map(|(name, _, fields)| {
            format!(
                "{name}({})",
                fields.split_whitespace().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn materialize(
    symbols: &BTreeMap<String, Expr>,
    partial: bool,
    current: Option<&Map<String, Value>>,
    results: Option<&Map<String, Value>>,
) -> Result<Value> {
    let mut context = Context {
        symbols,
        partial,
        visiting: BTreeSet::new(),
        budget: 4096,
        actions: Map::new(),
        state: Map::new(),
        locals: BTreeMap::new(),
        results,
    };
    let mut state = Map::new();
    for (name, expression) in symbols {
        if let Some(key) = name.strip_prefix('$') {
            let value = context.lower(expression, 0)?.unwrap_or(Value::Null);
            let initial = if let Some(value) = current.and_then(|state| state.get(key)) {
                value.clone()
            } else {
                crate::interactive::data::initial(&value)?
            };
            context.state.insert(key.into(), initial);
            state.insert(
                key.into(),
                if value.is_array() {
                    json!({"array":value})
                } else {
                    value
                },
            );
        }
    }
    if let Some(current) = current {
        context.state.clone_from(current);
    }
    let mut calls = Vec::new();
    for (id, expr) in symbols {
        if let Expr::Call(kind, args) = expr
            && matches!(kind.as_str(), "Query" | "Mutation")
        {
            ensure!(
                !id.starts_with('$'),
                "data calls need a plain reference name"
            );
            let query = kind == "Query";
            let arity = if query { 3..=4 } else { 2..=2 };
            ensure!(arity.contains(&args.len()), "invalid {kind} arguments");
            let tool = context.lower(&args[0], 0)?.unwrap_or(Value::Null);
            let tool = crate::interactive::eval(&tool, &context.state, &mut 4096)?;
            let tool = tool
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("data tool must be a name"))?;
            let arguments = context.lower(&args[1], 0)?.unwrap_or(Value::Null);
            let defaults = if query {
                context.data_default(args, 0)?
            } else {
                Value::Null
            };
            let refresh = args
                .get(3)
                .map(|expr| -> Result<Option<f64>> {
                    let value = context.lower(expr, 0)?.unwrap_or(Value::Null);
                    let value = crate::interactive::eval(&value, &context.state, &mut 4096)?;
                    if value.is_null() {
                        return Ok(None);
                    }
                    let seconds = value
                        .as_f64()
                        .ok_or_else(|| anyhow::anyhow!("refresh must be seconds"))?;
                    Ok(Some(seconds.max(15.0)))
                })
                .transpose()?
                .flatten();
            let mut dependencies = Vec::new();
            literal_states(&args[1], &mut dependencies);
            calls.push(json!({"id":id,"tool":tool,"query":query,"args":arguments,"defaults":defaults,"refresh":refresh,"dependencies":dependencies}));
        }
    }
    ensure!(calls.len() <= 32, "maximum 32 data calls per panel");
    ensure!(
        calls
            .iter()
            .filter(|call| !call["refresh"].is_null())
            .count()
            <= 5,
        "maximum 5 refresh timers per panel"
    );
    let root = Expr::Ref("root".into());
    let mut value = context
        .lower(&root, 0)?
        .ok_or_else(|| anyhow::anyhow!("root is not available yet"))?;
    ensure!(value.get("view").is_some(), "root must be a Panel");
    value["version"] = 1.into();
    value["state"] = state.into();
    value["actions"] = context.actions.into();
    value["calls"] = calls.into();
    if partial {
        retain_valid_nodes(&mut value);
    }
    Ok(value)
}

// Validate each arriving sibling against the same runtime contract. A malformed
// component or not-yet-declared binding cannot blank already valid elements.
fn retain_valid_nodes(value: &mut Value) {
    let nodes = value["view"].take();
    let Some(nodes) = nodes.as_array() else {
        return;
    };
    let actions = value["actions"].take();
    value["view"] = Value::Array(Vec::new());
    for node in nodes {
        value["view"]
            .as_array_mut()
            .expect("view array")
            .push(node.clone());
        value["actions"] = used_actions(&value["view"], &actions);
        if crate::interactive::Document::check_value(value.clone()).is_err() {
            value["view"].as_array_mut().expect("view array").pop();
        }
    }
    value["actions"] = used_actions(&value["view"], &actions);
}

fn used_actions(view: &Value, actions: &Value) -> Value {
    let mut used = Map::new();
    for node in view.as_array().into_iter().flatten() {
        for child in node["items"].as_array().into_iter().flatten() {
            used.extend(
                used_actions(&child["view"], actions)
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        used.extend(
            used_actions(&node["fields"], actions)
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        if node["type"].as_str() == Some("form")
            && let Some(name) = node["action"].as_str()
            && let Some(action) = actions.get(name)
        {
            used.insert(name.into(), action.clone());
        }
        if node["type"].as_str() == Some("buttons") {
            for button in node["items"].as_array().into_iter().flatten() {
                if let Some(name) = button["action"].as_str()
                    && let Some(action) = actions.get(name)
                {
                    used.insert(name.into(), action.clone());
                }
            }
        }
    }
    used.into()
}

fn literal_states(expr: &Expr, names: &mut Vec<String>) {
    match expr {
        Expr::State(name) => names.push(name.clone()),
        Expr::Array(items) | Expr::Call(_, items) => {
            for item in items {
                literal_states(item, names);
            }
        }
        Expr::Object(fields) => {
            for (_, value) in fields {
                literal_states(value, names);
            }
        }
        Expr::Member(value, _) | Expr::Unary(_, value) => literal_states(value, names),
        Expr::Index(a, b) | Expr::Binary(_, a, b) => {
            literal_states(a, names);
            literal_states(b, names);
        }
        Expr::Conditional(a, b, c) => {
            literal_states(a, names);
            literal_states(b, names);
            literal_states(c, names);
        }
        Expr::Literal(_) | Expr::Ref(_) => {}
    }
}

struct Context<'a> {
    symbols: &'a BTreeMap<String, Expr>,
    partial: bool,
    visiting: BTreeSet<String>,
    budget: usize,
    actions: Map<String, Value>,
    state: Map<String, Value>,
    locals: BTreeMap<String, Value>,
    results: Option<&'a Map<String, Value>>,
}
impl Context<'_> {
    #[allow(clippy::too_many_lines)]
    fn lower(&mut self, expression: &Expr, depth: usize) -> Result<Option<Value>> {
        ensure!(
            depth < 32 && self.budget > 0,
            "UI reference expansion exceeds budget"
        );
        self.budget -= 1;
        Ok(Some(match expression {
            Expr::Literal(value) => value.clone(),
            Expr::State(name) => json!({"get":name}),
            Expr::Ref(name) => {
                if let Some(value) = self
                    .locals
                    .get(name)
                    .or_else(|| self.results.and_then(|values| values.get(name)))
                {
                    return Ok(Some(json!({"literal": value})));
                }
                if !self.visiting.insert(name.clone()) {
                    ensure!(self.partial, "cyclic reference: {name}");
                    return Ok(None);
                }
                let result = if let Some(expression) = self.symbols.get(name) {
                    if let Expr::Call(kind, args) = expression
                        && matches!(kind.as_str(), "Query" | "Mutation")
                    {
                        let value = if kind == "Query" {
                            self.data_default(args, depth + 1)?
                        } else {
                            json!({"status":"idle","data":null,"error":null})
                        };
                        Ok(Some(json!({"literal":value})))
                    } else {
                        self.lower(expression, depth + 1)
                    }
                } else if matches!(
                    name.as_str(),
                    "__openui_loading" | "__openui_refetching" | "__openui_errors"
                ) {
                    let flags: Map<String, Value> = self.symbols.iter().filter(|(_, expr)| matches!(expr, Expr::Call(kind, _) if kind == "Query" || kind == "Mutation"))
                        .map(|(id, _)| (id.clone(), if name == "__openui_errors" { Value::Null } else { false.into() })).collect();
                    Ok(Some(json!({"literal":flags})))
                } else {
                    ensure!(self.partial, "unresolved reference: {name}");
                    Ok(None)
                };
                self.visiting.remove(name);
                return result;
            }
            Expr::Array(items) => {
                let mut values = Vec::new();
                for item in items {
                    match self.lower(item, depth + 1) {
                        Ok(Some(value)) if self.is_each(item, 0) => {
                            values.extend(value.as_array().into_iter().flatten().cloned());
                        }
                        Ok(Some(value)) => values.push(value),
                        Ok(None) => {}
                        Err(_) if self.partial => {}
                        Err(error) => return Err(error),
                    }
                }
                values.into()
            }
            Expr::Object(fields) => {
                let mut object = Map::new();
                for (key, expression) in fields {
                    object.insert(
                        key.clone(),
                        self.lower(expression, depth + 1)?.unwrap_or(Value::Null),
                    );
                }
                json!({"object":object})
            }
            Expr::Member(expression, field) => {
                let value = self.lower(expression, depth + 1)?.unwrap_or(Value::Null);
                json!({"field":[value,field]})
            }
            Expr::Index(expression, index) => {
                let value = self.lower(expression, depth + 1)?.unwrap_or(Value::Null);
                let index = self.lower(index, depth + 1)?.unwrap_or(Value::Null);
                json!({"index":[value,index]})
            }
            Expr::Call(name, args) => return self.call(name, args, depth + 1),
            Expr::Unary(operator, expression) => {
                let Some(value) = self.lower(expression, depth + 1)? else {
                    return Ok(None);
                };
                if *operator == '!' {
                    json!({"not":value})
                } else if let Some(number) = value.as_f64() {
                    json!(-number)
                } else {
                    json!({"sub":[0,value]})
                }
            }
            Expr::Binary(operator, left, right) => {
                let Some(left) = self.lower(left, depth + 1)? else {
                    return Ok(None);
                };
                let Some(right) = self.lower(right, depth + 1)? else {
                    return Ok(None);
                };
                let op = match operator.as_str() {
                    "+" => "add",
                    "-" => "sub",
                    "*" => "mul",
                    "/" => "div",
                    "%" => "mod",
                    "==" | "!=" => "eq",
                    "<" | ">=" => "lt",
                    ">" | "<=" => "gt",
                    "&&" => "and",
                    "||" => "or",
                    _ => bail!("unknown operator"),
                };
                let value = json!({op:[left,right]});
                if matches!(operator.as_str(), "!=" | ">=" | "<=") {
                    json!({"not":value})
                } else {
                    value
                }
            }
            Expr::Conditional(condition, yes, no) => {
                let Some(condition) = self.lower(condition, depth + 1)? else {
                    return Ok(None);
                };
                let Some(yes) = self.lower(yes, depth + 1)? else {
                    return Ok(None);
                };
                let Some(no) = self.lower(no, depth + 1)? else {
                    return Ok(None);
                };
                json!({"if":[condition,yes,no]})
            }
        }))
    }
    fn is_each(&self, expression: &Expr, depth: usize) -> bool {
        if depth >= 32 {
            return false;
        }
        match expression {
            Expr::Call(name, _) => name == "@Each",
            Expr::Ref(name) => self
                .symbols
                .get(name)
                .is_some_and(|expr| self.is_each(expr, depth + 1)),
            _ => false,
        }
    }
    fn binding(expression: &Expr) -> Result<String> {
        match expression {
            Expr::State(name) => Ok(name.clone()),
            _ => bail!("binding must be a $state variable"),
        }
    }
    fn is_textual(&mut self, expression: &Expr, depth: usize) -> bool {
        if depth >= 32 || self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        match expression {
            Expr::Literal(Value::String(_)) => true,
            Expr::Ref(name) => self
                .symbols
                .get(name)
                .is_some_and(|expr| self.is_textual(expr, depth + 1)),
            Expr::State(name) => self
                .symbols
                .get(&format!("${name}"))
                .is_some_and(|expr| self.is_textual(expr, depth + 1)),
            Expr::Binary(operator, left, right) if operator == "+" => {
                self.is_textual(left, depth + 1) || self.is_textual(right, depth + 1)
            }
            _ => false,
        }
    }
    fn template(&mut self, expression: &Expr, depth: usize) -> Result<Option<Value>> {
        ensure!(
            depth < 32 && self.budget > 0,
            "template exceeds expansion budget"
        );
        self.budget -= 1;
        match expression {
            Expr::State(name) => Ok(Some(format!("{{{{{name}}}}}").into())),
            Expr::Binary(operator, left, right) if operator == "+" => {
                ensure!(
                    self.is_textual(left, 0) || self.is_textual(right, 0),
                    "numeric arithmetic in text is unsupported; use Progress/Bars or a scalar state value"
                );
                let Some(left) = self.template(left, depth + 1)? else {
                    return Ok(None);
                };
                let Some(right) = self.template(right, depth + 1)? else {
                    return Ok(None);
                };
                Ok(Some(
                    format!(
                        "{}{}",
                        left.as_str().unwrap_or_default(),
                        right.as_str().unwrap_or_default()
                    )
                    .into(),
                ))
            }
            Expr::Ref(name) => {
                if let Some(value) = self.locals.get(name) {
                    return Ok(Some(crate::interactive::scalar_text(value).into()));
                }
                ensure!(
                    self.visiting.insert(name.clone()),
                    "cyclic template reference: {name}"
                );
                let result = match self.symbols.get(name) {
                    Some(Expr::Call(kind, _)) if matches!(kind.as_str(), "Query" | "Mutation") => {
                        self.visiting.remove(name);
                        let value = self.lower(expression, depth + 1)?.unwrap_or(Value::Null);
                        let value = crate::interactive::eval(&value, &self.state, &mut 4096)?;
                        Ok(Some(crate::interactive::scalar_text(&value).into()))
                    }
                    Some(expression) => self.template(expression, depth + 1),
                    None if self.partial => Ok(None),
                    None => bail!("unresolved reference: {name}"),
                };
                self.visiting.remove(name);
                result
            }
            Expr::Literal(Value::Null) => Ok(Some(Value::Null)),
            Expr::Literal(Value::String(text)) => Ok(Some(text.clone().into())),
            Expr::Literal(value) => Ok(Some(value.to_string().into())),
            _ => {
                let Some(value) = self.lower(expression, depth + 1)? else {
                    return Ok(None);
                };
                let value = crate::interactive::eval(&value, &self.state, &mut 1024)?;
                Ok(Some(crate::interactive::scalar_text(&value).into()))
            }
        }
    }
    fn data_default(&mut self, args: &[Expr], depth: usize) -> Result<Value> {
        let expr = args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("Query requires defaults"))?;
        let value = self.lower(expr, depth + 1)?.unwrap_or(Value::Null);
        crate::interactive::eval(&value, &self.state, &mut 4096)
    }
    fn call(&mut self, name: &str, args: &[Expr], depth: usize) -> Result<Option<Value>> {
        if name.starts_with('@') || name == "Action" {
            return self.function(name, args, depth);
        }
        let Some((_, tag, fields)) = COMPONENTS
            .iter()
            .find(|(component, _, _)| *component == name)
        else {
            bail!(
                "unknown component {name}; supported signatures:\n{}",
                signatures()
            );
        };
        let fields: Vec<_> = fields.split_whitespace().collect();
        ensure!(args.len() <= fields.len(), "too many arguments for {name}");
        let mut object = Map::new();
        if !tag.is_empty() && *tag != "panel" {
            object.insert("type".into(), (*tag).into());
        }
        for (index, field) in fields.iter().enumerate() {
            let key = field.trim_end_matches('?');
            let optional = field.ends_with('?');
            let Some(expression) = args.get(index) else {
                if self.partial && !optional {
                    return Ok(None);
                }
                ensure!(optional, "{name} requires {key}");
                continue;
            };
            if optional && matches!(expression, Expr::Literal(Value::Null)) {
                continue;
            }
            let value = if key == "key" || key == "select" {
                Some(Self::binding(expression)?.into())
            } else if matches!(
                key,
                "title"
                    | "subtitle"
                    | "text"
                    | "label"
                    | "badge"
                    | "by"
                    | "delta"
                    | "submit_label"
                    | "time"
                    | "name"
                    | "explain"
            ) || matches!((name, key), ("Stat", "value") | ("Question", "answer"))
            {
                self.template(expression, depth + 1)?
            } else if key == "rules" {
                self.lower(expression, depth + 1)?
                    .map(|value| crate::interactive::eval(&value, &self.state, &mut 1024))
                    .transpose()?
            } else {
                self.lower(expression, depth + 1)?
            };
            let Some(mut value) = value else {
                if optional {
                    continue;
                }
                return Ok(None);
            };
            if matches!(name, "Button" | "Form") && key == "action" {
                let steps = if value.get("op").is_some() {
                    vec![value]
                } else {
                    value
                        .as_array()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Button action must be @Set, @Answer, @ToAssistant or Action"
                            )
                        })?
                        .clone()
                };
                let id = format!("action_{}", self.actions.len());
                self.actions.insert(id.clone(), steps.into());
                value = id.into();
            }
            object.insert(key.into(), value);
        }
        Ok(Some(object.into()))
    }
    #[allow(clippy::too_many_lines)]
    fn function(&mut self, name: &str, args: &[Expr], depth: usize) -> Result<Option<Value>> {
        if name == "@Each" {
            ensure!(args.len() == 3, "@Each expects 3 arguments");
            let array = self.lower(&args[0], depth + 1)?.unwrap_or(Value::Null);
            let array = crate::interactive::eval(&array, &self.state, &mut 4096)?;
            let Expr::Literal(Value::String(var)) = &args[1] else {
                bail!("@Each variable must be a string")
            };
            crate::interactive::identifier(var)?;
            let items = array.as_array().map_or(&[][..], Vec::as_slice);
            ensure!(items.len() <= 200, "@Each exceeds 200 items");
            let previous = self.locals.get(var).cloned();
            let result = (|| {
                let mut result = Vec::new();
                for item in items {
                    self.locals.insert(var.clone(), item.clone());
                    if let Some(value) = self.lower(&args[2], depth + 1)? {
                        result.push(value);
                    }
                }
                Ok(Some(result.into()))
            })();
            if let Some(previous) = previous {
                self.locals.insert(var.clone(), previous);
            } else {
                self.locals.remove(var);
            }
            return result;
        }
        if name == "@Run" {
            ensure!(args.len() == 1, "@Run expects one reference");
            let Expr::Ref(id) = &args[0] else {
                bail!("@Run needs a Query/Mutation reference")
            };
            ensure!(
                matches!(self.symbols.get(id), Some(Expr::Call(kind, _)) if kind == "Query" || kind == "Mutation"),
                "@Run needs a Query/Mutation reference"
            );
            return Ok(Some(json!({"op":"run", "id":id})));
        }
        if name == "@Reset" {
            ensure!(!args.is_empty(), "@Reset needs state variables");
            let keys = args.iter().map(Self::binding).collect::<Result<Vec<_>>>()?;
            return Ok(Some(json!({"op":"reset", "keys":keys})));
        }
        if let Some(op) = crate::interactive::data::builtin(name) {
            crate::interactive::data::arity(op, args.len())?;
            let mut values = Vec::new();
            for arg in args {
                values.push(self.lower(arg, depth + 1)?.unwrap_or(Value::Null));
            }
            return Ok(Some(json!({op:values})));
        }
        let count = match name {
            "@Answer" => 0,
            "@Set" => 2,
            "@If" => 3,
            "Action" | "@ToAssistant" | "@OpenUrl" | "@Number" | "@Random" => 1,
            _ => bail!(
                "unsupported function {name}; valid: @Set, @Answer, @ToAssistant, @Number, @Random, @If, Action"
            ),
        };
        ensure!(
            args.len() == count || name == "@ToAssistant" && args.len() == 2,
            "{name} expects {count} arguments"
        );
        if name == "@Answer" {
            return Ok(Some(json!({"op":"answer"})));
        }
        if name == "@OpenUrl" {
            return Ok(self
                .lower(&args[0], depth + 1)?
                .map(|url| json!({"op":"open_url","url":url})));
        }
        if name == "@ToAssistant" {
            let context = args
                .get(1)
                .map(|arg| self.lower(arg, depth + 1))
                .transpose()?
                .flatten();
            return Ok(self.template(&args[0], depth + 1)?.map(|text| {
                let mut step = json!({"op":"to_assistant","text":text});
                if let Some(context) = context {
                    step["context"] = context;
                }
                step
            }));
        }
        if name == "@Set" {
            let key = Self::binding(&args[0])?;
            return Ok(self
                .lower(&args[1], depth + 1)?
                .map(|value| json!({"op":"set","key":key,"value":value})));
        }
        let mut values = Vec::new();
        for expression in args {
            let Some(value) = self.lower(expression, depth + 1)? else {
                return Ok(None);
            };
            values.push(value);
        }
        Ok(Some(match name {
            "Action" => {
                let steps = values.remove(0);
                ensure!(
                    steps
                        .as_array()
                        .is_some_and(|steps| steps.iter().all(|step| step.get("op").is_some())),
                    "Action expects an array of action calls"
                );
                steps
            }
            "@Number" => json!({"number":values.remove(0)}),
            "@Random" => json!({"random":values.remove(0)}),
            "@If" => json!({"if":values}),
            _ => unreachable!("validated function"),
        }))
    }
}
