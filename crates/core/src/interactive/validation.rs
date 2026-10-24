//! Input rules, including an anchored, regex-free ASCII pattern subset.

use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value};

pub(super) fn check(rules: &Map<String, Value>) -> Result<()> {
    for (key, value) in rules {
        match key.as_str() {
            "required" | "email" | "url" | "numeric" => {
                ensure!(value.is_boolean(), "{key} must be boolean");
            }
            "minLength" | "maxLength" => {
                ensure!(value.as_u64().is_some_and(|n| n <= 1024), "invalid {key}");
            }
            "min" | "max" => ensure!(value.as_f64().is_some_and(f64::is_finite), "invalid {key}"),
            "pattern" => {
                tokens(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("pattern must be a string"))?,
                )?;
            }
            _ => bail!("unknown validation rule: {key}"),
        }
    }
    Ok(())
}

pub(super) fn error(rules: &Map<String, Value>, value: &str) -> Option<String> {
    let enabled = |key: &str| rules.get(key).and_then(Value::as_bool) == Some(true);
    // Whitespace alone doesn't satisfy `required`.
    if value.trim().is_empty() {
        return enabled("required").then(|| "Required".into());
    }
    let email = || {
        let Some((local, domain)) = value.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && !domain.contains('@')
            && !value.chars().any(char::is_whitespace)
            && domain
                .rsplit_once('.')
                .is_some_and(|(host, tld)| !host.is_empty() && !tld.is_empty())
    };
    let numeric = || {
        let text = value.trim_start();
        (1..=text.len())
            .rev()
            .filter(|end| text.is_char_boundary(*end))
            .any(|end| text[..end].parse::<f64>().is_ok_and(|n| !n.is_nan()))
    };
    let length = u64::try_from(value.chars().count()).unwrap_or(u64::MAX);
    let failure = if enabled("email") && !email() {
        Some("Enter an email address")
    } else if enabled("url") && !reqwest::Url::parse(value).is_ok_and(|url| url.has_host()) {
        Some("Enter an absolute URL")
    } else if enabled("numeric") && !numeric() {
        Some("Enter a number")
    } else if rules
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|min| length < min)
    {
        Some("Too short")
    } else if rules
        .get("maxLength")
        .and_then(Value::as_u64)
        .is_some_and(|max| length > max)
    {
        Some("Too long")
    } else if rules
        .get("min")
        .and_then(Value::as_f64)
        .is_some_and(|min| value.parse::<f64>().map_or(true, |n| n < min))
    {
        Some("Below minimum")
    } else if rules
        .get("max")
        .and_then(Value::as_f64)
        .is_some_and(|max| value.parse::<f64>().map_or(true, |n| n > max))
    {
        Some("Above maximum")
    } else if rules
        .get("pattern")
        .and_then(Value::as_str)
        .is_some_and(|p| !matches(p, value))
    {
        Some("Does not match pattern")
    } else {
        None
    };
    failure.map(str::to_string)
}

struct Token {
    ranges: Vec<(u8, u8)>,
    min: usize,
    max: usize,
}
fn tokens(pattern: &str) -> Result<Vec<Token>> {
    ensure!(
        pattern.len() <= 120 && pattern.is_ascii(),
        "pattern must be at most 120 ASCII bytes"
    );
    ensure!(
        pattern.starts_with('^') && pattern.ends_with('$'),
        "pattern must be anchored with ^ and $"
    );
    let bytes = &pattern.as_bytes()[1..pattern.len() - 1];
    let mut offset = 0;
    let mut result = Vec::new();
    while offset < bytes.len() {
        let ch = bytes[offset];
        offset += 1;
        let ranges = match ch {
            b'.' => vec![(32, 126)],
            b'[' => {
                let mut ranges = Vec::new();
                while offset < bytes.len() && bytes[offset] != b']' {
                    let start = bytes[offset];
                    offset += 1;
                    let end = if offset + 1 < bytes.len() && bytes[offset] == b'-' {
                        offset += 2;
                        bytes[offset - 1]
                    } else {
                        start
                    };
                    ensure!(
                        start <= end && !matches!(start, b'^' | b'\\' | b'['),
                        "unsupported character class"
                    );
                    ranges.push((start, end));
                }
                ensure!(
                    !ranges.is_empty() && bytes.get(offset) == Some(&b']'),
                    "unclosed character class"
                );
                offset += 1;
                ranges
            }
            b'\\' => {
                let ch = *bytes
                    .get(offset)
                    .ok_or_else(|| anyhow::anyhow!("unfinished pattern escape"))?;
                offset += 1;
                match ch {
                    b'd' => vec![(b'0', b'9')],
                    b'w' => vec![(b'0', b'9'), (b'a', b'z'), (b'A', b'Z'), (b'_', b'_')],
                    b's' => vec![(b' ', b' ')],
                    b'.' | b'+' | b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'(' | b')' | b'|'
                    | b'^' | b'$' | b'\\' => vec![(ch, ch)],
                    _ => bail!("unsupported pattern escape"),
                }
            }
            b'(' | b')' | b'|' | b'^' | b'$' | b'*' | b'+' | b'?' | b'{' | b'}' | b']' => {
                bail!("unsupported pattern syntax")
            }
            _ => vec![(ch, ch)],
        };
        let (min, max) = match bytes.get(offset) {
            Some(b'*') => {
                offset += 1;
                (0, 1024)
            }
            Some(b'+') => {
                offset += 1;
                (1, 1024)
            }
            Some(b'?') => {
                offset += 1;
                (0, 1)
            }
            _ => (1, 1),
        };
        result.push(Token { ranges, min, max });
    }
    Ok(result)
}
fn matches(pattern: &str, text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    let Ok(tokens) = tokens(pattern) else {
        return false;
    };
    let mut reachable = vec![false; text.len() + 1];
    reachable[0] = true;
    // Linear dynamic programming per token, not recursive backtracking.
    for token in tokens {
        let mut next = vec![false; text.len() + 1];
        let mut run = 0;
        let mut latest = None;
        for end in 0..=text.len() {
            if end > 0 {
                if token
                    .ranges
                    .iter()
                    .any(|(a, b)| (*a..=*b).contains(&text.as_bytes()[end - 1]))
                {
                    run += 1;
                } else {
                    run = 0;
                }
            }
            if end >= token.min && reachable[end - token.min] {
                latest = Some(end - token.min);
            }
            next[end] = latest.is_some_and(|start| end - start <= token.max && end - start <= run);
        }
        reachable = next;
    }
    reachable[text.len()]
}
