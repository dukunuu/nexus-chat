//! Version-two, secret-free JSON-lines canvas protocol. One envelope per line.
//!
//! Version 2 renames `deck_path` to `path` (still accepted on input), adds the
//! envelope's `kind` (`deck` or `doc`) and gives docs their own `Comment` and
//! `Focus` payloads. Nexus and studio ship together; a peer that speaks another
//! version gets a clear "update" error instead of misbehaving.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// The protocol version this build speaks.
pub const VERSION: u8 = 2;

/// What a message is about: a slide deck or a flowing document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deck,
    Doc,
}

impl Kind {
    /// A `*.doc.json` file is a document; everything else is a deck.
    #[must_use]
    pub fn of_path(path: &std::path::Path) -> Self {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".doc.json"))
        {
            Self::Doc
        } else {
            Self::Deck
        }
    }
}

/// Protocol envelope; reject unsupported versions before handling a message.
/// `kind` names the document the message is about; it is absent for messages
/// that are about neither (`Theme`, `Bye`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(flatten)]
    pub message: Message,
}

/// Studio sends Hello/Comment/Reply/Focus; nexus sends Open/Focus/Theme/Activity/Bye.
///
/// `DocComment` and `DocFocus` are the Rust names of the `Comment` and `Focus`
/// messages of a `kind: "doc"` envelope: [`Line`] maps them on the wire, so a
/// peer sees `{"kind":"doc","type":"Comment",…}`. The paths are named `path` on
/// the wire; `deck_path` is still accepted on input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    Hello {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
    },
    Comment {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        thread_id: String,
        slide: String,
        block: Option<String>,
        point: (i32, i32),
        text: String,
    },
    DocComment {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        thread_id: String,
        block: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<crate::doc::feedback::Range>,
        quote: String,
        text: String,
    },
    Reply {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        thread_id: String,
        text: String,
    },
    Focus {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        slide: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocks: Vec<String>,
    },
    /// The block in view and the selected text of a doc.
    DocFocus {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<String>,
    },
    Open {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
    },
    Theme {
        colors: crate::theme_colors::ThemeColors,
    },
    Activity {
        #[serde(rename = "path", alias = "deck_path")]
        deck_path: PathBuf,
        thread_id: String,
        #[serde(flatten)]
        state: ActivityState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Bye,
}

/// Additive thread activity; unknown future states are ignored by studio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActivityState {
    Queued {
        reason: WaitingReason,
    },
    Working,
    Progress,
    Done,
    Failed,
    Stopped,
    #[serde(other)]
    Unknown,
}

/// Why a received comment cannot start yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    Draft,
    Streaming,
    NoModel,
    #[serde(other)]
    Unknown,
}

/// A result retained only for a chat task the studio bridge is watching.
pub struct TurnOutcome {
    pub state: ActivityState,
    pub detail: String,
}

impl Message {
    /// The document this message names, as a path.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Hello { deck_path }
            | Self::Comment { deck_path, .. }
            | Self::DocComment { deck_path, .. }
            | Self::Reply { deck_path, .. }
            | Self::Focus { deck_path, .. }
            | Self::DocFocus { deck_path, .. }
            | Self::Open { deck_path }
            | Self::Activity { deck_path, .. } => Some(deck_path),
            Self::Theme { .. } | Self::Bye => None,
        }
    }

    /// Deck or doc, by the message's own shape or else its path.
    #[must_use]
    pub fn kind(&self) -> Option<Kind> {
        match self {
            Self::DocComment { .. } | Self::DocFocus { .. } => Some(Kind::Doc),
            Self::Comment { .. } | Self::Focus { .. } => Some(Kind::Deck),
            other => other.path().map(Kind::of_path),
        }
    }

    fn check(&self) -> Result<()> {
        if let Some(path) = self.path() {
            ensure!(path.as_os_str().len() <= 4096, "link path too long");
        }
        match self {
            Self::Activity {
                thread_id, detail, ..
            } => {
                ensure!(thread_id.chars().count() <= 128, "activity thread too long");
                ensure!(
                    detail
                        .as_ref()
                        .is_none_or(|text| text.chars().count() <= 1024),
                    "activity detail too long"
                );
            }
            Self::DocComment {
                block,
                quote,
                thread_id,
                ..
            } => {
                ensure!(block.chars().count() <= 128, "comment block id too long");
                ensure!(thread_id.chars().count() <= 128, "comment thread too long");
                ensure!(quote.chars().count() <= 2000, "comment quote too long");
            }
            Self::DocFocus {
                block, selection, ..
            } => {
                ensure!(
                    block.as_ref().is_none_or(|id| id.chars().count() <= 128),
                    "focus block id too long"
                );
                ensure!(
                    selection
                        .as_ref()
                        .is_none_or(|text| text.chars().count() <= 2000),
                    "focus selection too long"
                );
            }
            _ => {}
        }
        Ok(())
    }
}

