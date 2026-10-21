//! Anchored feedback, compact context and independently checked suggested edits.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::suggest::{self, RawSuggestion, Selection, Suggestion};
use super::{CheckError, Content, Deck, HEIGHT, WIDTH, issue};

/// The intent of a feedback request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Thread,
    Review,
    Chat,
}

/// A point on a slide, optionally attached to a block.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub slide: String,
    pub block: Option<String>,
    pub x: i32,
    pub y: i32,
}

impl Anchor {
    /// Reject dangling anchors rather than silently moving a comment.
    pub fn check(&self, deck: &Deck) -> Result<(), String> {
        let slide = deck
            .slides
            .iter()
            .find(|s| s.id == self.slide)
            .ok_or("unknown slide")?;
        if self
            .block
            .as_ref()
            .is_some_and(|id| !slide.blocks.iter().any(|b| b.id == *id))
        {
            return Err("unknown block on anchored slide".into());
        }
        if !(0..WIDTH).contains(&self.x) || !(0..HEIGHT).contains(&self.y) {
            return Err("anchor point outside slide".into());
        }
        Ok(())
    }
}

/// Priority is visible on both the pin and thread.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Must,
    Should,
    #[default]
    Consider,
}

/// Conversation context; roles are restricted, never system messages.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    You,
    Model,
}

/// The complete thread, or recent chat history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

/// Revision-bound feedback request accepted at `/v1/studio/suggest`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub kind: Kind,
    pub deck: Deck,
    pub revision: String,
    #[serde(default)]
    pub selection: Selection,
    pub anchor: Option<Anchor>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub instruction: String,
    pub model: Option<String>,
}

