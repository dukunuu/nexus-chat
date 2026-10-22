//! HTML/XHTML to readable Markdown: headings, lists, tables, code and quotes
//! survive; scripts, styles, SVG, inline `data:` blobs and layout chrome do
//! not. Chat-export pages (Discord, Telegram) keep their author / time /
//! message structure as `[time] author: message` lines.
//!
//! The output is one block per line with no blank lines, so a paginated read
//! is not mostly whitespace and an embedding chunk is mostly signal.

use std::fmt::Write as _;

use super::markup::{Tok, Tokens, attr, decode_entities};

/// Elements dropped together with everything inside them.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "iframe", "canvas", "object", "audio",
    "video", "select", "button",
];
/// Elements with no end tag.
const VOID: &[&str] = &[
    "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "source", "track", "wbr",
    "param",
];
/// Elements that start and end a line.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "li",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "form",
    "fieldset",
    "details",
    "summary",
    "address",
    "center",
    "tr",
    "ul",
    "ol",
    "caption",
];

/// Class-name vocabulary of a chat-export format.
struct Profile {
    author: &'static [&'static str],
    time: &'static [&'static str],
    body: &'static [&'static str],
    message: &'static [&'static str],
}

const DISCORD: Profile = Profile {
    author: &["chatlog__author"],
    time: &["chatlog__timestamp", "chatlog__short-timestamp"],
    body: &["chatlog__content"],
    message: &["chatlog__message-container", "chatlog__message-group"],
};

const TELEGRAM: Profile = Profile {
    author: &["from_name"],
    time: &["date"],
    body: &["text"],
    message: &["message"],
};

fn profile_for(html: &str) -> Option<&'static Profile> {
    if html.contains("chatlog__") {
        Some(&DISCORD)
    } else if html.contains("from_name") && html.contains("clearfix") {
        Some(&TELEGRAM)
    } else {
        None
    }
}

