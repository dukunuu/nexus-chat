//! Flowing document data, atomic edits, Markdown, history and comments.
pub mod direct;
pub mod feedback;
pub mod markdown;
pub mod model;
pub mod ops;
pub mod storage;
pub mod suggest;
pub mod validate;

pub use crate::deck::history;
pub use model::{Block, Doc, Kind, ListItem, Page, Theme};
pub use ops::{Op, apply, apply_with_resolver};

impl Doc {
    /// Empty document following the supplied design.
    #[must_use]
    pub fn from_design(title: &str, design: &crate::design::Design) -> Self {
        Self {
            version: 1,
            title: title.into(),
            design: Some(design.slug()),
            design_rev: Some(design.revision()),
            theme: Theme::from_design(design),
            page: design.doc.page.clone(),
            blocks: Vec::new(),
            footnotes: std::collections::BTreeMap::new(),
        }
    }
    /// SHA-256 of canonical JSON.
    #[must_use]
    pub fn revision(&self) -> String {
        crate::deck::hash(self)
    }
    /// Find a block by stable id.
    #[must_use]
    pub fn block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }
}

/// Compact, bounded outline. The final `more:` line names omitted blocks.
#[must_use]
pub fn outline(doc: &Doc, max_chars: usize) -> String {
    let lines = doc
        .blocks
        .iter()
        .map(|block| {
            let label = match &block.kind {
                Kind::Title { text } => format!("# {text}"),
                Kind::Heading { level, text } => {
                    format!("{} {text}", "#".repeat(usize::from(*level) + 1))
                }
                Kind::List { ordered, items } => format!(
                    "{} {}",
                    if *ordered { "1." } else { "-" },
                    items.first().map_or("", |i| i.text.as_str())
                ),
                Kind::Table { rows, .. } => format!(
                    "table {}x{}: {}{}",
                    rows.len(),
                    rows.first().map_or(0, Vec::len),
                    rows.first().map_or_else(String::new, |r| r.join(" | ")),
                    if rows.len() > 1 { " …" } else { "" }
                ),
                Kind::Image { alt, .. } => format!("image: {alt}"),
                Kind::Chart { .. } => "chart".into(),
                Kind::Divider => "---".into(),
                Kind::PageBreak => "page_break".into(),
                Kind::Toc => "toc".into(),
                _ => block.text().unwrap_or_default().replace('\n', " "),
            };
            let label = label.replace('\n', " ");
            let label = if label.chars().count() > 100 {
                format!("{}…", label.chars().take(99).collect::<String>())
            } else {
                label
            };
            format!("[{}] {label}", block.id)
        })
        .collect::<Vec<_>>();
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        let remaining = lines.len() - index;
        let more = format!("more: {remaining} blocks\n");
        let reserve = if remaining > 1 {
            more.chars().count()
        } else {
            0
        };
        if out.chars().count() + line.chars().count() + 1 + reserve > max_chars {
            out.extend(
                more.chars()
                    .take(max_chars.saturating_sub(out.chars().count())),
            );
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Render selected blocks in full, in document order. A heading selection
/// includes its section through the next heading of equal or higher level.
#[must_use]
pub fn read_blocks(doc: &Doc, ids: &[String]) -> String {
    let mut selected = doc.clone();
    let mut section = None;
    selected.blocks.retain(|block| {
        if let Kind::Heading { level, .. } = block.kind {
            if section.is_some_and(|current| level <= current) {
                section = None;
            }
            if ids.contains(&block.id) {
                section = Some(level);
            }
        }
        ids.contains(&block.id) || section.is_some()
    });
    selected
        .blocks
        .iter()
        .map(|block| format!("[{}]\n{}", block.id, markdown::render_block(block)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests;
