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
//! carrying framed text. A recorded tool call wedges a session the same way
//! — the server re-parses its arguments and fails the request with
//! `404 … Unterminated string` — so unparsable arguments are repaired there
//! too, and a truncated call never reaches the transcript at all. Both are wired in for local endpoints only
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
    /// mid-token drops the partial framing; a tool call whose arguments
    /// never finished is dropped too, rather than recorded as a call the
    /// server will refuse to replay.
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
            // Inside a header it still separates words: gpt-oss writes the
            // recipient without a trailing space (`to=functions.batch`
            // `<|constrain|>json`), and fusing those yields the tool name
            // `batchjson`, which no catalog has.
            _ => {
                if self.sink == Sink::Header {
                    self.header.push(' ');
                }
            }
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
            let name = std::mem::take(&mut self.name);
            let arguments = std::mem::take(&mut self.args);
            // Arguments cut off mid-generation are worse than useless: the
            // tool rejects them now and the server rejects the *recorded*
            // call forever after, so drop the call and let the turn end.
            if let Some(arguments) = replayable(&arguments) {
                out.push(Piece::ToolCall { name, arguments });
            }
        }
        self.sink = Sink::Drop;
        self.header.clear();
    }
}

/// Arguments in the form a local runtime will accept back. `mlx_lm.server`
/// re-renders every recorded tool call through the Harmony template and
/// `json.loads` its arguments, so one unparsable call fails *every* later
/// request — `404 … Unterminated string starting at: line 1 column 99` —
/// wedging the session exactly the way framed `content` used to. Empty
/// arguments are a no-arg call and become `{}`; anything else that does not
/// parse is unrecoverable, and `None` says so.
fn replayable(arguments: &str) -> Option<String> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Some("{}".to_string());
    }
    serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    Some(trimmed.to_string())
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
/// are rewritten to their visible text, and recorded tool calls whose
/// arguments do not parse are reduced to `{}` — a local server rejects the
/// whole request over either one. `None` when nothing needs repairing, so
/// the usual path clones nothing.
#[must_use]
pub fn sanitize_history(messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    let broken = |m: &ChatMessage| {
        m.role == "assistant" && (sanitize(&m.content).is_some() || repair_calls(m).is_some())
    };
    if !messages.iter().any(broken) {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|m| {
                if m.role != "assistant" {
                    return m.clone();
                }
                ChatMessage {
                    content: sanitize(&m.content).unwrap_or_else(|| m.content.clone()),
                    tool_calls: repair_calls(m).or_else(|| m.tool_calls.clone()),
                    ..m.clone()
                }
            })
            .collect(),
    )
}

