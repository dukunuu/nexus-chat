//! Native OpenUI-style programs: cached statements, an implicitly closed tail,
//! hoisted references, and positional calls into a bounded terminal library.

mod library;
mod syntax;

use super::{CheckError, Document, MAX_SOURCE};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::BTreeMap;
use syntax::Expr;

/// Positional component signatures generated from the parser's library registry.
#[must_use]
pub fn component_signatures() -> String {
    library::signatures()
}

/// Incremental parser for one streamed UI program. Completed statement ASTs are
/// cached; source replacement resets them. No actions are executed by parsing.
#[derive(Default)]
pub struct StreamParser {
    source: String,
    consumed: usize,
    symbols: BTreeMap<String, Expr>,
    error: Option<String>,
}
impl StreamParser {
    /// Update the accumulated program and return its validated display snapshot.
    /// Incomplete new definitions can render; incomplete redefinitions cannot
    /// replace completed ones. Unsupported or malformed elements remain inert.
    #[must_use]
    pub fn set(&mut self, source: &str) -> Option<Document> {
        if source.len() > MAX_SOURCE {
            return None;
        }
        if !source.starts_with(&self.source) {
            *self = Self::default();
        }
        self.source = source.into();
        let (ends, suffix) = syntax::boundaries(&source[self.consumed..]);
        let start = self.consumed;
        for end in ends {
            let statement = &source[self.consumed..start + end];
            if !empty(statement) && self.error.is_none() {
                match syntax::statement(statement) {
                    Ok((name, expression)) => {
                        if self.symbols.len() >= 128 && !self.symbols.contains_key(&name) {
                            self.error = Some("UI exceeds 128 statements".into());
                        } else {
                            self.symbols.insert(name, expression);
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            self.consumed = start + end;
        }
        let mut symbols = self.symbols.clone();
        let tail = &source[self.consumed..];
        if self.error.is_none() && !empty(tail) {
            let pending = format!("{tail}{suffix}");
            if let Ok((name, expression)) = syntax::statement(&pending)
                && (suffix.is_empty() || !symbols.contains_key(&name))
                && (symbols.len() < 128 || symbols.contains_key(&name))
            {
                symbols.insert(name, expression);
            }
        }
        let value = library::materialize(&symbols, true, None, None).ok()?;
        Document::check_value(value)
            .ok()
            .map(Document::inert_preview)
    }
}

fn empty(source: &str) -> bool {
    source.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with('#') || line.starts_with("//")
    })
}

pub(super) fn decode(source: &str) -> std::result::Result<Value, Vec<CheckError>> {
    let json = source.trim_start().starts_with('{');
    let result = if json {
        legacy(source)
    } else {
        program(source)
    };
    result.map_err(|error| {
        vec![CheckError {
            path: if json { "$" } else { "source" }.into(),
            message: error.to_string(),
        }]
    })
}

fn legacy(source: &str) -> Result<Value> {
    let mut stream = serde_json::Deserializer::from_str(source).into_iter::<Value>();
    let value = stream
        .next()
        .unwrap_or_else(|| serde_json::from_str(source))?;
    ensure!(
        source[stream.byte_offset()..]
            .chars()
            .all(|ch| ch.is_whitespace() || matches!(ch, '}' | ']')),
        "trailing characters after UI document"
    );
    Ok(value)
}

fn program(source: &str) -> Result<Value> {
    program_state(source, None, None)
}

pub(super) fn program_state(
    source: &str,
    state: Option<&serde_json::Map<String, Value>>,
    results: Option<&serde_json::Map<String, Value>>,
) -> Result<Value> {
    let symbols = statements(source)?
        .into_iter()
        .map(|(name, (_, expr))| (name, expr))
        .collect();
    library::materialize(&symbols, false, state, results)
}

fn statements(source: &str) -> Result<BTreeMap<String, (String, Expr)>> {
    ensure!(source.len() <= MAX_SOURCE, "UI document exceeds 16 KiB");
    let (mut ends, _) = syntax::boundaries(source);
    if ends.last() != Some(&source.len()) {
        ends.push(source.len());
    }
    let mut start = 0;
    let mut symbols = BTreeMap::new();
    for end in ends {
        let statement = &source[start..end];
        if !empty(statement) {
            let line = source[..start].bytes().filter(|ch| *ch == b'\n').count() + 1;
            let (name, expression) = syntax::statement(statement)
                .map_err(|error| anyhow::anyhow!("line {line}: {error}"))?;
            ensure!(
                symbols.len() < 128 || symbols.contains_key(&name),
                "UI exceeds 128 statements"
            );
            symbols.insert(name, (statement.trim().to_string(), expression));
        }
        start = end;
    }
    Ok(symbols)
}

/// Merge a completed patch by statement name and validate it atomically.
/// A top-level null deletes; unreachable value statements are pruned.
/// # Errors
/// Returns syntax, reference, schema or budget failures without changing input.
pub fn merge_program(source: &str, patch: &str) -> Result<String> {
    let mut merged = statements(source)?;
    for (name, statement) in statements(patch)? {
        if matches!(statement.1, Expr::Literal(Value::Null)) {
            merged.remove(&name);
        } else {
            merged.insert(name, statement);
        }
    }
    if merged.contains_key("root") {
        let mut reachable = std::collections::BTreeSet::new();
        let mut pending = vec!["root".to_string()];
        pending.extend(merged.iter().filter(|(name, (_, expr))| {
            name.starts_with('$')
                || matches!(expr, Expr::Call(kind, _) if kind == "Query" || kind == "Mutation")
        }).map(|(name, _)| name.clone()));
        while let Some(name) = pending.pop() {
            if reachable.insert(name.clone())
                && let Some((_, expression)) = merged.get(&name)
            {
                references(expression, &mut pending);
            }
        }
        merged.retain(|name, _| reachable.contains(name));
    }
    let program = merged
        .values()
        .map(|(source, _)| source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Document::parse(&program)?;
    Ok(program)
}

fn references(expression: &Expr, names: &mut Vec<String>) {
    match expression {
        Expr::Ref(name) => names.push(name.clone()),
        Expr::State(name) => names.push(format!("${name}")),
        Expr::Array(items) | Expr::Call(_, items) => {
            for item in items {
                references(item, names);
            }
        }
        Expr::Object(fields) => {
            for (_, item) in fields {
                references(item, names);
            }
        }
        Expr::Member(value, _) | Expr::Unary(_, value) => references(value, names),
        Expr::Index(a, b) | Expr::Binary(_, a, b) => {
            references(a, names);
            references(b, names);
        }
        Expr::Conditional(a, b, c) => {
            references(a, names);
            references(b, names);
            references(c, names);
        }
        Expr::Literal(_) => {}
    }
}

pub(super) fn preview(source: &str) -> Option<Document> {
    StreamParser::default().set(source)
}

#[cfg(test)]
mod tests;
