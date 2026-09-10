//! Harmony-format demultiplexing for local `gpt-oss` servers.
//!
//! `gpt-oss` speaks `OpenAI`'s Harmony format: every message it writes is
//! wrapped in control tokens naming a channel — `analysis` for the
//! chain-of-thought, `final` for the answer, `commentary` for tool calls and
//! preambles. Hosted backends parse that server-side and hand back the usual
//! `content` / `reasoning` / `tool_calls` fields. Several local runtimes
//! (`mlx_lm.server` among them) do not: they stream the raw template text
//! through `content`, so the transcript fills with
//! `<|channel|>analysis<|message|>…` and the *next* request is rejected
//! outright — the server refuses to re-tokenize its own framing:
//!
//! ```text
//! request failed (404 Not Found): You have passed a message containing
//! <|channel|> tags in the content field.
//! ```
//!
//! [`Demux`] splits the channels apart as deltas arrive, so nothing framed
//! ever reaches the transcript; [`sanitize_history`] repairs assistant turns
//! recorded before that existed, which is what unwedges a session already
//! carrying framed text. Both are wired in for local endpoints only
//! (`OpenRouter::is_local`) — a hosted model quoting a `<|channel|>` tag is
//! discussing Harmony, not speaking it.

use super::{ChatMessage, ToolCall};

/// Control tokens that can *open* a Harmony stream. Content that does not
/// start with one is passed through untouched, so a local model that writes
/// about the format mid-answer keeps its text.
const OPENERS: [&str; 2] = ["<|start|>", "<|channel|>"];

/// Framing that marks already-recorded content as raw Harmony text.
const FRAMING: [&str; 3] = ["<|channel|>", "<|message|>", "<|start|>"];

/// One demultiplexed span of a Harmony stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Visible answer text: the `final` channel, plus `commentary`
    /// preambles, which Harmony intends the user to see.
    Content(String),
    /// Chain-of-thought (`analysis`): shown separately, never replayed.
    Reasoning(String),
    /// A `commentary to=functions.<name>` call the runtime left unparsed.
    ToolCall { name: String, arguments: String },
}

/// Whether the stream is framed, decided from its first non-blank bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    /// Too few bytes to tell an opener from ordinary text.
    #[default]
    Undecided,
    /// Not Harmony: every delta is visible content, verbatim.
    PassThrough,
    /// Harmony: control tokens route the text.
    Harmony,
}

/// Where the text currently being read belongs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Sink {
    #[default]
    Content,
    Reasoning,
    /// Between `<|channel|>` and `<|message|>`: the channel name, an
    /// optional `to=` recipient, and the `<|constrain|>` type.
    Header,
    /// A tool call's arguments, buffered until `<|call|>` closes them.
    Tool,
    /// Framing with no destination — a role name after `<|start|>`, or the
    /// gap between two messages.
    Drop,
}

/// Streaming Harmony splitter. Feed it `content` deltas with [`Demux::push`]
/// and flush the tail with [`Demux::finish`]; control tokens split across
/// delta boundaries are held back until they complete.
#[derive(Debug, Default)]
pub struct Demux {
    mode: Mode,
    /// Text not yet emitted: an unfinished control token, or everything
    /// received while the mode is still undecided.
    buf: String,
    sink: Sink,
    header: String,
    name: String,
    args: String,
}

impl Demux {
    /// A splitter that knows its input is framed, for repairing recorded
    /// text rather than sniffing a live stream.
    fn framed() -> Self {
        Self {
            mode: Mode::Harmony,
            ..Self::default()
        }
    }

    /// Feed one streamed `content` delta.
    pub fn push(&mut self, chunk: &str) -> Vec<Piece> {
        self.buf.push_str(chunk);
        if self.mode == Mode::Undecided {
            self.mode = detect(&self.buf);
        }
        match self.mode {
            // Still ambiguous: hold the bytes until the next delta decides.
            Mode::Undecided => Vec::new(),
            Mode::PassThrough => {
                let text = std::mem::take(&mut self.buf);
                vec![Piece::Content(text)]
            }
            Mode::Harmony => self.drain(),
        }
    }

    /// Flush whatever the last delta held back. A stream that ended
    /// mid-token drops the partial framing; an unterminated tool call is
    /// still reported, since its arguments are all the caller will get.
    pub fn finish(&mut self) -> Vec<Piece> {
        if self.mode == Mode::Undecided {
            self.mode = Mode::PassThrough;
        }
        let text = std::mem::take(&mut self.buf);
        if self.mode == Mode::PassThrough {
            return if text.is_empty() {
                Vec::new()
            } else {
                vec![Piece::Content(text)]
            };
        }
        let mut out = Vec::new();
        // A tail that opened a token but never closed it is framing.
        if !text.starts_with("<|") || text.contains("|>") {
            self.emit(&mut out, text);
        }
        if self.sink == Sink::Tool {
            self.close_message(&mut out);
        }
        out
    }