impl Line {
    /// Encode a complete line, including its newline.
    pub fn encode(message: Message) -> Result<String> {
        message.check()?;
        let line = serde_json::to_string(&Self {
            version: VERSION,
            kind: message.kind(),
            message,
        })?;
        // The doc payloads are the `Comment` and `Focus` of a doc envelope.
        let line = [("DocComment", "Comment"), ("DocFocus", "Focus")]
            .iter()
            .fold(line, |line, (rust, wire)| {
                line.replacen(
                    &format!("\"type\":\"{rust}\""),
                    &format!("\"type\":\"{wire}\""),
                    1,
                )
            });
        Ok(line + "\n")
    }

    /// Decode one bounded, versioned message. A peer on another version is
    /// told to update rather than misread.
    pub fn decode(source: &str) -> Result<Message> {
        ensure!(source.len() <= 64 * 1024, "link message too large");
        let mut value: serde_json::Value = serde_json::from_str(source)?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == u64::from(VERSION) => {}
            Some(v) if v < u64::from(VERSION) => anyhow::bail!(
                "studio speaks link version {v} but this nexus speaks {VERSION}: update nexus-studio"
            ),
            Some(v) => anyhow::bail!(
                "studio speaks link version {v} but this nexus speaks {VERSION}: update nexus"
            ),
            None => anyhow::bail!("link message has no version"),
        }
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("doc") {
            let wire = value.get("type").and_then(serde_json::Value::as_str);
            let rust = match wire {
                Some("Comment") => Some("DocComment"),
                Some("Focus") => Some("DocFocus"),
                _ => None,
            };
            if let Some(rust) = rust {
                value["type"] = rust.into();
            }
        }
        let line: Self = serde_json::from_value(value)?;
        line.message.check()?;
        ensure!(
            line.kind
                .is_none_or(|kind| Some(kind) == line.message.kind()),
            "link message kind does not match its path"
        );
        Ok(line.message)
    }
}

/// What the terminal and the `canvas` tool share about the open canvas: what
/// studio is showing, and which decks the model just changed. Studio's link is
/// per terminal, not per conversation, so this holds no session.
#[derive(Default)]
pub struct Context {
    /// The deck, slide and selected blocks studio last reported.
    pub focus: Option<(PathBuf, String, Vec<String>)>,
    /// Decks the model created or edited since the terminal last looked, with
    /// the first changed slide, in order.
    pub changed: Vec<(PathBuf, String)>,
    /// Whether canvas already sent its deck field reference this session.
    pub schema_shown: bool,
    /// Whether canvas already sent its deck art direction this session.
    pub art_shown: bool,
    /// The independent, deferred doc field reference has been returned.
    pub doc_schema_shown: bool,
    /// Registered comment turns, keyed by chat task id; the bridge drains results.
    pub turns: HashMap<u64, Option<TurnOutcome>>,
    /// Wakes the terminal's event loop when `changed` grows.
    pub notify: Arc<Notify>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_is_additive_bounded_and_forward_compatible() {
        for state in [
            ActivityState::Queued {
                reason: WaitingReason::Draft,
            },
            ActivityState::Queued {
                reason: WaitingReason::Streaming,
            },
            ActivityState::Queued {
                reason: WaitingReason::NoModel,
            },
            ActivityState::Working,
            ActivityState::Progress,
            ActivityState::Done,
            ActivityState::Failed,
            ActivityState::Stopped,
        ] {
            let message = Message::Activity {
                deck_path: "a.deck.json".into(),
                thread_id: "t1".into(),
                state,
                detail: Some("edited slide 3".into()),
            };
            let line = Line::encode(message.clone()).unwrap();
            assert_eq!(Line::decode(&line).unwrap(), message);
        }
        let future = r#"{"version":2,"type":"Activity","deck_path":"a","thread_id":"t","state":"thinking","detail":"future"}"#;
        assert!(matches!(
            Line::decode(future).unwrap(),
            Message::Activity {
                state: ActivityState::Unknown,
                ..
            }
        ));
        let huge = future.replace("future", &"x".repeat(1025));
        assert!(Line::decode(&huge).is_err());
        assert!(
            Line::decode(&future.replace("\"t\"", &format!("\"{}\"", "t".repeat(129)))).is_err()
        );
    }