impl Request {
    /// Validate deck, selection, conversation bounds and anchor before inference.
    pub fn check(&self) -> Result<(), Vec<CheckError>> {
        let mut errors = Vec::new();
        if self.instruction.chars().count() > 2_000 {
            issue(&mut errors, "instruction", "at most 2000 characters");
        }
        if self.revision != self.deck.revision() {
            issue(&mut errors, "revision", "does not match deck");
        }
        let source = serde_json::to_string(&self.deck).expect("deck JSON");
        if let Err(deck_errors) = Deck::check(&source) {
            errors.extend(deck_errors);
        }
        self.selection.check(&self.deck, &mut errors);
        if self.kind == Kind::Thread && self.anchor.is_none() {
            issue(&mut errors, "anchor", "thread requires an anchor");
        }
        if let Some(anchor) = &self.anchor
            && let Err(error) = anchor.check(&self.deck)
        {
            issue(&mut errors, "anchor", &error);
        }
        if self.messages.len() > 100 || self.messages.iter().any(|m| m.text.chars().count() > 8_000)
        {
            issue(
                &mut errors,
                "messages",
                "at most 100 messages, 8000 characters each",
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A validated model-authored comment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub anchor: Anchor,
    pub text: String,
    pub severity: Severity,
    pub edit: Option<Suggestion>,
}

/// The host proposes; it never changes the document.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub text: String,
    pub comments: Vec<Comment>,
    pub edit: Option<Suggestion>,
    pub options: Vec<Suggestion>,
    pub revision: String,
    pub model: String,
    pub notes: Vec<String>,
}

/// Only full slides selected by this function can be edited by the model.
#[must_use]
pub fn prompt_context(req: &Request) -> String {
    let small = req.deck.slides.len() <= 12
        && serde_json::to_vec(&req.deck).expect("deck JSON").len() <= 24 * 1024;
    let mut full = Vec::new();
    if let Some(anchor) = &req.anchor {
        full.push(anchor.slide.clone());
    }
    if req.kind != Kind::Thread {
        for id in req
            .selection
            .slides
            .iter()
            .chain(req.selection.blocks.iter().map(|(s, _)| s))
            .chain(req.deck.slides.iter().map(|s| &s.id))
        {
            if !full.contains(id) && (small || full.len() < 12) {
                full.push(id.clone());
            }
        }
    }
    let slides: Vec<_> = req
        .deck
        .slides
        .iter()
        .map(|s| {
            if full.contains(&s.id) {
                serde_json::to_value(s).expect("slide JSON")
            } else {
                let title = s
                    .blocks
                    .iter()
                    .find_map(|b| match &b.content {
                        Content::Text { text } => Some(text.lines().next().unwrap_or_default()),
                        _ => None,
                    })
                    .unwrap_or_default();
                json!({"id":s.id,"layout":s.layout,"title":title,"summary":true})
            }
        })
        .collect();
    json!({"kind":req.kind,"title":req.deck.title,"theme":req.deck.theme,"slides":slides,"selection":req.selection,"anchor":req.anchor,"messages":req.messages,"instruction":req.instruction}).to_string()
}

fn edit(base: &Deck, value: Value) -> Result<Suggestion, String> {
    let raw: RawSuggestion = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if raw.why.trim().is_empty() || raw.ops.is_empty() {
        return Err("suggested edit needs a summary and at least one operation".into());
    }
    let (mut edits, errors) = suggest::prepare(base, vec![raw]);
    edits.pop().ok_or_else(|| {
        errors
            .iter()
            .map(|e| format!("{}: {}", e.path, e.message))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn comment(base: &Deck, value: Value) -> Result<Comment, String> {
    #[derive(Deserialize)]
    struct Raw {
        anchor: Anchor,
        text: String,
        severity: Severity,
        edit: Option<Value>,
    }
    let raw: Raw = serde_json::from_value(value).map_err(|e| e.to_string())?;
    raw.anchor.check(base)?;
    if !plain_comment(&raw.text) {
        return Err("comment must be nonempty, at most two sentences and 500 characters".into());
    }
    Ok(Comment {
        anchor: raw.anchor,
        text: raw.text,
        severity: raw.severity,
        edit: raw.edit.map(|v| edit(base, v)).transpose()?,
    })
}

fn plain_comment(text: &str) -> bool {
    let mut sentences = 0;
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?')
            && text[i + 1..].chars().next().is_none_or(char::is_whitespace)
        {
            sentences += usize::from(!text[start..=i].trim().is_empty());
            start = i + 1;
        }
    }
    sentences += usize::from(!text[start..].trim().is_empty());
    !text.trim().is_empty() && text.chars().count() <= 500 && sentences <= 2
}

fn reply_object(source: &str) -> Option<Value> {
    let source = source.trim();
    let source = if source.starts_with("```") {
        source.split_once('\n').map_or(source, |(_, body)| body)
    } else {
        source
    };
    serde_json::Deserializer::from_str(source)
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok)
        .filter(Value::is_object)
}

/// Tolerate fences/trailing prose; validate each comment/option independently.
/// Located errors are returned for one repair round; valid siblings survive.
#[must_use]
pub fn prepare(req: &Request, source: &str) -> (Response, Vec<CheckError>) {
    let mut response = Response {
        revision: req.revision.clone(),
        ..Response::default()
    };
    let mut errors = Vec::new();
    let Some(value) = reply_object(source) else {
        issue(&mut errors, "reply", "expected JSON object");
        return (response, errors);
    };
    if let Some(text) = value["text"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 8_000)
    {
        response.text = text.into();
    } else {
        issue(
            &mut errors,
            "text",
            "expected nonempty reply, at most 8000 characters",
        );
    }
    if req.kind == Kind::Thread && !plain_comment(&response.text) {
        response.text.clear();
        issue(
            &mut errors,
            "text",
            "thread reply must be at most two sentences and 500 characters",
        );
    }
    if req.kind == Kind::Review {
        if let Some(comments) = value["comments"].as_array() {
            if comments.len() > 8 {
                issue(
                    &mut errors,
                    "comments",
                    "at most 8 comments, most impactful first",
                );
            }
            for (i, value) in comments.iter().take(8).enumerate() {
                match comment(&req.deck, value.clone()) {
                    Ok(comment) => response.comments.push(comment),
                    Err(error) => issue(&mut errors, &format!("comments[{i}]"), &error),
                }
            }
        } else {
            issue(
                &mut errors,
                "comments",
                "review requires an array of anchored comments",
            );
        }
    } else if value
        .get("comments")
        .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
    {
        issue(&mut errors, "comments", "pinned comments belong to reviews");
    }
    if let Some(value) = value.get("edit").filter(|v| !v.is_null()) {
        match edit(&req.deck, value.clone()) {
            Ok(edit) => response.edit = Some(edit),
            Err(error) => issue(&mut errors, "edit", &error),
        }
    }
    if let Some(options) = value.get("options").filter(|v| !v.is_null()) {
        if let Some(options) = options
            .as_array()
            .filter(|a| a.is_empty() || (req.kind == Kind::Chat && (2..=3).contains(&a.len())))
        {
            for (i, value) in options.iter().enumerate() {
                match edit(&req.deck, value.clone()) {
                    Ok(edit) => response.options.push(edit),
                    Err(error) => issue(&mut errors, &format!("options[{i}]"), &error),
                }
            }
        } else {
            issue(
                &mut errors,
                "options",
                "chat options must contain 2–3 independent alternatives",
            );
        }
    }
    if !response.options.is_empty() && response.options.len() < 2 {
        response.options.clear();
        issue(
            &mut errors,
            "options",
            "fewer than two valid options remain",
        );
    }
    if response.edit.is_some() && !response.options.is_empty() {
        response.edit = None;
        issue(
            &mut errors,
            "edit",
            "return a change set OR options, not both",
        );
    }
    (response, errors)
}

#[cfg(test)]
mod tests;
