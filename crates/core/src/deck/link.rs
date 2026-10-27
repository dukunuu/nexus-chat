//! Version-one, secret-free JSON-lines canvas protocol. One envelope per line.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// Protocol envelope; reject unsupported versions before handling a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub version: u8,
    #[serde(flatten)]
    pub message: Message,
}

/// Studio sends Hello/Comment/Reply/Focus; nexus sends Open/Focus/Theme/Activity/Bye.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    Hello {
        deck_path: PathBuf,
    },
    Comment {
        deck_path: PathBuf,
        thread_id: String,
        slide: String,
        block: Option<String>,
        point: (i32, i32),
        text: String,
    },
    Reply {
        deck_path: PathBuf,
        thread_id: String,
        text: String,
    },
    Focus {
        deck_path: PathBuf,
        slide: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocks: Vec<String>,
    },
    Open {
        deck_path: PathBuf,
    },
    Theme {
        colors: crate::theme_colors::ThemeColors,
    },
    Activity {
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
    fn check(&self) -> Result<()> {
        if let Self::Activity {
            deck_path,
            thread_id,
            detail,
            ..
        } = self
        {
            ensure!(
                deck_path.as_os_str().len() <= 4096,
                "activity path too long"
            );
            ensure!(thread_id.chars().count() <= 128, "activity thread too long");
            ensure!(
                detail
                    .as_ref()
                    .is_none_or(|text| text.chars().count() <= 1024),
                "activity detail too long"
            );
        }
        Ok(())
    }
}

impl Line {
    /// Encode a complete line, including its newline.
    pub fn encode(message: Message) -> Result<String> {
        message.check()?;
        Ok(serde_json::to_string(&Self {
            version: 1,
            message,
        })? + "\n")
    }

    /// Decode one bounded, versioned message.
    pub fn decode(source: &str) -> Result<Message> {
        ensure!(source.len() <= 64 * 1024, "link message too large");
        let line: Self = serde_json::from_str(source)?;
        ensure!(line.version == 1, "unsupported studio protocol version");
        line.message.check()?;
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
        let future = r#"{"version":1,"type":"Activity","deck_path":"a","thread_id":"t","state":"thinking","detail":"future"}"#;
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
            assert!(Line::decode(&line.replace("\"version\":1", "\"version\":2")).is_err());
        }
    }

    #[test]
    fn lines_are_tagged_bounded_and_strict() {
        let line = Line::encode(Message::Bye).unwrap();
        assert_eq!(line, "{\"version\":1,\"type\":\"Bye\"}\n");
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
            "{{\"version\":1,\"type\":\"Reply\",\"deck_path\":\"a\",\"thread_id\":\"t\",\"text\":\"{}\"}}",
            "x".repeat(70_000)
        );
        assert!(Line::decode(&huge).is_err());
    }
}