    #[test]
    fn protocol_roundtrip_and_version() {
        for message in [
            Message::Hello {
                deck_path: "decks/a.deck.json".into(),
            },
            Message::Open {
                deck_path: "decks/a.deck.json".into(),
            },
            Message::Comment {
                deck_path: "decks/a.deck.json".into(),
                thread_id: "t".into(),
                slide: "s1".into(),
                block: Some("b1".into()),
                point: (2, 3),
                text: "Line one\nline two".into(),
            },
            Message::Reply {
                deck_path: "decks/a.deck.json".into(),
                thread_id: "t".into(),
                text: "Yes".into(),
            },
            Message::Focus {
                deck_path: "decks/a.deck.json".into(),
                slide: "s1".into(),
                blocks: vec!["b1".into()],
            },
            Message::Bye,
        ] {
            let line = Line::encode(message.clone()).unwrap();
            assert_eq!(line.lines().count(), 1);
            assert_eq!(Line::decode(&line).unwrap(), message);
            assert!(Line::decode(&line.replace("\"version\":2", "\"version\":1")).is_err());
        }
    }

    #[test]
    fn lines_are_tagged_bounded_and_strict() {
        let line = Line::encode(Message::Bye).unwrap();
        assert_eq!(line, "{\"version\":2,\"type\":\"Bye\"}\n");
        let comment = Line::encode(Message::Comment {
            deck_path: "decks/a.deck.json".into(),
            thread_id: "t".into(),
            slide: "s1".into(),
            block: None,
            point: (1, 2),
            text: "x".into(),
        })
        .unwrap();
        assert!(comment.contains("\"type\":\"Comment\"") && comment.contains("\"block\":null"));
        for bad in [
            "",
            "not json",
            "{\"version\":1}",
            "{\"version\":1,\"type\":\"Launch\"}",
            "{\"type\":\"Bye\"}",
        ] {
            assert!(Line::decode(bad).is_err(), "{bad}");
        }
        let huge = format!(
            "{{\"version\":2,\"type\":\"Reply\",\"deck_path\":\"a\",\"thread_id\":\"t\",\"text\":\"{}\"}}",
            "x".repeat(70_000)
        );
        assert!(Line::decode(&huge).is_err());
    }

    #[test]
    fn version_two_names_the_path_and_the_kind() {
        let hello = Line::encode(Message::Hello {
            deck_path: "decks/a.deck.json".into(),
        })
        .unwrap();
        assert_eq!(
            hello,
            "{\"version\":2,\"kind\":\"deck\",\"type\":\"Hello\",\"path\":\"decks/a.deck.json\"}\n"
        );
        let open = Line::encode(Message::Open {
            deck_path: "docs/q3.doc.json".into(),
        })
        .unwrap();
        assert!(
            open.contains("\"kind\":\"doc\"") && open.contains("\"path\":\"docs/q3.doc.json\"")
        );
        assert!(!open.contains("deck_path"));
    }

