// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, ListItem, ListState, Padding, Paragraph,
};

use crate::app_view::AppView;
use nexus_core::app::Popup;

pub mod citations_style;
pub mod history;
pub mod markdown;
pub mod popups;

use history::render_history;

pub fn render(f: &mut Frame, app: &mut AppView) {
    // Popups re-record their list geometry as they draw; a frame without a
    // list must not leave a stale hit area behind.
    app.list_hit.set(None);
    // Paint the base first so widgets that only set foreground colors inherit
    // the configured opaque or terminal-transparent surface.
    f.render_widget(
        Block::default().style(app.theme.background_style()),
        f.area(),
    );

    // Grow the input box with its wrapped content (1–20 rows) plus 2 for the
    // border. `measure` wants the width the widget renders at: inside the
    // border and its one-column padding on each side.
    let inner_w = f.area().width.saturating_sub(4);
    let content_rows = app.input.measure(inner_w).preferred_rows;
    let input_h = content_rows.saturating_add(2);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),          // history
            Constraint::Length(input_h), // input (auto-height, max 22)
            Constraint::Length(1),       // status (with inline context bar)
        ])
        .split(f.area());

    render_history(f, app, chunks[0]);
    render_input(f, app, chunks[1]);
    render_status(f, app, chunks[2]);
    // Autocomplete floats above the input; only when no modal popup is open.
    if app.popup == Popup::None {
        render_command_popup(f, app, chunks[1]);
        render_at_popup(f, app, chunks[1]);
        render_notifications(f, app, chunks[0]);
    } else {
        app.notification_areas.clear();
    }

    // Dim everything behind an open popup so the modal reads as the focus
    // (every popup clears its own rect first, so it stays at full strength).
    if app.popup != Popup::None {
        let area = f.area();
        f.buffer_mut()
            .set_style(area, Style::default().add_modifier(Modifier::DIM));
    }

    match app.popup {
        Popup::Model => popups::model::render(f, app),
        Popup::Session => popups::session::render(f, app),
        Popup::Copy => popups::copy::render(f, app),
        Popup::Key => popups::key::render(f, app),
        Popup::Settings => popups::settings::render(f, app),
        Popup::Space => popups::space::render(f, app),
        Popup::Context => popups::context::render(f, app),
        Popup::Skills => popups::skills::render(f, app),
        Popup::Files => popups::files::render(f, app),
        Popup::Apps => popups::apps::render(f, app),
        Popup::Watch => popups::watches::render(f, app),
        Popup::ResearchLive => popups::research_live::render(f, app),
        Popup::Swarm => popups::swarm::render(f, app),
        Popup::Usage => popups::usage::render(f, app),
        Popup::Login => popups::login::render(f, app),
        Popup::Local => popups::local::render(f, app),
        Popup::Help => popups::help::render(f, app),
        Popup::None => {}
    }
}

pub fn dim(s: impl Into<String>, theme: &crate::theme::Theme) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(theme.fg_dim))
}

/// Map core's abstract spinner palette onto terminal colors.
pub fn to_color(c: nexus_core::app::SpinnerColor) -> Color {
    match c {
        nexus_core::app::SpinnerColor::Green => Color::Green,
        nexus_core::app::SpinnerColor::Cyan => Color::Cyan,
        nexus_core::app::SpinnerColor::Magenta => Color::Magenta,
    }
}

fn render_input(f: &mut Frame, app: &mut AppView, area: Rect) {
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
fn render_command_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
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
fn render_at_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
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

/// Green → yellow → red gradient for `t` in 0.0..=1.0.
fn gradient(t: f64) -> Color {
    // Two linear segments: green→yellow (0..0.5), yellow→red (0.5..1).
    let (r, g) = if t < 0.5 {
        let k = t / 0.5;
        ((40.0 + k * 190.0) as u8, 200u8) // 40→230 red, green steady
    } else {
        let k = (t - 0.5) / 0.5;
        (230u8, (200.0 - k * 190.0) as u8) // red steady, green 200→10
    };
    Color::Rgb(r, g, 40)
}

/// Compact token counts: 940, 1.2k, 128k, 1.0m.
fn humanize(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    }
}

