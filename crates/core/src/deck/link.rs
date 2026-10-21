//! Version-one, secret-free JSON-lines canvas protocol. One envelope per line.

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

/// Studio sends Hello/Comment/Reply/Focus; nexus sends Open/Focus/Bye.
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
    Bye,
}

impl Line {
    /// Encode a complete line, including its newline.
    pub fn encode(message: Message) -> Result<String> {
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
        Ok(line.message)
    }
}

/// What the terminal and the `deck` tool share about the open canvas: what
/// studio is showing, and which decks the model just changed. Studio's link is
/// per terminal, not per conversation, so this holds no session.
#[derive(Default)]
pub struct Context {
    /// The deck, slide and selected blocks studio last reported.
    pub focus: Option<(PathBuf, String, Vec<String>)>,
    /// Decks the model created or edited since the terminal last looked, with
    /// the first changed slide, in order.
    pub changed: Vec<(PathBuf, String)>,
    /// Wakes the terminal's event loop when `changed` grows.
    pub notify: Arc<Notify>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
