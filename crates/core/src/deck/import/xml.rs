//! A small, bounded XML reader for the parts of an OOXML package.
//!
//! It builds a tree of elements with local names (prefixes dropped), their
//! attributes and the text directly inside them. Attributes keep their prefix
//! only when it names the relationships namespace (`r:id`) or is otherwise
//! qualified (`p14:dur`). It rejects nothing a real `PowerPoint` writes, expands
//! only the five predefined entities and numeric references, ignores DOCTYPE
//! declarations (so no entity bomb is possible) and caps depth and size.

use anyhow::{Result, bail};

/// Relationship-ID namespace, the one prefix `PowerPoint` parts rely on.
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_DEPTH: usize = 200;
const MAX_ELEMENTS: usize = 1_500_000;

/// One element: its local name, attributes, children and direct text.
#[derive(Debug, Default, Clone)]
pub(crate) struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<El>,
    pub text: String,
}

impl El {
    /// An attribute by its exact key (`id`, `r:id`, `p14:dur`).
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The `r:`-prefixed attribute `key`.
    pub fn rel(&self, key: &str) -> Option<&str> {
        self.attr(&format!("r:{key}"))
    }

    /// An attribute by its local name, whatever its prefix.
    pub fn attr_local(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.rsplit(':').next() == Some(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        self.attr(key)?
            .trim()
            .parse()
            .ok()
            .filter(|n: &f64| n.is_finite())
    }

    /// A boolean attribute: `1` and `true` are set, `0` and `false` are not.
    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.attr(key)? {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        }
    }

    pub fn kid(&self, name: &str) -> Option<&El> {
        self.kids.iter().find(|k| k.name == name)
    }

    pub fn kids_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> {
        self.kids.iter().filter(move |k| k.name == name)
    }

    /// Follow a chain of child names.
    pub fn path(&self, names: &[&str]) -> Option<&El> {
        names.iter().try_fold(self, |el, name| el.kid(name))
    }

    /// Every descendant (depth first, not self) named `name`.
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a El>) {
        for kid in &self.kids {
            if kid.name == name {
                out.push(kid);
            }
            kid.descendants(name, out);
        }
    }

    /// The first descendant named `name`.
    pub fn find(&self, name: &str) -> Option<&El> {
        for kid in &self.kids {
            if kid.name == name {
                return Some(kid);
            }
            if let Some(found) = kid.find(name) {
                return Some(found);
            }
        }
        None
    }
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Decode predefined and numeric entities; anything else stays as written.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| entity(&rest[1..=end]).map(|c| (c, end + 2)));
        if let Some((c, length)) = decoded {
            out.push(c);
            rest = &rest[length..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn skip_to<'a>(rest: &'a str, end: &str) -> Result<&'a str> {
    match rest.find(end) {
        Some(at) => Ok(&rest[at + end.len()..]),
        None => bail!("unterminated markup"),
    }
}

/// The attributes of a start tag body (`name a="1" b='2'`), resolved against
/// the namespace declarations in scope.
fn attributes(
    body: &str,
    scope: &mut Vec<(String, String)>,
) -> Result<(String, Vec<(String, String)>)> {
    let body = body.trim_end_matches('/');
    let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = body[..name_end].to_string();
    let mut rest = body[name_end..].trim_start();
    let mut raw: Vec<(String, String)> = Vec::new();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else {
            bail!("malformed attribute");
        };
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            bail!("unquoted attribute");
        };
        let Some(close) = after[1..].find(quote) else {
            bail!("unterminated attribute");
        };
        raw.push((key, decode(&after[1..=close])));
        rest = after[close + 2..].trim_start();
    }
    for (key, value) in &raw {
        if let Some(prefix) = key.strip_prefix("xmlns:") {
            scope.push((prefix.to_string(), value.clone()));
        }
    }
    let mut attrs = Vec::with_capacity(raw.len());
    for (key, value) in raw {
        if key == "xmlns" || key.starts_with("xmlns:") {
            continue;
        }
        let key = match key.split_once(':') {
            Some((prefix, local)) => {
                let uri = scope
                    .iter()
                    .rev()
                    .find(|(p, _)| p == prefix)
                    .map(|(_, u)| u.as_str());
                if uri == Some(REL_NS) {
                    format!("r:{local}")
                } else {
                    key
                }
            }
            None => key,
        };
        attrs.push((key, value));
    }
    let local = name.rsplit(':').next().unwrap_or(&name).to_string();
    Ok((local, attrs))
}