    #[test]
    fn the_old_path_name_is_still_accepted_on_input() {
        let line = r#"{"version":2,"kind":"deck","type":"Open","deck_path":"decks/a.deck.json"}"#;
        assert_eq!(
            Line::decode(line).unwrap(),
            Message::Open {
                deck_path: "decks/a.deck.json".into()
            }
        );
        // Both spellings of a missing kind are fine; a wrong one is not.
        let bare = r#"{"version":2,"type":"Hello","path":"docs/a.doc.json"}"#;
        assert!(Line::decode(bare).is_ok());
        let wrong = r#"{"version":2,"kind":"deck","type":"Hello","path":"docs/a.doc.json"}"#;
        assert!(
            Line::decode(wrong)
                .unwrap_err()
                .to_string()
                .contains("kind")
        );
    }

    #[test]
    fn doc_comments_and_focus_round_trip_with_blocks_and_ranges() {
        let comment = Message::DocComment {
            deck_path: "docs/q3.doc.json".into(),
            thread_id: "t".into(),
            block: "b4".into(),
            range: Some(crate::doc::feedback::Range { start: 3, end: 9 }),
            quote: "Ship the".into(),
            text: "Say why\nthis ships".into(),
        };
        let line = Line::encode(comment.clone()).unwrap();
        assert_eq!(line.lines().count(), 1);
        for needle in [
            "\"kind\":\"doc\"",
            "\"type\":\"Comment\"",
            "\"block\":\"b4\"",
            "\"range\":{\"start\":3,\"end\":9}",
            "\"quote\":\"Ship the\"",
        ] {
            assert!(line.contains(needle), "{needle} in {line}");
        }
        assert!(!line.contains("slide") && !line.contains("point") && !line.contains("DocComment"));
        assert_eq!(Line::decode(&line).unwrap(), comment);
        let whole_block = Message::DocComment {
            deck_path: "docs/q3.doc.json".into(),
            thread_id: "t".into(),
            block: "b4".into(),
            range: None,
            quote: String::new(),
            text: "x".into(),
        };
        assert_eq!(
            Line::decode(&Line::encode(whole_block.clone()).unwrap()).unwrap(),
            whole_block
        );
        for focus in [
            Message::DocFocus {
                deck_path: "docs/q3.doc.json".into(),
                block: Some("b12".into()),
                selection: Some("two words".into()),
            },
            Message::DocFocus {
                deck_path: "docs/q3.doc.json".into(),
                block: None,
                selection: None,
            },
        ] {
            let line = Line::encode(focus.clone()).unwrap();
            assert!(line.contains("\"type\":\"Focus\"") && line.contains("\"kind\":\"doc\""));
            assert_eq!(Line::decode(&line).unwrap(), focus);
        }
        // A deck comment keeps its slide and point.
        let deck = r#"{"version":2,"kind":"deck","type":"Comment","path":"decks/a.deck.json","thread_id":"t","slide":"s1","block":null,"point":[1,2],"text":"x"}"#;
        assert!(matches!(
            Line::decode(deck).unwrap(),
            Message::Comment { .. }
        ));
        let long = Message::DocComment {
            deck_path: "docs/q3.doc.json".into(),
            thread_id: "t".into(),
            block: "b".repeat(129),
            range: None,
            quote: String::new(),
            text: "x".into(),
        };
        assert!(Line::encode(long).is_err());
    }

    #[test]
    fn a_peer_on_another_version_is_told_to_update() {
        let old = r#"{"version":1,"type":"Hello","deck_path":"decks/a.deck.json"}"#;
        let error = Line::decode(old).unwrap_err().to_string();
        assert!(error.contains("update nexus-studio"), "{error}");
        let new = r#"{"version":3,"type":"Bye"}"#;
        let error = Line::decode(new).unwrap_err().to_string();
        assert!(
            error.contains("update nexus") && !error.contains("nexus-studio"),
            "{error}"
        );
        assert!(Line::decode(r#"{"type":"Bye"}"#).is_err());
    }
}
