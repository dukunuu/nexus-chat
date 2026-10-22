//! A lenient HTML/XML tokenizer plus entity decoding, shared by every
//! markup converter (HTML, SVG, ODF, EPUB, mail bodies). It never fails: bad
//! markup degrades to text, and the iterator always terminates.

use std::borrow::Cow;

/// One lexical piece of a markup document. Names are lowercased; `attrs` is
/// the raw attribute text (look values up with [`attr`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok<'a> {
    Text(&'a str),
    Start {
        name: String,
        attrs: &'a str,
        self_closing: bool,
    },
    End {
        name: String,
    },
}

/// Elements whose body is raw text (no nested markup).
const RAW_TEXT: &[&str] = &["script", "style"];

pub struct Tokens<'a> {
    src: &'a str,
    pos: usize,
    raw_until: Option<&'static str>,
}

impl<'a> Tokens<'a> {
    pub const fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            raw_until: None,
        }
    }
}

/// Byte offset of the first ASCII-case-insensitive `needle` in `hay`.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        loop {
            if self.pos >= self.src.len() {
                return None;
            }
            let rest = &self.src[self.pos..];
            if let Some(tag) = self.raw_until {
                let close = format!("</{tag}");
                self.raw_until = None;
                return match find_ci(rest, &close) {
                    Some(0) => continue,
                    Some(at) => {
                        self.pos += at;
                        Some(Tok::Text(&rest[..at]))
                    }
                    None => {
                        self.pos = self.src.len();
                        Some(Tok::Text(rest))
                    }
                };
            }
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                self.pos += end;
                return Some(Tok::Text(&rest[..end]));
            }
            if let Some(body) = rest.strip_prefix("<!--") {
                self.pos += body.find("-->").map_or(rest.len(), |i| 4 + i + 3);
                continue;
            }
            if let Some(body) = rest.strip_prefix("<![CDATA[") {
                let end = body.find("]]>").unwrap_or(body.len());
                self.pos += 9 + end + if end < body.len() { 3 } else { 0 };
                return Some(Tok::Text(&body[..end]));
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                self.pos += rest.find('>').map_or(rest.len(), |i| i + 1);
                continue;
            }
            if let Some(body) = rest.strip_prefix("</") {
                if !body.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    self.pos += 2;
                    return Some(Tok::Text("</"));
                }
                let gt = body.find('>').map_or(body.len(), |i| i + 1);
                let name = body[..gt]
                    .trim_end_matches('>')
                    .split(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                self.pos += 2 + gt;
                return Some(Tok::End { name });
            }
            let body = &rest[1..];
            if !body.starts_with(|c: char| c.is_ascii_alphabetic()) {
                self.pos += 1;
                return Some(Tok::Text("<"));
            }
            // Start tag: name, then attributes up to the unquoted `>`.
            let name_end = body
                .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .unwrap_or(body.len());
            let name = body[..name_end].to_ascii_lowercase();
            let mut quote: Option<char> = None;
            let mut gt = None;
            for (i, c) in body[name_end..].char_indices() {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (None, '"' | '\'') => quote = Some(c),
                    (None, '>') => {
                        gt = Some(name_end + i);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(gt) = gt else {
                // Unterminated tag: the remainder is text.
                self.pos = self.src.len();
                return Some(Tok::Text(rest));
            };
            let raw_attrs = &body[name_end..gt];
            let self_closing = raw_attrs.trim_end().ends_with('/');
            let attrs = raw_attrs.trim().trim_end_matches('/').trim_end();
            self.pos += 1 + gt + 1;
            if !self_closing && let Some(tag) = RAW_TEXT.iter().find(|t| **t == name) {
                self.raw_until = Some(tag);
            }
            return Some(Tok::Start {
                name,
                attrs,
                self_closing,
            });
        }
    }
}

/// The (entity-decoded) value of attribute `name` in a raw attribute string.
pub fn attr(attrs: &str, name: &str) -> Option<String> {
    let b = attrs.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        let key = &attrs[key_start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = "";
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let start = i + 1;
                i = start;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = &attrs[start..i.min(b.len())];
                i = (i + 1).min(b.len());
            } else {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = &attrs[start..i];
            }
        }
        if !key.is_empty() && key.eq_ignore_ascii_case(name) {
            return Some(decode_entities(value).into_owned());
        }
        if key.is_empty() {
            i += 1;
        }
    }
    None
}

