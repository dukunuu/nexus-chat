// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use super::{App, MemoryOp};
use crate::db::Message;
use crate::provider::ChatMessage;
use std::fmt::Write as _;
use tokio::sync::mpsc;

impl App {
    // --- memory (per-space, extracted after every assistant reply) ---

    /// Raw contents of the active space's memory file, capped to ~120k chars
    /// (~30k tokens — headroom is cheap on 1M-context models; this just stops
    /// a runaway file from eating the whole budget).
    pub fn read_memory(&self) -> String {
        let text = std::fs::read_to_string(self.space.memory_path(&self.active_space.name))
            .unwrap_or_default();
        text.chars().take(120_000).collect()
    }

    /// After an assistant reply, ask the memory model for ADD/UPDATE/DELETE ops
    /// against the space's fact file. No-op if extraction is disabled or the
    /// last exchange is unavailable.
    pub fn maybe_extract_memory(&mut self) {
        if self.memory_model.trim().is_empty() {
            return;
        }
        let Some((provider, raw_model)) = self.resolve_utility_model_backend(&self.memory_model)
        else {
            return;
        };
        let Some((user_msg, assistant_msg)) = latest_memory_exchange(&self.messages) else {
            return;
        };
        let facts = self.read_memory();
        let space = self.active_space.name.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.memory_rx = Some(rx);
        tokio::spawn(async move {
            let truncate = |s: &str| s.chars().take(2000).collect::<String>();
            let prompt = format!(
                "Stored facts (numbered, may be empty):\n{facts}\n\n\
                 Latest exchange:\nuser: {}\nassistant: {}\n\n\
                 Reply with ONLY a JSON array of memory ops, no markdown, no prose. \
                 Each op is one of:\n\
                 {{\"op\":\"add\",\"text\":\"<durable single-line fact>\"}}\n\
                 {{\"op\":\"update\",\"id\":<N>,\"text\":\"<replacement>\"}}\n\
                 {{\"op\":\"delete\",\"id\":<N>}}\n\
                 Empty array [] if nothing memory-worthy. Facts must be durable and \
                 user/project-relevant (preferences, identity, ongoing goals) — never a \
                 summary of what was just said. Merge/update instead of duplicating. \
                 Keep the total under 500 facts.",
                truncate(&user_msg.content),
                truncate(&assistant_msg.content),
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(text) = provider.complete(&raw_model, msgs).await {
                let ops = parse_memory_ops(&text);
                let _ = tx.send((space, ops));
            }
        });
    }

    /// Apply extracted ops to the active space's memory file, if it's still the
    /// active one (a meanwhile space-switch discards stale results).
    pub fn on_memory_result(&mut self, result: Option<(String, Vec<MemoryOp>)>) {
        self.memory_rx = None;
        let Some((space, ops)) = result else { return };
        if space != self.active_space.name || ops.is_empty() {
            return;
        }
        // Ids in `ops` refer to the *original* numbering, so resolve updates/
        // deletes against that fixed list before appending adds — mutating the
        // vector in place as ops are applied would shift later ids underfoot.
        let mut updates: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();
        let mut deletes: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut adds: Vec<String> = Vec::new();
        for op in ops {
            match op {
                MemoryOp::Add(text) => adds.push(text),
                MemoryOp::Update(id, text) => {
                    updates.insert(id, text);
                }
                MemoryOp::Delete(id) => {
                    deletes.insert(id);
                }
            }
        }
        let mut facts: Vec<String> = self
            .read_memory()
            .lines()
            .filter_map(parse_fact_line)
            .map(|(_, text)| text)
            .enumerate()
            .filter(|(i, _)| !deletes.contains(&(i + 1)))
            .map(|(i, text)| updates.remove(&(i + 1)).unwrap_or(text))
            .collect();
        facts.extend(adds);
        let body: String = facts
            .iter()
            .enumerate()
            .fold(String::new(), |mut b, (i, f)| {
                let _ = writeln!(b, "{}. {f}", i + 1);
                b
            });
        let _ = self.space.ensure_space_dir(&self.active_space.name);
        let _ = std::fs::write(self.space.memory_path(&self.active_space.name), body);
    }
}

/// Latest user→assistant exchange worth memory extraction. Tool results are
/// stored as transcript messages between the user and final assistant answer,
/// so don't require the final two visible rows to be exactly user/assistant.
fn latest_memory_exchange(messages: &[Message]) -> Option<(Message, Message)> {
    let assistant_idx = messages
        .iter()
        .rposition(|m| m.role == "assistant" && m.persona.is_none())?;
    let user_idx = messages[..assistant_idx]
        .iter()
        .rposition(|m| m.role == "user")?;
    Some((messages[user_idx].clone(), messages[assistant_idx].clone()))
}

/// Parse one numbered fact line (`"3. some fact"`) into `(id, text)`.
pub fn parse_fact_line(line: &str) -> Option<(usize, String)> {
    let (num, rest) = line.split_once(". ")?;
    let id: usize = num.trim().parse().ok()?;
    Some((id, rest.trim().to_string()))
}

/// Parse the memory model's reply into a list of ops. Tolerates surrounding
/// prose/fences by extracting the first `[...]`; malformed or unrecognized
/// entries are silently skipped rather than failing the whole batch.
pub fn parse_memory_ops(text: &str) -> Vec<MemoryOp> {
    let Some(start) = text.find('[') else {
        return Vec::new();
    };
    let Some(end) = text.rfind(']') else {
        return Vec::new();
    };
    let Some(json) = text.get(start..=end) else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(arr) = arr.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let op = v.get("op")?.as_str()?;
            match op {
                "add" => Some(MemoryOp::Add(v.get("text")?.as_str()?.trim().to_string())),
                "update" => Some(MemoryOp::Update(
                    v.get("id")?.as_u64()? as usize,
                    v.get("text")?.as_str()?.trim().to_string(),
                )),
                "delete" => Some(MemoryOp::Delete(v.get("id")?.as_u64()? as usize)),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }

    #[test]
    fn latest_memory_exchange_skips_tool_rows_between_user_and_assistant() {
        let messages = vec![
            msg("user", "remember I prefer terse answers"),
            msg("tool_call", "search result"),
            msg("assistant", "Noted."),
        ];

        let (user, assistant) = latest_memory_exchange(&messages).unwrap();
        assert_eq!(user.content, "remember I prefer terse answers");
        assert_eq!(assistant.content, "Noted.");
    }

    #[test]
    fn latest_memory_exchange_ignores_persona_round_replies() {
        let mut persona = msg("assistant", "persona chatter");
        persona.persona = Some("Skeptic".to_string());
        let messages = vec![
            msg("user", "remember x"),
            persona,
            msg("assistant", "final"),
        ];

        let (_, assistant) = latest_memory_exchange(&messages).unwrap();
        assert_eq!(assistant.content, "final");
    }
}