/// Rewrite a recorded turn's unparsable tool arguments. The call itself is
/// kept — dropping it would orphan the `tool` result that answers its id,
/// which the server rejects just as hard — so the arguments become `{}` and
/// the already-recorded result stands. `None` when every call replays.
fn repair_calls(message: &ChatMessage) -> Option<Vec<ToolCall>> {
    let calls = message.tool_calls.as_ref()?;
    if calls
        .iter()
        .all(|call| replayable(&call.arguments).as_deref() == Some(call.arguments.as_str()))
    {
        return None;
    }
    Some(
        calls
            .iter()
            .map(|call| ToolCall {
                arguments: replayable(&call.arguments).unwrap_or_else(|| "{}".to_string()),
                ..call.clone()
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
    fn constrain_token_does_not_fuse_into_the_tool_name() {
        // gpt-oss writes the recipient with no trailing space, so the
        // constrain type used to fuse onto it and produce `batchjson`.
        let turn = "<|channel|>commentary to=functions.batch<|constrain|>json<|message|>\
{\"calls\":[]}<|call|>";
        for chunk in [1, 5, turn.len()] {
            assert!(
                stream(turn, chunk).contains(&Piece::ToolCall {
                    name: "batch".into(),
                    arguments: "{\"calls\":[]}".into(),
                }),
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn truncated_arguments_never_become_a_call() {
        // Generation cut mid-string: the tool would reject these now and the
        // server would reject the recorded call on every later request.
        let turn = "<|channel|>commentary to=functions.batch <|constrain|>json<|message|>\
{\"calls\":[{\"tool\":\"scripts\",\"action\":\"write\",\"path\":\"scripts";
        let pieces = stream(turn, 6);
        assert!(!pieces.iter().any(|p| matches!(p, Piece::ToolCall { .. })));
        // A no-arg call is not truncated, it is empty — that one replays.
        let empty = "<|channel|>commentary to=functions.ls <|constrain|>json<|message|><|call|>";
        assert!(stream(empty, 4).contains(&Piece::ToolCall {
            name: "ls".into(),
            arguments: "{}".into(),
        }));
    }

    #[test]
    fn recorded_unparsable_arguments_are_repaired_before_replay() {
        let call = |arguments: &str| ChatMessage {
            role: "assistant".into(),
            tool_calls: Some(vec![ToolCall {
                id: "harmony-0".into(),
                name: "batch".into(),
                arguments: arguments.into(),
            }]),
            ..Default::default()
        };
        // A session already carrying a truncated call unwedges: the call is
        // kept so its recorded result still has a parent, with empty args.
        let history = vec![
            call("{\"calls\":[{\"tool\":\"scripts"),
            ChatMessage {
                role: "tool".into(),
                content: "error".into(),
                tool_call_id: Some("harmony-0".into()),
                ..Default::default()
            },
        ];
        let repaired = sanitize_history(&history).expect("bad arguments need repair");
        let calls = repaired[0].tool_calls.as_ref().expect("call is kept");
        assert_eq!(calls[0].arguments, "{}");
        assert_eq!(calls[0].id, "harmony-0");
        assert_eq!(repaired[1].tool_call_id.as_deref(), Some("harmony-0"));
        // Valid arguments are left exactly as recorded.
        assert!(sanitize_history(&[call("{\"calls\":[]}")]).is_none());
    }

    /// Verbatim captures from `mlx_lm.server` 0.32.0 streaming
    /// `gpt-oss-20b`, one request apart. The first call spaces the
    /// recipient off from `<|constrain|>` and the second does not — and
    /// neither is closed by `<|call|>`, because the server stops *on* that
    /// token and never emits it, so every call resolves in `finish`.
    #[test]
    fn real_mlx_captures_yield_the_same_call_either_way() {
        const SPACED: &str = "<|channel|>analysis<|message|>We need to search web. Use \
functions.search.<|end|><|start|>assistant<|channel|>commentary to=functions.search \
<|constrain|>json<|message|>{\"query\":\"latest version of Codex CLI tool\",\"mode\":\"news\"}";
        const FUSED: &str = "<|channel|>analysis<|message|>We need to search for Codex CLI. \
Let's do that.<|end|><|start|>assistant<|channel|>commentary to=functions.search\
<|constrain|>json<|message|>{\"query\":\"latest version of Codex CLI tool\",\"mode\":\"news\"}";
        for (label, turn) in [("spaced", SPACED), ("fused", FUSED)] {
            for chunk in [1, 9, turn.len()] {
                let pieces = stream(turn, chunk);
                assert!(
                    pieces.contains(&Piece::ToolCall {
                        name: "search".into(),
                        arguments:
                            "{\"query\":\"latest version of Codex CLI tool\",\"mode\":\"news\"}"
                                .into(),
                    }),
                    "{label} capture, chunk {chunk}: {pieces:?}"
                );
                // The arguments went to the call, not to the transcript:
                // this is the turn that used to render as a bare `{"query"
                // …}` blob with no search behind it.
                let (content, reasoning) = joined(&pieces);
                assert!(content.is_empty(), "{label} chunk {chunk}: {content:?}");
                assert!(!reasoning.contains("\"query\""), "{label} chunk {chunk}");
            }
        }
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