/// Decode HTML/XML character references. Unknown names are left as written.
pub fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let decoded = tail
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| entity(&tail[..end]).map(|c| (c, end + 1)));
        if let Some((text, used)) = decoded {
            out.push_str(&text);
            rest = &tail[used..];
        } else {
            out.push('&');
            rest = tail;
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn entity(name: &str) -> Option<Cow<'static, str>> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        let c = char::from_u32(code).filter(|c| *c != '\0')?;
        // A non-breaking space is just a space once text is extracted.
        return Some(Cow::Owned(if c == '\u{a0}' { ' ' } else { c }.to_string()));
    }
    let s = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" | "ensp" | "emsp" | "thinsp" => " ",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "hellip" => "\u{2026}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "sbquo" => "\u{201a}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "bdquo" => "\u{201e}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "bull" => "\u{2022}",
        "middot" => "\u{b7}",
        "deg" => "\u{b0}",
        "plusmn" => "\u{b1}",
        "times" => "\u{d7}",
        "divide" => "\u{f7}",
        "euro" => "\u{20ac}",
        "pound" => "\u{a3}",
        "yen" => "\u{a5}",
        "cent" => "\u{a2}",
        "sect" => "\u{a7}",
        "para" => "\u{b6}",
        "frac12" => "\u{bd}",
        "frac14" => "\u{bc}",
        "frac34" => "\u{be}",
        "larr" => "\u{2190}",
        "rarr" => "\u{2192}",
        "uarr" => "\u{2191}",
        "darr" => "\u{2193}",
        "harr" => "\u{2194}",
        "eacute" => "\u{e9}",
        "egrave" => "\u{e8}",
        "agrave" => "\u{e0}",
        "aacute" => "\u{e1}",
        "iacute" => "\u{ed}",
        "oacute" => "\u{f3}",
        "uacute" => "\u{fa}",
        "auml" => "\u{e4}",
        "ouml" => "\u{f6}",
        "uuml" => "\u{fc}",
        "szlig" => "\u{df}",
        "ccedil" => "\u{e7}",
        "ntilde" => "\u{f1}",
        _ => return None,
    };
    Some(Cow::Borrowed(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_tags_text_comments_and_raw_text() {
        let toks: Vec<Tok> =
            Tokens::new("<P class=\"a\">hi<!-- x --><script>if (a<b) {}</script></P>").collect();
        assert_eq!(
            toks,
            vec![
                Tok::Start {
                    name: "p".into(),
                    attrs: "class=\"a\"",
                    self_closing: false
                },
                Tok::Text("hi"),
                Tok::Start {
                    name: "script".into(),
                    attrs: "",
                    self_closing: false
                },
                Tok::Text("if (a<b) {}"),
                Tok::End {
                    name: "script".into()
                },
                Tok::End { name: "p".into() },
            ]
        );
    }

    #[test]
    fn stray_angle_brackets_and_truncated_tags_terminate() {
        let toks: Vec<Tok> = Tokens::new("a < b <div class=\"x").collect();
        assert!(matches!(toks.last(), Some(Tok::Text(t)) if t.starts_with("<div")));
        // Pathological input must still end.
        assert!(Tokens::new("<<<<</</<!<?").count() < 40);
    }

    #[test]
    fn attr_reads_quoted_unquoted_and_decodes() {
        let a = "href='x?a=1&amp;b=2' TITLE=\"t > u\" data-x=y disabled";
        assert_eq!(attr(a, "href").as_deref(), Some("x?a=1&b=2"));
        assert_eq!(attr(a, "title").as_deref(), Some("t > u"));
        assert_eq!(attr(a, "data-x").as_deref(), Some("y"));
        assert_eq!(attr(a, "disabled").as_deref(), Some(""));
        assert!(attr(a, "missing").is_none());
    }

    #[test]
    fn entities_decode_named_numeric_and_unknown() {
        assert_eq!(
            decode_entities("a&nbsp;b &amp; &#65;&#x42; &bogus; &"),
            "a b & AB &bogus; &"
        );
        assert!(matches!(decode_entities("plain"), Cow::Borrowed(_)));
    }
}
