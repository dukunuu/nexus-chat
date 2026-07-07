use super::{App, MemoryOp};
use crate::provider::ChatMessage;
use tokio::sync::mpsc;

impl App {
    // --- memory (per-space, extracted after every assistant reply) ---

    /// Raw contents of the active space's memory file, capped to ~120k chars
    /// (~30k tokens — headroom is cheap on 1M-context models; this just stops
    /// a runaway file from eating the whole budget).
    pub(super) fn read_memory(&self) -> String {
        let text = std::fs::read_to_string(self.space.memory_path(&self.active_space.name))
            .unwrap_or_default();
        text.chars().take(120_000).collect()
    }

    /// After an assistant reply, ask the memory model for ADD/UPDATE/DELETE ops
    /// against the space's fact file. No-op if extraction is disabled or the
    /// last exchange is unavailable.
    pub(super) fn maybe_extract_memory(&mut self) {
        if self.memory_model.trim().is_empty() {
            return;
        }
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let [user_msg, assistant_msg] = match self.messages.as_slice() {
            [.., u, a] if u.role == "user" && a.role == "assistant" => [u.clone(), a.clone()],
            _ => return,
        };
        let facts = self.read_memory();
        let model = self.memory_model.clone();
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
            if let Ok(text) = provider.complete(&model, msgs).await {
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
        let original: Vec<String> = self
            .read_memory()
            .lines()
            .filter_map(parse_fact_line)
            .map(|(_, text)| text)
            .collect();
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
        let mut facts: Vec<String> = original
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !deletes.contains(&(i + 1)))
            .map(|(i, text)| updates.remove(&(i + 1)).unwrap_or(text))
            .collect();
        facts.extend(adds);
        let body: String = facts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {f}\n", i + 1))
            .collect();
        let _ = std::fs::write(self.space.memory_path(&self.active_space.name), body);
    }
}

/// Parse one numbered fact line (`"3. some fact"`) into `(id, text)`.
pub(super) fn parse_fact_line(line: &str) -> Option<(usize, String)> {
    let (num, rest) = line.split_once(". ")?;
    let id: usize = num.trim().parse().ok()?;
    Some((id, rest.trim().to_string()))
}

/// Parse the memory model's reply into a list of ops. Tolerates surrounding
/// prose/fences by extracting the first `[...]`; malformed or unrecognized
/// entries are silently skipped rather than failing the whole batch.
pub(super) fn parse_memory_ops(text: &str) -> Vec<MemoryOp> {
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