fn has_class(attrs: &str, names: &[&str]) -> bool {
    attr(attrs, "class").is_some_and(|c| c.split_whitespace().any(|t| names.contains(&t)))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Capture {
    Author,
    Time,
}

struct Open {
    name: String,
    depth: usize,
    kind: OpenKind,
}

enum OpenKind {
    Capture(Capture, Option<String>),
    Body,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Option<Vec<String>>,
    cell: Option<String>,
    nested: usize,
}

struct Conv {
    lines: Vec<String>,
    line: String,
    title: String,
    in_title: bool,
    skip: Option<(String, usize)>,
    lists: Vec<Option<usize>>,
    quote: usize,
    pre: Option<String>,
    table: Option<Table>,
    link: Option<(String, usize)>,
    profile: Option<&'static Profile>,
    open: Option<Open>,
    capture_buf: String,
    author: String,
    time: String,
    pending_prefix: bool,
}

impl Conv {
    fn new(profile: Option<&'static Profile>) -> Self {
        Self {
            lines: Vec::new(),
            line: String::new(),
            title: String::new(),
            in_title: false,
            skip: None,
            lists: Vec::new(),
            quote: 0,
            pre: None,
            table: None,
            link: None,
            profile,
            open: None,
            capture_buf: String::new(),
            author: String::new(),
            time: String::new(),
            pending_prefix: false,
        }
    }

    /// Finish the current line.
    fn flush(&mut self) {
        // Keep leading indentation (list nesting); drop trailing space.
        let text = self.line.trim_end();
        if !text.trim().is_empty() {
            let mut out = String::new();
            if self.quote > 0 {
                out.push_str(&"> ".repeat(self.quote));
            }
            out.push_str(text);
            self.lines.push(out);
        }
        self.line.clear();
    }

    fn prefix(&self) -> String {
        match (self.time.is_empty(), self.author.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("[{}] ", self.time),
            (true, false) => format!("{}: ", self.author),
            (false, false) => format!("[{}] {}: ", self.time, self.author),
        }
    }

    /// Append inline text, collapsing whitespace.
    fn push_inline(&mut self, text: &str) {
        let capturing = self
            .open
            .as_ref()
            .is_some_and(|o| matches!(o.kind, OpenKind::Capture(..)) && o.depth > 0);
        let target: &mut String = match &mut self.table {
            Some(Table {
                cell: Some(cell), ..
            }) => cell,
            _ if capturing => &mut self.capture_buf,
            _ => &mut self.line,
        };
        let leading = text.starts_with(char::is_whitespace);
        let mut first = true;
        for word in text.split_whitespace() {
            let word = if word.len() > 500 && word.bytes().all(is_base64) {
                "[data omitted]"
            } else {
                word
            };
            let need_space = if first { leading } else { true };
            if need_space && !target.is_empty() && !target.ends_with(' ') {
                target.push(' ');
            }
            target.push_str(word);
            first = false;
        }
        if text.ends_with(char::is_whitespace) && !target.is_empty() && !target.ends_with(' ') {
            target.push(' ');
        }
    }

    fn block(&mut self) {
        if let Some(t) = &mut self.table {
            if let Some(cell) = &mut t.cell
                && !cell.ends_with(' ')
                && !cell.is_empty()
            {
                cell.push(' ');
            }
            return;
        }
        self.flush();
    }

    fn text(&mut self, raw: &str) {
        if self.skip.is_some() {
            return;
        }
        let decoded = decode_entities(raw);
        if self.in_title {
            self.title.push_str(&decoded);
            return;
        }
        if let Some(pre) = &mut self.pre {
            pre.push_str(&decoded);
            return;
        }
        if decoded.is_empty() {
            return;
        }
        if self.pending_prefix
            && !decoded.trim().is_empty()
            && self.open.as_ref().is_none_or(|o| o.depth == 0)
            && self.table.is_none()
        {
            self.flush();
            let p = self.prefix();
            self.line.push_str(&p);
            self.pending_prefix = false;
        }
        self.push_inline(&decoded);
    }

    #[allow(clippy::too_many_lines)]
    fn start(&mut self, name: &str, attrs: &str, self_closing: bool) {
        if let Some((tag, depth)) = &mut self.skip {
            if *tag == name && !self_closing {
                *depth += 1;
            }
            return;
        }
        if SKIP.contains(&name) && !self_closing {
            self.skip = Some((name.to_string(), 1));
            return;
        }
        if name == "title" {
            self.in_title = true;
            return;
        }
        if self.pre.is_some() {
            return;
        }
        match name {
            "br" => {
                if let Some(t) = &mut self.table {
                    if let Some(cell) = &mut t.cell
                        && !cell.ends_with(' ')
                    {
                        cell.push(' ');
                    }
                } else {
                    self.flush();
                }
            }
            "hr" => {
                self.block();
                if self.table.is_none() {
                    self.lines.push("---".to_string());
                }
            }
            "img" => {
                let avatar = attr(attrs, "class").is_some_and(|c| c.contains("avatar"));
                let alt = if avatar {
                    String::new()
                } else {
                    attr(attrs, "alt").unwrap_or_default()
                };
                let alt = alt.trim();
                if !alt.is_empty() {
                    if alt.contains(char::is_whitespace) {
                        self.text(&format!(" [image: {alt}] "));
                    } else {
                        self.text(&format!(" {alt} "));
                    }
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block();
                if self.table.is_none() {
                    let level = usize::from(name.as_bytes()[1] - b'0');
                    self.line.push_str(&"#".repeat(level));
                    self.line.push(' ');
                }
            }
            "ul" | "ol" => {
                self.block();
                self.lists.push((name == "ol").then_some(0));
            }
            "li" => {
                self.block();
                if self.table.is_none() {
                    let depth = self.lists.len().saturating_sub(1);
                    self.line.push_str(&"  ".repeat(depth));
                    match self.lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            let _ = write!(self.line, "{n}. ");
                        }
                        _ => self.line.push_str("- "),
                    }
                }
            }
            "blockquote" => {
                self.block();
                self.quote += 1;
            }
            "pre" => {
                self.block();
                self.pre = Some(String::new());
            }
            "code" | "kbd" | "samp" | "tt" => self.push_inline(" `"),
            "a" => {
                if let Some(href) = attr(attrs, "href") {
                    self.link = Some((href, self.line.len()));
                }
            }
            "table" => {
                if let Some(t) = &mut self.table {
                    t.nested += 1;
                    if let Some(cell) = &mut t.cell
                        && !cell.ends_with(' ')
                    {
                        cell.push(' ');
                    }
                } else {
                    self.flush();
                    self.table = Some(Table::default());
                }
            }
            "tr" => {
                if let Some(t) = self.table.as_mut().filter(|t| t.nested == 0) {
                    finish_row(t);
                    t.row = Some(Vec::new());
                } else {
                    self.block();
                }
            }
            "td" | "th" => {
                if let Some(t) = self.table.as_mut().filter(|t| t.nested == 0) {
                    finish_cell(t);
                    t.cell = Some(String::new());
                }
            }
            n if BLOCK.contains(&n) => self.block(),
            _ => {}
        }
        // After the generic block handling, so a message body's `[time] author:`
        // prefix is not flushed onto its own line.
        self.chat_start(name, attrs);
    }

    fn end(&mut self, name: &str) {
        if let Some((tag, depth)) = &mut self.skip {
            if *tag == name {
                *depth -= 1;
                if *depth == 0 {
                    self.skip = None;
                }
            }
            return;
        }
        if name == "title" {
            self.in_title = false;
            return;
        }
        if name == "pre" {
            if let Some(code) = self.pre.take() {
                self.lines.push("```".to_string());
                for l in code.trim_matches('\n').lines() {
                    self.lines.push(l.trim_end().to_string());
                }
                self.lines.push("```".to_string());
            }
            return;
        }
        if self.pre.is_some() {
            return;
        }
        self.chat_end(name);
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" => self.block(),
            "ul" | "ol" => {
                self.lists.pop();
                self.block();
            }
            "blockquote" => {
                self.block();
                self.quote = self.quote.saturating_sub(1);
            }
            "code" | "kbd" | "samp" | "tt" => {
                let target = self.cell_or_line();
                if target.ends_with(" `") {
                    target.truncate(target.len() - 2);
                } else {
                    target.push('`');
                }
            }
            "a" => {
                if let Some((href, start)) = self.link.take()
                    && self.table.is_none()
                    && self.line.get(start..).is_some_and(|s| s.trim().is_empty())
                    && href.starts_with("http")
                {
                    self.push_inline(&href);
                }
            }
            "td" | "th" => {
                if let Some(t) = self.table.as_mut().filter(|t| t.nested == 0) {
                    finish_cell(t);
                }
            }
            "tr" => {
                if let Some(t) = self.table.as_mut().filter(|t| t.nested == 0) {
                    finish_row(t);
                } else {
                    self.block();
                }
            }
            "table" => {
                let nested = self.table.as_ref().is_some_and(|t| t.nested > 0);
                if nested {
                    if let Some(t) = &mut self.table {
                        t.nested -= 1;
                    }
                } else if let Some(mut t) = self.table.take() {
                    finish_row(&mut t);
                    self.render_table(&t.rows);
                }
            }
            n if BLOCK.contains(&n) => self.block(),
            _ => {}
        }
    }

    fn cell_or_line(&mut self) -> &mut String {
        match &mut self.table {
            Some(Table { cell: Some(c), .. }) => c,
            _ => &mut self.line,
        }
    }

    fn render_table(&mut self, rows: &[Vec<String>]) {
        let rows: Vec<&Vec<String>> = rows.iter().filter(|r| !r.is_empty()).collect();
        if rows.is_empty() {
            return;
        }
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 1 {
            // A layout table: its cells are just stacked text.
            for r in rows {
                if !r[0].is_empty() {
                    self.lines.push(r[0].clone());
                }
            }
            return;
        }
        let render = |r: &Vec<String>| {
            let mut cells: Vec<String> = r.iter().map(|c| c.replace('|', "\\|")).collect();
            cells.resize(cols, String::new());
            format!("| {} |", cells.join(" | "))
        };
        self.lines.push(render(rows[0]));
        self.lines
            .push(format!("| {} |", vec!["---"; cols].join(" | ")));
        for r in &rows[1..] {
            self.lines.push(render(r));
        }
    }

    // --- chat-export structure ------------------------------------------

    fn chat_start(&mut self, name: &str, attrs: &str) {
        let Some(p) = self.profile else { return };
        if VOID.contains(&name) {
            return;
        }
        if let Some(o) = &mut self.open {
            if o.name == name {
                o.depth += 1;
            }
            return;
        }
        if has_class(attrs, p.message) {
            self.flush();
            self.pending_prefix = false;
        } else if has_class(attrs, p.author) {
            self.begin_open(name, OpenKind::Capture(Capture::Author, None));
        } else if has_class(attrs, p.time) {
            let title = attr(attrs, "title").filter(|t| !t.trim().is_empty());
            self.begin_open(name, OpenKind::Capture(Capture::Time, title));
        } else if has_class(attrs, p.body) {
            self.flush();
            let prefix = self.prefix();
            self.line.push_str(&prefix);
            self.pending_prefix = false;
            self.begin_open(name, OpenKind::Body);
        }
    }

    fn begin_open(&mut self, name: &str, kind: OpenKind) {
        if matches!(kind, OpenKind::Capture(..)) {
            self.capture_buf.clear();
        }
        self.open = Some(Open {
            name: name.to_string(),
            depth: 1,
            kind,
        });
    }

    fn chat_end(&mut self, name: &str) {
        let Some(o) = &mut self.open else { return };
        if o.name != name {
            return;
        }
        o.depth -= 1;
        if o.depth > 0 {
            return;
        }
        let Some(o) = self.open.take() else { return };
        match o.kind {
            OpenKind::Capture(Capture::Author, _) => {
                self.author = self.capture_buf.trim().to_string();
                self.pending_prefix = true;
            }
            OpenKind::Capture(Capture::Time, title) => {
                self.time = title.unwrap_or_else(|| self.capture_buf.trim().to_string());
                self.pending_prefix = true;
            }
            OpenKind::Body => self.flush(),
        }
    }

    fn finish(mut self) -> String {
        if let Some(mut t) = self.table.take() {
            finish_row(&mut t);
            self.render_table(&t.rows);
        }
        self.flush();
        let body = self.lines.join("\n");
        let title = self.title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            body
        } else if body.is_empty() {
            format!("# {title}")
        } else {
            format!("# {title}\n{body}")
        }
    }
}