    /// Consume every complete token and span currently buffered.
    fn drain(&mut self) -> Vec<Piece> {
        let mut out = Vec::new();
        loop {
            let Some(open) = self.buf.find('<') else {
                let text = std::mem::take(&mut self.buf);
                self.emit(&mut out, text);
                break;
            };
            let text: String = self.buf.drain(..open).collect();
            self.emit(&mut out, text);
            if !self.buf.starts_with("<|") {
                if self.buf.len() < 2 {
                    break; // could still grow into a token
                }
                // A lone `<` in message text.
                let stray: String = self.buf.drain(..1).collect();
                self.emit(&mut out, stray);
                continue;
            }
            let Some(end) = self.buf.find("|>") else {
                break; // token split across deltas
            };
            let token: String = self.buf.drain(..end + 2).collect();
            self.apply(&mut out, &token);
        }
        out
    }

    /// Route a span of plain text to whatever the last control token opened.
    fn emit(&mut self, out: &mut Vec<Piece>, text: String) {
        if text.is_empty() {
            return;
        }
        match self.sink {
            Sink::Content => out.push(Piece::Content(text)),
            Sink::Reasoning => out.push(Piece::Reasoning(text)),
            Sink::Header => self.header.push_str(&text),
            Sink::Tool => self.args.push_str(&text),
            Sink::Drop => {}
        }
    }

    fn apply(&mut self, out: &mut Vec<Piece>, token: &str) {
        match token {
            "<|start|>" => {
                self.sink = Sink::Drop;
                self.header.clear();
            }
            "<|channel|>" => {
                self.sink = Sink::Header;
                self.header.clear();
            }
            "<|message|>" => self.open_message(),
            "<|end|>" | "<|return|>" | "<|call|>" => self.close_message(out),
            // `<|constrain|>` and anything unrecognized is framing, not text.
            _ => {}
        }
    }

    /// A `<|message|>` closes the header: decide where its body goes.
    fn open_message(&mut self) {
        let header = std::mem::take(&mut self.header);
        if let Some(target) = header
            .split_whitespace()
            .find_map(|word| word.strip_prefix("to="))
        {
            // `to=functions.get_weather` — the tool name is the last segment.
            self.name = target.rsplit('.').next().unwrap_or(target).to_string();
            self.args.clear();
            self.sink = Sink::Tool;
            return;
        }
        // Unknown channels are shown rather than hidden: swallowing an
        // answer is worse than leaking a stray label.
        let channel = header.split_whitespace().next().unwrap_or_default();
        self.sink = if channel == "analysis" {
            Sink::Reasoning
        } else {
            Sink::Content
        };
    }

    fn close_message(&mut self, out: &mut Vec<Piece>) {
        if self.sink == Sink::Tool {
            out.push(Piece::ToolCall {
                name: std::mem::take(&mut self.name),
                arguments: std::mem::take(&mut self.args),
            });
        }
        self.sink = Sink::Drop;
        self.header.clear();
    }
}

/// Decide from the buffered head whether the stream is framed.
fn detect(buf: &str) -> Mode {
    let text = buf.trim_start();
    if OPENERS.iter().any(|opener| text.starts_with(opener)) {
        Mode::Harmony
    } else if text.is_empty() || OPENERS.iter().any(|opener| opener.starts_with(text)) {
        Mode::Undecided
    } else {
        Mode::PassThrough
    }
}

/// Recover the visible answer from content a local runtime recorded as raw
/// Harmony text. `None` when there is no framing to strip — the case for
/// every backend that parses its own format.
#[must_use]
pub fn sanitize(content: &str) -> Option<String> {
    if !FRAMING.iter().any(|token| content.contains(token)) {
        return None;
    }
    let mut demux = Demux::framed();
    let mut pieces = demux.push(content);
    pieces.extend(demux.finish());
    let mut text = String::new();
    for piece in pieces {
        if let Piece::Content(part) = piece {
            text.push_str(&part);
        }
    }
    Some(text.trim().to_string())
}

/// Repair a replayed history: assistant turns holding raw Harmony framing
/// are rewritten to their visible text, since a local server rejects the
/// whole request over one framed message. `None` when nothing needs
/// repairing, so the usual path clones nothing.
#[must_use]
pub fn sanitize_history(messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    let framed = |m: &ChatMessage| m.role == "assistant" && sanitize(&m.content).is_some();
    if !messages.iter().any(framed) {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|m| match sanitize(&m.content) {
                Some(text) if m.role == "assistant" => ChatMessage {
                    content: text,
                    ..m.clone()
                },
                _ => m.clone(),
            })
            .collect(),
    )
}