/// `$0.0042`, `$1.23`, `$0.000012`, or `—` when the price is unknown.
/// Small-but-real costs keep enough decimals that they never read `$0.0000`.
fn fmt_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) if c > 0.0 && c < 1.0 => {
            let four = format!("${c:.4}");
            if four == "$0.0000" {
                format!("${c:.6}")
            } else {
                four
            }
        }
        Some(c) => format!("${c:.2}"),
        None => "—".to_string(),
    }
}

fn render_status(f: &mut Frame, app: &AppView, area: Rect) {
    use nexus_core::db::DEFAULT_SPACE;
    let theme = &app.theme;
    let dim = Style::default().fg(theme.fg_dim);
    let sep = || Span::styled(" · ", Style::default().fg(theme.border_dim));

    // Left: the model (accent), then the quiet mode tags.
    let model = app
        .current_model
        .as_deref()
        .map_or_else(|| "no model".to_string(), short_model_label);
    let mut tags: Vec<String> = Vec::new();
    if app.active_space.name != DEFAULT_SPACE {
        tags.push(format!("⌂ {}", app.active_space.name));
    }
    if app.web_mode {
        tags.push("🌐 web".into());
    }
    if app.incognito {
        tags.push("🕶 incognito".into());
    }
    let badge_max = (area.width as usize * 2 / 5).max(12);
    let tag_w: usize = tags.iter().map(|t| t.chars().count() + 3).sum();
    let model = fit_badge("", &model, badge_max.saturating_sub(tag_w + 2).max(8));
    let mut left = vec![
        Span::styled(" ◆ ", Style::default().fg(theme.accent)),
        Span::styled(
            model,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    for tag in tags {
        left.push(sep());
        left.push(Span::styled(tag, Style::default().fg(theme.accent2)));
    }

    // Right: the context meter and numbers, when the window is known.
    let mut right: Vec<Span> = Vec::new();
    if app.settings.show_stats
        && let Some(limit) = app.context_limit()
    {
        right.extend(context_meter(app, limit, 10));
        right.push(Span::raw(" "));
        right.push(Span::styled(context_numbers(app, limit), dim));
    } else if let Some(rate) = app.turn_cache.rate() {
        let partial = if app.turn_cache.is_partial() { "~" } else { "" };
        right.push(Span::styled(
            format!("{partial}{:.0}% cached", rate * 100.0),
            dim,
        ));
    }
    if !right.is_empty() {
        right.push(Span::raw(" "));
    }

    let width_of = |spans: &[Span]| -> u16 {
        u16::try_from(spans.iter().map(Span::width).sum::<usize>()).unwrap_or(u16::MAX)
    };
    let (left_w, right_w) = (width_of(&left), width_of(&right));
    // On a narrow bar the meter yields first, then the status text.
    let right_w = if left_w + right_w + 8 > area.width {
        0
    } else {
        right_w
    };
    let cols = Layout::horizontal([
        Constraint::Length(left_w),
        Constraint::Min(0),
        Constraint::Length(right_w),
    ])
    .split(area);
    f.render_widget(Paragraph::new(Line::from(left)), cols[0]);
    if !app.status.is_empty() {
        let text = popups::chrome::truncate(&app.status, cols[1].width.saturating_sub(3) as usize);
        f.render_widget(
            Paragraph::new(Line::from(vec![sep(), Span::styled(text, dim)])),
            cols[1],
        );
    }
    if right_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(right)).alignment(ratatui::layout::Alignment::Right),
            cols[2],
        );
    }
}