fn finish_cell(t: &mut Table) {
    if let Some(cell) = t.cell.take() {
        let cell = cell.split_whitespace().collect::<Vec<_>>().join(" ");
        t.row.get_or_insert_with(Vec::new).push(cell);
    }
}

fn finish_row(t: &mut Table) {
    finish_cell(t);
    if let Some(row) = t.row.take()
        && !row.is_empty()
    {
        t.rows.push(row);
    }
}

const fn is_base64(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_')
}

/// Convert an HTML (or XHTML) document or fragment to Markdown.
pub fn to_markdown(html: &str) -> String {
    let mut conv = Conv::new(profile_for(html));
    for tok in Tokens::new(html) {
        match tok {
            Tok::Text(t) => conv.text(t),
            Tok::Start {
                name,
                attrs,
                self_closing,
            } => {
                conv.start(&name, attrs, self_closing);
                // Void elements have no end tag; `<div/>` opens and closes.
                if self_closing && !VOID.contains(&name.as_str()) {
                    conv.end(&name);
                }
            }
            Tok::End { name } => {
                if !VOID.contains(&name.as_str()) {
                    conv.end(&name);
                }
            }
        }
    }
    conv.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_scripts_styles_svg_and_data_uris_but_keeps_text() {
        let html = "<html><head><title>My page</title><style>body{}</style>\
            <script>track();</script></head><body>\n\n<h1>Title</h1>\n<p>Some   text</p>\n\n\n\
            <svg><text>vector</text></svg><img src=\"data:image/png;base64,AAAA\">\
            <p>More &amp; more</p></body></html>";
        let text = to_markdown(html);
        assert_eq!(text, "# My page\n# Title\nSome text\nMore & more");
        assert!(!text.contains("track"));
        assert!(!text.contains("vector"));
        assert!(!text.contains("base64"));
        assert!(!text.contains("\n\n"));
    }

    #[test]
    fn headings_lists_links_code_and_quotes() {
        let html = "<h2>Plan</h2><ul><li>one</li><li>two<ol><li>deep</li></ol></li></ul>\
            <p>see <a href=\"https://x.test/a\">the docs</a> or <a href=\"https://y.test\"></a></p>\
            <p>run <code>cargo test</code> now</p><blockquote>quoted</blockquote>\
            <pre>fn a() {\n    1\n}</pre>";
        let text = to_markdown(html);
        assert_eq!(
            text,
            "## Plan\n- one\n- two\n  1. deep\nsee the docs or https://y.test\n\
             run `cargo test` now\n> quoted\n```\nfn a() {\n    1\n}\n```"
        );
    }

    #[test]
    fn tables_become_pipe_tables_and_layout_tables_stack() {
        let html = "<body><p>Intro</p><table>\
            <tr><th>Model</th><th>Score</th></tr>\
            <tr><td>A</td><td>91</td></tr>\
            <tr><td>B|b</td><td>88</td></tr></table><p>Outro</p></body>";
        let text = to_markdown(html);
        assert!(text.contains("| Model | Score |"), "{text:?}");
        assert!(text.contains("| --- | --- |"), "{text:?}");
        assert!(text.contains("| A | 91 |"), "{text:?}");
        assert!(text.contains("| B\\|b | 88 |"), "{text:?}");
        assert!(
            text.starts_with("Intro\n") && text.ends_with("\nOutro"),
            "{text:?}"
        );

        let layout = to_markdown("<table><tr><td>one</td></tr><tr><td>two</td></tr></table>");
        assert_eq!(layout, "one\ntwo");
    }

    #[test]
    fn nested_tables_flatten_without_panicking() {
        let html = "<table><tr><td>outer<table><tr><td>inner</td></tr></table></td><td>x</td></tr></table>";
        let text = to_markdown(html);
        assert!(text.contains("outer inner"), "{text:?}");
        assert!(text.contains("| x |") || text.contains("x"), "{text:?}");
    }

    #[test]
    fn long_base64_text_is_omitted() {
        let blob = "A".repeat(900);
        let text = to_markdown(&format!("<p>before {blob} after</p>"));
        assert_eq!(text, "before [data omitted] after");
    }

    #[test]
    fn unterminated_markup_does_not_panic() {
        let text = to_markdown("<p>keep</p><script>var x = 1;");
        assert_eq!(text, "keep");
        let text = to_markdown("<div><p>open <b>forever");
        assert_eq!(text, "open forever");
    }

    #[test]
    fn discord_style_export_keeps_author_time_and_message() {
        let html = r##"<html><head><title>Guild - Channel</title><style>.x{}</style></head><body>
        <div class="preamble"><div class="preamble__entry">Guild</div><div class="preamble__entry">Channel</div></div>
        <div class="chatlog">
        <div class="chatlog__message-group">
          <div class="chatlog__message-container" id="chatlog__message-container-1">
            <div class="chatlog__message">
              <div class="chatlog__message-aside"><img class="chatlog__avatar" src="data:image/png;base64,AAAAAAAA" alt="Avatar"></div>
              <div class="chatlog__message-primary">
                <div class="chatlog__header">
                  <span class="chatlog__author" title="ann#0001">Ann</span>
                  <span class="chatlog__timestamp"><a href="#chatlog__message-container-1">22-Aug-23 10:02 PM</a></span>
                </div>
                <div class="chatlog__content chatlog__markdown"><span class="markdown">hello <img class="emoji" alt="&#128512;"> there</span></div>
              </div>
            </div>
          </div>
          <div class="chatlog__message-container" id="chatlog__message-container-2">
            <div class="chatlog__message">
              <div class="chatlog__message-aside"><span class="chatlog__short-timestamp" title="22-Aug-23 10:03 PM">10:03 PM</span></div>
              <div class="chatlog__message-primary">
                <div class="chatlog__content chatlog__markdown"><span class="markdown">second one</span></div>
              </div>
            </div>
          </div>
        </div>
        <div class="chatlog__message-group">
          <div class="chatlog__message-container" id="chatlog__message-container-3">
            <div class="chatlog__message"><div class="chatlog__message-primary">
              <div class="chatlog__header"><span class="chatlog__author">Bob</span><span class="chatlog__timestamp"><a>22-Aug-23 10:05 PM</a></span></div>
              <div class="chatlog__attachment"><img alt="photo.png" src="x.png"></div>
            </div></div>
          </div>
        </div>
        </div></body></html>"##;
        let text = to_markdown(html);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.contains(&"[22-Aug-23 10:02 PM] Ann: hello \u{1F600} there"),
            "{text}"
        );
        assert!(
            lines.contains(&"[22-Aug-23 10:03 PM] Ann: second one"),
            "{text}"
        );
        assert!(
            lines.contains(&"[22-Aug-23 10:05 PM] Bob: photo.png"),
            "{text}"
        );
        assert!(!text.contains("base64") && !text.contains(".x{}"), "{text}");
    }

    #[test]
    fn plain_author_spans_are_not_swallowed_outside_chat_exports() {
        let text = to_markdown("<p>By <span class=\"author\">Jane</span> today</p>");
        assert_eq!(text, "By Jane today");
    }
}