/// Build the tool call a runtime should have parsed out of `commentary`.
/// The id is synthesized: the framing carries no call id, and the server
/// only ever sees it again as the `tool_call_id` we echo back.
#[must_use]
pub fn synthetic_call(index: usize, name: String, arguments: String) -> ToolCall {
    ToolCall {
        id: format!("harmony-{index}"),
        name,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split `text` at every char boundary and feed it through one demux,
    /// proving control tokens survive arbitrary delta boundaries.
    fn stream(text: &str, chunk: usize) -> Vec<Piece> {
        let mut demux = Demux::default();
        let mut out = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            let mut take = chunk.min(rest.len());
            while !rest.is_char_boundary(take) {
                take += 1;
            }
            let (head, tail) = rest.split_at(take);
            out.extend(demux.push(head));
            rest = tail;
        }
        out.extend(demux.finish());
        out
    }

    fn joined(pieces: &[Piece]) -> (String, String) {
        let mut content = String::new();
        let mut reasoning = String::new();
        for piece in pieces {
            match piece {
                Piece::Content(t) => content.push_str(t),
                Piece::Reasoning(t) => reasoning.push_str(t),
                Piece::ToolCall { .. } => {}
            }
        }
        (content, reasoning)
    }

    const TURN: &str = "<|channel|>analysis<|message|>User says \"hello\". Just greeting?<|end|>\
<|start|>assistant<|channel|>final<|message|>Hello.<|return|>";

    #[test]
    fn framed_turn_splits_at_every_delta_boundary() {
        for chunk in [1, 2, 3, 7, 11, TURN.len()] {
            let (content, reasoning) = joined(&stream(TURN, chunk));
            assert_eq!(content, "Hello.", "chunk {chunk}");
            assert_eq!(
                reasoning, "User says \"hello\". Just greeting?",
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn unframed_streams_pass_through_untouched() {
        // A model explaining the format is writing text, not framing it.
        let prose = "Harmony wraps text in <|channel|>analysis<|message|> tags — a < is fine too.";
        for chunk in [1, 4, prose.len()] {
            let (content, reasoning) = joined(&stream(prose, chunk));
            assert_eq!(content, prose, "chunk {chunk}");
            assert!(reasoning.is_empty(), "chunk {chunk}");
        }
        assert_eq!(sanitize("plain answer"), None);
    }

    #[test]
    fn unparsed_commentary_becomes_a_tool_call() {
        let turn = "<|channel|>analysis<|message|>need the file<|end|><|start|>assistant\
<|channel|>commentary to=functions.read_file <|constrain|>json<|message|>\
{\"name\":\"a.txt\"}<|call|>";
        let pieces = stream(turn, 5);
        assert!(pieces.contains(&Piece::ToolCall {
            name: "read_file".into(),
            arguments: "{\"name\":\"a.txt\"}".into(),
        }));
        let (content, reasoning) = joined(&pieces);
        assert!(content.is_empty());
        assert_eq!(reasoning, "need the file");
        assert_eq!(
            synthetic_call(2, "read_file".into(), "{}".into()).id,
            "harmony-2"
        );
    }

    #[test]
    fn commentary_preambles_stay_visible() {
        let turn = "<|channel|>commentary<|message|>Reading the file first.<|end|>";
        let (content, _) = joined(&stream(turn, 6));
        assert_eq!(content, "Reading the file first.");
    }

    #[test]
    fn truncated_streams_lose_framing_not_text() {
        // Cut mid-token: the partial tag is framing and is dropped.
        let (content, _) = joined(&stream("<|channel|>final<|message|>Half<|chan", 3));
        assert_eq!(content, "Half");
        // Cut mid-call: the arguments are all the caller will ever get.
        let pieces = stream(
            "<|channel|>commentary to=functions.grep<|message|>{\"q\":\"x\"}",
            4,
        );
        assert!(pieces.contains(&Piece::ToolCall {
            name: "grep".into(),
            arguments: "{\"q\":\"x\"}".into(),
        }));
    }

    #[test]
    fn recorded_framing_is_repaired_before_replay() {
        // The exact shape a local mlx server records, which it then refuses
        // to accept back ("passed a message containing <|channel|> tags").
        assert_eq!(sanitize(TURN).unwrap(), "Hello.");
        let history = vec![
            ChatMessage::text("user", "hello"),
            ChatMessage::text("assistant", TURN),
            ChatMessage::text("user", "okay then"),
        ];
        let repaired = sanitize_history(&history).expect("framing needs repair");
        assert_eq!(repaired[1].content, "Hello.");
        assert!(!repaired[1].content.contains("<|"));
        assert_eq!(repaired[0].content, "hello");
        assert_eq!(repaired[2].content, "okay then");
        // A user quoting the tags keeps them; only the model's own framing
        // is rewritten, and a clean history is left alone entirely.
        let quoted = vec![ChatMessage::text("user", "why does <|channel|> appear?")];
        assert!(sanitize_history(&quoted).is_none());
        assert!(sanitize_history(&[ChatMessage::text("assistant", "hi")]).is_none());
    }
}
