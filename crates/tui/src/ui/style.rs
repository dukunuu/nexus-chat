//! The TUI's visual vocabulary. Every screen draws from here so one meaning
//! always looks one way: a single monochrome glyph per concept (no emoji —
//! their widths vary by terminal and they clash with the line-art chrome),
//! one color per role, one way to draw a section header, a separator, or a
//! line of metadata.
//!
//! Color roles:
//! - `accent`: you, and anything interactive or waiting on your input
//!   (your turns, the prompt, selections, titles, questions for you)
//! - `accent2`: the assistant and its agents (replies, tools, research)
//! - `fg_dim`: metadata (times, stats, sizes, hints)
//! - `border_dim`: chrome (frames, rules, rails, separators)
//! - `success` / `warning` / `error`: status, nothing else

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// One glyph per concept, each a single terminal column.
pub mod glyph {
    /// Your turns and the composer prompt.
    pub const YOU: &str = "❯";
    /// An assistant reply.
    pub const ASSISTANT: &str = "✦";
    /// A tool call.
    pub const TOOL: &str = "⚒";
    /// Research: stage rows and research sessions.
    pub const RESEARCH: &str = "◇";
    /// A question round waiting on your answer.
    pub const QUESTION: &str = "?";
    /// A plan waiting on your approval.
    pub const PLAN: &str = "☰";
    /// A compaction digest.
    pub const DIGEST: &str = "≡";
    /// A link to another session.
    pub const LINK: &str = "↪";
    /// An image or document placeholder.
    pub const MEDIA: &str = "▣";
    /// The active model.
    pub const MODEL: &str = "◆";
    /// The active space.
    pub const SPACE: &str = "⌂";
    /// Web answer mode.
    pub const WEB: &str = "◎";
    /// Incognito mode.
    pub const INCOGNITO: &str = "◌";
    /// Something running.
    pub const RUNNING: &str = "⟳";
    /// Finished while you weren't looking; a toggle that's on.
    pub const DOT: &str = "●";
    /// A toggle that's off; an inactive choice.
    pub const RING: &str = "○";
    /// Success.
    pub const OK: &str = "✓";
    /// Failure.
    pub const FAIL: &str = "✗";
    /// A warning (text form: one column in every terminal).
    pub const WARN: &str = "⚠";
    /// The selected row; a collapsed section.
    pub const SELECTED: &str = "▸";
    /// An expanded section.
    pub const EXPANDED: &str = "▾";
    /// Section-header bar.
    pub const SECTION: &str = "▍";
    /// A card's left rail.
    pub const RAIL: &str = "▎";
    /// A favorite.
    pub const FAVORITE: &str = "★";
    /// Accepts images.
    pub const VISION: &str = "⊡";
}

/// The one separator between inline fields.
pub const SEP: &str = " · ";

/// A popup or panel title: the name in bold accent.
pub fn title(theme: &Theme, name: impl Into<String>) -> Span<'static> {
    Span::styled(
        format!(" {} ", name.into()),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )
}

/// `▍title ─────` — the one section header, ruled out to `width`.
pub fn section(theme: &Theme, name: &str, width: usize) -> Line<'static> {
    let head = format!("{} {name} ", glyph::SECTION);
    let rule = width.saturating_sub(head.chars().count());
    Line::from(vec![
        Span::styled(
            head,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("─".repeat(rule), Style::default().fg(theme.border_dim)),
    ])
}

/// Metadata text: times, stats, sizes, hints.
pub fn meta(theme: &Theme, text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(theme.fg_dim))
}

/// The inline field separator, in the chrome color.
pub fn sep(theme: &Theme) -> Span<'static> {
    Span::styled(SEP, Style::default().fg(theme.border_dim))
}

/// A transcript event's first line: role-colored glyph, then its title.
pub fn event_head(
    glyph: &str,
    glyph_style: Style,
    title: String,
    title_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(title, title_style),
    ])
}

/// A transcript event's body line, indented under its title.
pub fn event_body(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), style))
}

#[cfg(test)]
mod tests {
    /// The vocabulary is monochrome: an emoji in any UI string (outside
    /// tests) would reintroduce varying widths and a second visual language.
    #[test]
    fn ui_strings_use_the_glyph_vocabulary_not_emoji() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
        let mut stack = vec![root];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.file_name().is_some_and(|n| n == "tests.rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                let code = src.split("#[cfg(test)]").next().unwrap_or("");
                for (n, line) in code.lines().enumerate() {
                    let is_comment = line.trim_start().starts_with("//");
                    if !is_comment
                        && line
                            .chars()
                            .any(|c| ('\u{1F000}'..='\u{1FFFF}').contains(&c))
                    {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "emoji in UI strings:\n{}",
            offenders.join("\n")
        );
    }
}