/// Parse one XML document.
///
/// # Errors
/// On malformed markup, or a document deeper or larger than the caps.
#[allow(clippy::too_many_lines)]
pub(crate) fn parse(source: &str) -> Result<El> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut stack: Vec<El> = Vec::new();
    // Namespace declarations in scope, and how many each open element added.
    let mut scope: Vec<(String, String)> = Vec::new();
    let mut scope_marks: Vec<usize> = Vec::new();
    let mut root: Option<El> = None;
    let mut count = 0_usize;
    let mut rest = source;
    while !rest.is_empty() {
        let Some(at) = rest.find('<') else {
            break;
        };
        let text = &rest[..at];
        if let Some(top) = stack.last_mut()
            && (top.name == "t" || !text.trim().is_empty())
        {
            top.text.push_str(&decode(text));
        }
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_to(after, "-->")?;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after
                .find("]]>")
                .ok_or_else(|| anyhow::anyhow!("unterminated CDATA"))?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&after[..end]);
            }
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("<?") {
            rest = skip_to(after, "?>")?;
        } else if let Some(after) = rest.strip_prefix("<!") {
            // A DOCTYPE: skipped whole, entity definitions are never used.
            let bracket = after.find('[');
            let close = after.find('>');
            rest = match (bracket, close) {
                (Some(b), Some(c)) if b < c => skip_to(&after[b..], "]>")?,
                _ => skip_to(after, ">")?,
            };
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after
                .find('>')
                .ok_or_else(|| anyhow::anyhow!("unterminated end tag"))?;
            let closing = after[..end].trim();
            let Some(el) = stack.pop() else {
                bail!("unbalanced end tag");
            };
            let local = closing.rsplit(':').next().unwrap_or(closing);
            if el.name != local {
                bail!("mismatched end tag");
            }
            scope.truncate(scope.len() - scope_marks.pop().unwrap_or(0));
            rest = &after[end + 1..];
            match stack.last_mut() {
                Some(parent) => parent.kids.push(el),
                None => root = Some(el),
            }
        } else {
            // A start tag; find its end outside quoted attribute values.
            let body = &rest[1..];
            let mut quote = None;
            let mut end = None;
            for (i, c) in body.char_indices() {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, '>') => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let end = end.ok_or_else(|| anyhow::anyhow!("unterminated start tag"))?;
            let tag = &body[..end];
            let before = scope.len();
            let (name, attrs) = attributes(tag, &mut scope)?;
            let el = El {
                name,
                attrs,
                ..El::default()
            };
            count += 1;
            if count > MAX_ELEMENTS {
                bail!("the XML part has too many elements");
            }
            rest = &body[end + 1..];
            if tag.ends_with('/') {
                scope.truncate(before);
                match stack.last_mut() {
                    Some(parent) => parent.kids.push(el),
                    None => root = Some(el),
                }
            } else {
                if stack.len() >= MAX_DEPTH {
                    bail!("the XML part is nested too deeply");
                }
                scope_marks.push(scope.len() - before);
                stack.push(el);
            }
        }
        if root.is_some() && stack.is_empty() {
            break;
        }
    }
    if !stack.is_empty() {
        bail!("unclosed element");
    }
    root.ok_or_else(|| anyhow::anyhow!("no root element"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_attributes_text_and_entities() {
        let el = parse(
            "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY a \"b\">]><a:root xmlns:a=\"u\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" x=\"1 &amp; 2\"><!-- c --><a:t>a &lt; b &#65;&#x42;&bogus;</a:t><b/><![CDATA[<raw>]]></a:root>",
        )
        .unwrap();
        assert_eq!(el.name, "root");
        assert_eq!(el.rel("id"), Some("rId1"));
        assert_eq!(el.attr("x"), Some("1 & 2"));
        assert_eq!(el.kid("t").unwrap().text, "a < b AB&bogus;");
        assert!(el.kid("b").is_some());
        assert_eq!(el.text, "<raw>");
    }

    #[test]
    fn keeps_whitespace_only_text_runs_and_ignores_layout_whitespace() {
        let el = parse("<a>\n  <t> </t>\n  <b>x</b>\n</a>").unwrap();
        assert_eq!(el.kid("t").unwrap().text, " ");
        assert_eq!(el.text, "");
    }

    #[test]
    fn rejects_malformed_and_runaway_input() {
        for bad in [
            "",
            "<a>",
            "<a></b>",
            "</a>",
            "<a x=1/>",
            "<a x=\"1/>",
            "not xml",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = format!(
            "{}{}",
            "<a>".repeat(MAX_DEPTH + 1),
            "</a>".repeat(MAX_DEPTH + 1)
        );
        assert!(parse(&deep).is_err());
        let fine = format!(
            "{}{}",
            "<a>".repeat(MAX_DEPTH - 1),
            "</a>".repeat(MAX_DEPTH - 1)
        );
        assert!(parse(&fine).is_ok());
    }

    #[test]
    fn quoted_angle_brackets_do_not_end_a_tag() {
        let el = parse("<a v=\"1>2\"><b/></a>").unwrap();
        assert_eq!(el.attr("v"), Some("1>2"));
        assert_eq!(el.kids.len(), 1);
    }
}
