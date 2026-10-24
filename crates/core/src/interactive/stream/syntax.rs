//! Bounded recursive-descent parser for the native `OpenUI` dialect.

use anyhow::{Result, bail, ensure};
use serde_json::Value;

#[derive(Clone, Debug)]
pub(super) enum Expr {
    Literal(Value),
    Ref(String),
    State(String),
    Array(Vec<Self>),
    Call(String, Vec<Self>),
    Unary(char, Box<Self>),
    Binary(String, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
}

pub(super) fn statement(source: &str) -> Result<(String, Expr)> {
    let mut reader = Reader {
        source,
        offset: 0,
        budget: 2048,
    };
    reader.space();
    let state = reader.take("$");
    let mut name = reader.name()?;
    if state {
        name.insert(0, '$');
    }
    ensure!(reader.take("="), "expected '=' after {name}");
    let expr = reader.expression(0, 0)?;
    reader.space();
    ensure!(
        reader.offset == source.len(),
        "unexpected source after {name}"
    );
    Ok((name, expr))
}

struct Reader<'a> {
    source: &'a str,
    offset: usize,
    budget: usize,
}
impl Reader<'_> {
    fn rest(&self) -> &str {
        &self.source[self.offset..]
    }
    fn space(&mut self) {
        loop {
            while let Some(ch) = self.rest().chars().next().filter(|c| c.is_whitespace()) {
                self.offset += ch.len_utf8();
            }
            if self.rest().starts_with('#') || self.rest().starts_with("//") {
                self.offset += self.rest().find('\n').unwrap_or(self.rest().len());
            } else {
                break;
            }
        }
    }
    fn take(&mut self, token: &str) -> bool {
        self.space();
        if self.rest().starts_with(token) {
            self.offset += token.len();
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Result<String> {
        self.space();
        let length = self
            .rest()
            .bytes()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == b'_')
            .count();
        ensure!(
            length > 0 && !self.rest().as_bytes()[0].is_ascii_digit(),
            "expected identifier"
        );
        let name = self.rest()[..length].to_string();
        ensure!(length <= 32, "identifier exceeds 32 bytes");
        self.offset += length;
        Ok(name)
    }
    fn string(&mut self) -> Result<String> {
        let quote = self.rest().chars().next().expect("opening quote");
        self.offset += 1;
        let mut result = String::new();
        while let Some(ch) = self.rest().chars().next() {
            self.offset += ch.len_utf8();
            if ch == quote {
                return Ok(result);
            }
            if ch != '\\' {
                result.push(ch);
                continue;
            }
            let escape = self
                .rest()
                .chars()
                .next()
                .ok_or_else(|| anyhow::anyhow!("unfinished string escape"))?;
            self.offset += escape.len_utf8();
            match escape {
                'n' => result.push('\n'),
                'r' => result.push('\r'),
                't' => result.push('\t'),
                'b' => result.push('\u{8}'),
                'f' => result.push('\u{c}'),
                '\\' | '/' | '\'' | '"' => result.push(escape),
                'u' => {
                    let start = self.offset.saturating_sub(2);
                    ensure!(self.rest().len() >= 4, "unfinished unicode escape");
                    let mut end = self.offset + 4;
                    ensure!(self.source.is_char_boundary(end), "invalid unicode escape");
                    // Let serde handle UTF-16 surrogate pairs correctly.
                    if self.source.get(end..end + 2) == Some("\\u") {
                        end += 6;
                    }
                    let encoded = self
                        .source
                        .get(start..end)
                        .ok_or_else(|| anyhow::anyhow!("unfinished unicode escape"))?;
                    result.push_str(&serde_json::from_str::<String>(&format!("\"{encoded}\""))?);
                    self.offset = end;
                }
                _ => bail!("unsupported string escape \\{escape}"),
            }
        }
        bail!("unclosed string")
    }
    fn primary(&mut self, depth: usize) -> Result<Expr> {
        self.space();
        ensure!(
            depth < 32 && self.budget > 0,
            "expression exceeds parsing budget"
        );
        self.budget -= 1;
        let first = self
            .rest()
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("expected expression"))?;
        if first == '"' || first == '\'' {
            return Ok(Expr::Literal(self.string()?.into()));
        }
        if self.take("[") {
            return Ok(Expr::Array(self.arguments("]", depth + 1)?));
        }
        if self.take("(") {
            let expr = self.expression(0, depth + 1)?;
            ensure!(self.take(")"), "expected ')'");
            return Ok(expr);
        }
        if self.take("!") {
            return Ok(Expr::Unary('!', Box::new(self.expression(8, depth + 1)?)));
        }
        if self.take("-") {
            return Ok(Expr::Unary('-', Box::new(self.expression(8, depth + 1)?)));
        }
        if first.is_ascii_digit() {
            let length = self
                .rest()
                .bytes()
                .take_while(|ch| {
                    ch.is_ascii_digit() || matches!(ch, b'.' | b'e' | b'E' | b'+' | b'-')
                })
                .enumerate()
                .take_while(|(index, ch)| {
                    !matches!(ch, b'+' | b'-')
                        || *index > 0 && matches!(self.rest().as_bytes()[index - 1], b'e' | b'E')
                })
                .count();
            let number: Value = serde_json::from_str(&self.rest()[..length])?;
            ensure!(number.is_number(), "expected number");
            self.offset += length;
            return Ok(Expr::Literal(number));
        }
        if self.take("$") {
            return Ok(Expr::State(self.name()?));
        }
        let function = self.take("@");
        let name = self.name()?;
        if self.take("(") {
            let args = self.arguments(")", depth + 1)?;
            return Ok(Expr::Call(
                if function { format!("@{name}") } else { name },
                args,
            ));
        }
        ensure!(!function, "function {name} requires parentheses");
        Ok(match name.as_str() {
            "true" => Expr::Literal(true.into()),
            "false" => Expr::Literal(false.into()),
            "null" => Expr::Literal(Value::Null),
            _ => Expr::Ref(name),
        })
    }
    fn arguments(&mut self, close: &str, depth: usize) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.take(close) {
            args.push(self.expression(0, depth)?);
            self.take(",");
        }
        Ok(args)
    }
    fn expression(&mut self, minimum: u8, depth: usize) -> Result<Expr> {
        let mut left = self.primary(depth)?;
        loop {
            self.space();
            let operator = [
                "||", "&&", "==", "!=", ">=", "<=", ">", "<", "+", "-", "*", "/", "%",
            ]
            .into_iter()
            .find(|op| self.rest().starts_with(op));
            let Some(operator) = operator else {
                break;
            };
            let precedence = match operator {
                "||" => 1,
                "&&" => 2,
                "==" | "!=" => 3,
                ">=" | "<=" | ">" | "<" => 4,
                "+" | "-" => 5,
                _ => 6,
            };
            if precedence < minimum {
                break;
            }
            self.offset += operator.len();
            let right = self.expression(precedence + 1, depth + 1)?;
            left = Expr::Binary(operator.into(), Box::new(left), Box::new(right));
        }
        if minimum == 0 && self.take("?") {
            let yes = self.expression(0, depth + 1)?;
            ensure!(self.take(":"), "expected ':' in conditional");
            let no = self.expression(0, depth + 1)?;
            left = Expr::Conditional(Box::new(left), Box::new(yes), Box::new(no));
        }
        Ok(left)
    }
}

/// Statement boundaries and the minimal suffix for a pending string/call.
/// Comments and delimiters in strings cannot corrupt the boundary scan.
pub(super) fn boundaries(source: &str) -> (Vec<usize>, String) {
    let mut ends = Vec::new();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escape = false;
    let mut comment = false;
    let mut chars = source.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if comment {
            if ch != '\n' {
                continue;
            }
            comment = false;
        } else if let Some(delimiter) = quote {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        } else if ch == '#' || ch == '/' && chars.peek().is_some_and(|(_, c)| *c == '/') {
            comment = true;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            ')' | ']' if stack.last() == Some(&ch) => {
                stack.pop();
            }
            '\n' if stack.is_empty() => ends.push(index + 1),
            _ => {}
        }
    }
    let mut suffix = String::new();
    if let Some(quote) = quote {
        if escape {
            suffix.push('\\');
        }
        suffix.push(quote);
    }
    if comment && !stack.is_empty() {
        suffix.push('\n');
    }
    suffix.extend(stack.into_iter().rev());
    (ends, suffix)
}
