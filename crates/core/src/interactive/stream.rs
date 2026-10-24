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
        let value = library::materialize(&symbols, true).ok()?;
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
            symbols.insert(name, expression);
        }
        start = end;
    }
    library::materialize(&symbols, false)
}

pub(super) fn preview(source: &str) -> Option<Document> {
    StreamParser::default().set(source)
}

#[cfg(test)]
mod tests;