/// `▰▰▰▱▱▱▱▱` — how full the context window is, each filled cell colored by
/// its position along a green → yellow → red ramp.
fn context_meter(app: &AppView, limit: u64, cells: usize) -> Vec<Span<'static>> {
    let ratio = if limit > 0 {
        (app.context_used() as f64 / limit as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (ratio * cells as f64).ceil() as usize;
    (0..cells)
        .map(|x| {
            if x < filled {
                let t = x as f64 / (cells - 1).max(1) as f64;
                Span::styled("▰", Style::default().fg(gradient(t)))
            } else {
                Span::styled("▱", Style::default().fg(app.theme.border_dim))
            }
        })
        .collect()
}

/// `34% 44k/128k · 82% cached`. The cache rate covers the whole turn, not
/// its last request: a tool loop's final request sits on the longest cached
/// prefix, so reporting it alone would flatter the number. `~` marks a turn
/// some provider did not fully account for.
fn context_numbers(app: &AppView, limit: u64) -> String {
    let used = app.context_used();
    let pct = if limit > 0 {
        used as f64 / limit as f64 * 100.0
    } else {
        0.0
    };
    let mut label = format!("{pct:.0}% {}/{}", humanize(used), humanize(limit));
    if let Some(rate) = app.turn_cache.rate() {
        let partial = if app.turn_cache.is_partial() { "~" } else { "" };
        let _ = std::fmt::Write::write_fmt(
            &mut label,
            format_args!(" · {partial}{:.0}% cached", rate * 100.0),
        );
    }
    label
}

/// `prefix` + `model` within `max` chars. A too-long label shortens the model
/// name but keeps its ` · backend` tag, which is the part that disambiguates.
fn fit_badge(prefix: &str, model: &str, max: usize) -> String {
    let full = format!("{prefix}{model}");
    if full.chars().count() <= max {
        return full;
    }
    let (name, tag) = model
        .rsplit_once(" · ")
        .map_or((model, String::new()), |(n, t)| (n, format!(" · {t}")));
    let room = max.saturating_sub(prefix.chars().count() + tag.chars().count());
    if room < 4 {
        return popups::chrome::truncate(&full, max);
    }
    format!("{prefix}{}{tag}", popups::chrome::truncate(name, room))
}

/// The status-bar name for a model id: the last path segment, with the
/// backend named when it isn't `OpenRouter` — `local:org/Model-7B` reads as
/// `Model-7B · local`. `OpenRouter` ids may contain `:` (`…:free`), so only a
/// known backend prefix is split off.
pub(crate) fn short_model_label(id: &str) -> String {
    let (backend, rest) = match id.split_once(':') {
        Some((tag, rest)) if matches!(tag, "openai" | "opencode" | "go" | "codex" | "local") => {
            (Some(tag), rest)
        }
        _ => (None, id),
    };
    let name = rest.rsplit('/').next().unwrap_or(rest);
    match backend {
        Some(tag) => format!("{name} · {tag}"),
        None => name.to_string(),
    }
}

/// Persistent, direct-click targets for completed chat tasks. The queue keeps
/// every completion; only the newest five are painted to avoid covering a
/// small terminal.
fn render_notifications(f: &mut Frame, app: &mut AppView, area: Rect) {
    app.notification_areas.clear();
    let rows = app.notifications.len().min(5) as u16;
    if rows == 0 || area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.min(64);
    let x = area.x + area.width.saturating_sub(width);
    let start = app.notifications.len().saturating_sub(rows as usize);
    let y = area.y + area.height.saturating_sub(rows);
    for (offset, index) in (start..app.notifications.len()).enumerate() {
        let rect = Rect {
            x,
            y: y + offset as u16,
            width,
            height: 1,
        };
        let notification = &app.notifications[index];
        let glyph = if notification.success { "✓ " } else { "× " };
        let color = if notification.success {
            app.theme.success
        } else {
            app.theme.error
        };
        let label = format!("{glyph}{} — {}", notification.title, notification.text);
        let label: String = label.chars().take(width as usize).collect();
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )))
            .style(app.theme.background_style()),
            rect,
        );
        app.notification_areas.push((rect, index));
    }
}

/// Short absolute timestamp from an rfc3339 string (falls back to the raw text).
pub(super) fn fmt_created(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339).map_or_else(
        |_| rfc3339.to_string(),
        |dt| {
            dt.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        },
    )
}

/// A rect `pct_w` × `pct_h` percent of `area`, centered.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_h) / 2),
            Constraint::Percentage(pct_h),
            Constraint::Percentage((100 - pct_h) / 2),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_w) / 2),
            Constraint::Percentage(pct_w),
            Constraint::Percentage((100 - pct_w) / 2),
        ])
        .split(v[1]);
    h[1]
}
