//! The composer frame and the menus that float above it: slash commands
//! and `@` files.

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, ListItem, ListState, Padding};

use super::{popups, to_color};
use crate::app_view::AppView;

pub(super) fn render_input(f: &mut Frame, app: &mut AppView, area: Rect) {
    let hints = !app.settings.hide_hints;
    let busy = app.is_streaming() || app.is_compacting_current_session();
    // What's running shows regardless of `hide_hints` — it's state, not key
    // help; only the "Esc to stop" part is a hint.
    let activity = if app.viewing_stream() {
        Some(if hints {
            "working · Esc to stop".to_string()
        } else {
            "working".into()
        })
    } else if app.is_compacting_current_session() {
        Some("⟳ compacting…".to_string())
    } else if app.is_streaming() {
        let n = app.chat_task_count();
        Some(format!(
            "⟳ {n} chat{} running",
            if n == 1 { "" } else { "s" }
        ))
    } else {
        app.research_running
            .as_ref()
            .filter(|(id, _)| app.session.as_ref().is_none_or(|s| &s.id != id))
            .map(|(_, topic)| format!("🔎 researching: {topic}"))
    };
    // The border takes the spinner's color while something runs, so the
    // active state reads at a glance.
    let border_color = if busy {
        to_color(app.spinner_color())
    } else {
        app.theme.border_dim
    };
    let mut title = vec![Span::styled(
        " ❯ ",
        Style::default()
            .fg(if busy { border_color } else { app.theme.accent })
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(activity) = activity {
        title.push(Span::styled(
            format!("{activity} "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    let name = app.session.as_ref().map(|s| s.title.clone());
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(title));
    if let Some(name) = name {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" {} ", popups::chrome::truncate(&name, 40)),
                Style::default().fg(app.theme.fg_dim),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    app.input_inner = inner; // remembered for mouse click -> cursor mapping
    f.render_widget(block, area);
    app.input
        .set_selection_style(Style::default().bg(app.theme.selection).fg(app.theme.fg));
    app.input
        .set_placeholder_style(Style::default().fg(app.theme.fg_dim));
    f.render_widget(&app.input, inner);
}

/// Slash-command autocomplete: a fuzzy-ranked list floating just above the
/// input box: `/name` in the accent color, its description dimmed alongside.
pub(super) fn render_command_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
    let matches = app.command_matches();
    if matches.is_empty() {
        return;
    }
    // Pad the `/name` column so every description starts at the same column.
    let name_w = matches
        .iter()
        .map(|c| c.name().chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let desc_w = usize::from(MENU_W).saturating_sub(name_w + 8);
    let items: Vec<ListItem> = matches
        .iter()
        .map(|c| {
            let name = format!("/{}", c.name());
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{name:<name_w$}"),
                    Style::default().fg(app.theme.accent),
                ),
                Span::raw("  "),
                Span::styled(
                    popups::chrome::truncate(c.desc(), desc_w),
                    Style::default().fg(app.theme.fg_dim),
                ),
            ]))
        })
        .collect();
    floating_menu(
        f,
        app,
        input_area,
        ("commands", "Tab fill · Enter run"),
        items,
        app.command_selected(),
    );
}

/// Widest a floating composer menu gets.
const MENU_W: u16 = 76;

/// A rounded menu floating just above the composer's left edge, at most ten
/// rows tall (it scrolls), selection styled like every popup list.
fn floating_menu(
    f: &mut Frame,
    app: &AppView,
    input_area: Rect,
    (title, hint): (&str, &str),
    items: Vec<ListItem<'static>>,
    selected: usize,
) {
    let rows = u16::try_from(items.len()).unwrap_or(u16::MAX).min(10);
    let h = (rows + 2).min(input_area.y);
    if h < 3 {
        return;
    }
    let area = Rect {
        x: input_area.x,
        y: input_area.y - h,
        width: input_area.width.min(MENU_W),
        height: h,
    };
    let hint = if app.settings.hide_hints { "" } else { hint };
    let block = popups::chrome::hinted_block(
        Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(app.theme.fg_dim),
        )),
        hint,
        app,
        false,
        popups::chrome::Tone::Normal,
        area.width,
    );
    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));
    let list = popups::chrome::standard_list(items, &app.theme).block(block);
    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut state);
}

/// `@` file autocomplete: space files matching the text after `@`.
pub(super) fn render_at_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
    let Some((ref matches, selected, _)) = app.at_state else {
        return;
    };
    let row_w = usize::from(input_area.width.min(MENU_W)).saturating_sub(5);
    let items: Vec<ListItem> = matches
        .iter()
        .map(|f| {
            let meta = format!(
                "  {}  {}",
                nexus_core::app::human_size(f.size.unsigned_abs()),
                f.status
            );
            let name = popups::chrome::truncate_middle(
                &f.name,
                row_w.saturating_sub(meta.chars().count()),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, Style::default().fg(app.theme.fg_dim)),
            ]))
        })
        .collect();
    floating_menu(
        f,
        app,
        input_area,
        ("files", "Tab insert · Esc cancel"),
        items,
        selected,
    );
}
