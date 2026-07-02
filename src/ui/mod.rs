use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, ModelPanel, Popup};
use crate::provider::Model;

mod history;
use history::render_history;

pub fn render(f: &mut Frame, app: &mut App) {
    // Grow the input box with its wrapped content (1–20 rows) plus 2 for the
    // border. `measure` wants the width the widget renders at, i.e. inside the
    // border (full width - 2).
    let inner_w = f.area().width.saturating_sub(2);
    let content_rows = app.input.measure(inner_w).preferred_rows;
    let input_h = content_rows.saturating_add(2);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),           // history
            Constraint::Length(input_h),  // input (auto-height, max 22)
            Constraint::Length(1),        // status (with inline context bar)
        ])
        .split(f.area());

    render_history(f, app, chunks[0]);
    render_input(f, app, chunks[1]);
    render_status(f, app, chunks[2]);
    // Autocomplete floats above the input; only when no modal popup is open.
    if app.popup == Popup::None {
        render_command_popup(f, app, chunks[1]);
    }

    match app.popup {
        Popup::Model => render_model_popup(f, app),
        Popup::Session => render_session_popup(f, app),
        Popup::Copy => render_copy_popup(f, app),
        Popup::Key => render_key_popup(f, app),
        Popup::Settings => render_settings_popup(f, app),
        Popup::Space => render_space_popup(f, app),
        Popup::Context => render_context_popup(f, app),
        Popup::Skills => render_skills_popup(f, app),
        Popup::None => {}
    }
}

fn render_settings_popup(f: &mut Frame, app: &App) {
    use crate::app::SettingsField;
    use ratatui::widgets::BorderType;
    let area = centered(f.area(), 60, 46);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    // Split "label (description)" into the two display columns.
    let split = |label: &'static str| -> (String, String) {
        match label.split_once(" (") {
            Some((name, rest)) => (name.to_string(), rest.trim_end_matches(')').to_string()),
            None => (label.to_string(), String::new()),
        }
    };
    let name_w = SettingsField::ALL
        .iter()
        .map(|f| split(f.label()).0.chars().count())
        .max()
        .unwrap_or(0);

    // The value cell: a pill for toggles, the typed number (or "default") otherwise.
    let toggle = |b: bool| -> Span<'static> {
        if b {
            Span::styled("● on ", Style::default().fg(Color::Green))
        } else {
            Span::styled("○ off", dim)
        }
    };
    let numeric = |s: &str| -> Span<'static> {
        if s.trim().is_empty() {
            Span::styled("default", dim.add_modifier(Modifier::ITALIC))
        } else {
            Span::styled(s.to_string(), Style::default().fg(Color::Cyan))
        }
    };
    let value = |field: SettingsField| -> Span<'static> {
        match field {
            SettingsField::ShowStats => toggle(app.settings.show_stats),
            SettingsField::ShowReasoning => toggle(app.settings.show_reasoning),
            SettingsField::HideHints => toggle(app.settings.hide_hints),
            SettingsField::Temperature => numeric(&app.settings_inputs[0]),
            SettingsField::TopP => numeric(&app.settings_inputs[1]),
            SettingsField::MaxTokens => numeric(&app.settings_inputs[2]),
            SettingsField::MemoryModel => numeric(&app.memory_model),
            SettingsField::CompactThreshold => numeric(&app.settings_inputs[3]),
            SettingsField::SearxngUrl => numeric(&app.settings_inputs[4]),
            SettingsField::Verbosity => Span::styled(app.verbosity.clone(), Style::default().fg(Color::Cyan)),
            SettingsField::LangsearchKey => numeric(&app.settings_inputs[5]),
            SettingsField::SearchProvider => {
                Span::styled(app.search_provider.clone(), Style::default().fg(Color::Cyan))
            }
        }
    };

    let items: Vec<ListItem> = SettingsField::ALL
        .iter()
        .map(|f| {
            let (name, desc) = split(f.label());
            let mut spans = vec![
                Span::styled(format!("{name:<name_w$}"), Style::default().fg(Color::White)),
                Span::raw("   "),
                value(*f),
            ];
            if !desc.is_empty() {
                spans.push(Span::styled(format!("   {desc}"), dim));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    hint_title(app, " nerd config ", "nerd config — Space toggles · type numbers · Esc saves"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
        )
        // No inverse bar — a cyan arrow + bold row keeps the value colors readable.
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.settings_selected));
    f.render_stateful_widget(list, area, &mut state);
}

/// Outer rects of the model picker's two columns (Favorites, Available).
/// Shared by the renderer and the mouse hit-tester so they always agree.
pub fn model_popup_areas(screen: Rect) -> (Rect, Rect) {
    let outer = centered(screen, 82, 74);
    let cols = Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(outer);
    (cols[0], cols[1])
}

/// The clickable list area inside a bordered popup column.
pub fn list_inner(outer: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(outer)
}

fn render_key_popup(f: &mut Frame, app: &App) {
    let outer = centered(f.area(), 60, 20);
    f.render_widget(Clear, outer);
    // Mask the key so it isn't shown in the clear.
    let masked = "*".repeat(app.key_input.chars().count());
    let para = Paragraph::new(format!("{masked}▏")).block(
        Block::default()
            .borders(Borders::ALL)
            .title(hint_title(app, " OpenRouter key ", "OpenRouter key — Enter to save, Esc to cancel")),
    );
    f.render_widget(para, outer);
}

/// Plain text of a rendered line (span contents concatenated).
pub(crate) fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub(super) fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(Color::DarkGray))
}

pub(super) fn dot() -> Line<'static> {
    Line::from(Span::styled(
        "⏺",
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
    ))
}

fn render_input(f: &mut Frame, app: &mut App, area: Rect) {
    let hint = if app.settings.hide_hints {
        ""
    } else if app.is_streaming() {
        " …working (spinner above) "
    } else {
        " message (Enter to send, /help) "
    };
    // Session title sits in the top-right corner of the input box.
    let name = match &app.session {
        Some(s) => s.title.clone(),
        None => "nexus-chat".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title_top(Line::from(hint))
        .title_top(Line::from(Span::styled(format!(" {name} "), Style::default().fg(Color::Cyan))).right_aligned());
    let inner = block.inner(area);
    app.input_inner = inner; // remembered for mouse click -> cursor mapping
    f.render_widget(block, area);
    // The editor draws its own cursor + selection highlight inside the border.
    f.render_widget(&app.input, inner);
}

/// Slash-command autocomplete: a fuzzy-ranked list floating just above the
/// input box. `/name` in cyan, ≤20-char description dimmed alongside.
fn render_command_popup(f: &mut Frame, app: &App, input_area: Rect) {
    let matches = app.command_matches();
    if matches.is_empty() {
        return;
    }
    let hints = !app.settings.hide_hints;
    let title_rows = if hints { 1 } else { 0 };
    let n = matches.len() as u16;
    let h = n + title_rows;
    let w = input_area.width; // full width, no border
    let y = input_area.y.saturating_sub(h);
    let area = Rect { x: input_area.x, y, width: w, height: h };

    // Pad the `/name` column so every description starts at the same column.
    let name_w = matches.iter().map(|c| c.name().chars().count()).max().unwrap_or(0) + 1;
    let items: Vec<ListItem> = matches
        .iter()
        .map(|c| {
            let name = format!("/{}", c.name());
            ListItem::new(Line::from(vec![
                Span::styled(format!("{name:<name_w$}"), Style::default().fg(Color::Cyan)),
                Span::raw("   "),
                Span::styled(c.desc().to_string(), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let mut block = Block::default();
    if hints {
        block = block.title(Line::from(Span::styled(
            "commands (Tab fill · Enter run)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let mut state = ListState::default();
    state.select(Some(app.command_selected()));
    // Mark the selection by making its text bold + an arrow — no inverse/white bg.
    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut state);
}

/// A filling context-usage bar drawn as a gradient: each filled cell is
/// coloured by its position, green (fresh) sliding through yellow to red
/// (refill) as the bar fills toward the right.
fn render_context_bar(f: &mut Frame, app: &App, area: Rect) {
    let Some(limit) = app.context_limit() else {
        return;
    };
    let ratio = if limit > 0 {
        (app.context_used() as f64 / limit as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let width = area.width as usize;
    if width == 0 {
        return;
    }
    let filled = (ratio * width as f64).round() as usize;

    let mut spans: Vec<Span> = Vec::with_capacity(width);
    for x in 0..width {
        if x < filled {
            // Position along the whole bar → gradient stop.
            let t = if width > 1 { x as f64 / (width - 1) as f64 } else { 0.0 };
            spans.push(Span::styled("█", Style::default().fg(gradient(t))));
        } else {
            spans.push(Span::styled("░", Style::default().fg(Color::DarkGray)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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

/// `"34% 44k/128k"` for the status line, or None when unavailable.
fn context_label(app: &App) -> Option<String> {
    let limit = app.context_limit()?;
    let used = app.context_used();
    let pct = if limit > 0 { used as f64 / limit as f64 * 100.0 } else { 0.0 };
    Some(format!("{pct:.0}% {}/{}", humanize(used), humanize(limit)))
}

/// Compact token counts: 940, 1.2k, 128k.
fn humanize(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    use crate::db::DEFAULT_SPACE;
    let model = app.current_model.as_deref().unwrap_or("(no model)");
    let space_tag = if app.active_space.name != DEFAULT_SPACE {
        format!("[{}] ", app.active_space.name)
    } else {
        String::new()
    };
    let show_bar = app.settings.show_stats && app.context_limit().is_some();

    if !show_bar {
        let text = format!("{space_tag}{model}  |  {}", app.status);
        let para = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, area);
        return;
    }

    // Model, then the gradient context bar beside it, then numbers + status.
    let model_w = model.chars().count() as u16 + 2;
    let gauge_w = 18u16.min(area.width.saturating_sub(model_w + 4));
    let cols = Layout::horizontal([
        Constraint::Length(model_w),
        Constraint::Length(gauge_w),
        Constraint::Min(0),
    ])
    .split(area);

    let style = Style::default().fg(Color::DarkGray);
    f.render_widget(Paragraph::new(format!("{model} ")).style(style), cols[0]);
    render_context_bar(f, app, cols[1]);
    let tail = match context_label(app) {
        Some(l) => format!(" {l}  |  {}", app.status),
        None => format!("  |  {}", app.status),
    };
    f.render_widget(Paragraph::new(tail).style(style), cols[2]);
}

fn render_model_popup(f: &mut Frame, app: &mut App) {
    let (fav_outer, avail_outer) = model_popup_areas(f.area());
    f.render_widget(Clear, fav_outer);
    f.render_widget(Clear, avail_outer);

    let fav_focused = app.model_focus == ModelPanel::Favorites;
    let fav_title = match app.model_pick_target {
        crate::app::ModelPickTarget::Memory => " ★ Favorites — picking memory model ",
        crate::app::ModelPickTarget::Session => " ★ Favorites ",
    };

    // Favorites column.
    let fav_items = model_items(app, &app.favorite_models());
    let fav_list = panel_list(fav_items, fav_title, fav_focused);
    f.render_stateful_widget(fav_list, fav_outer, &mut app.fav_state);

    // Available column (with the search box in the title).
    let avail_items = model_items(app, &app.available_models());
    let title = if app.settings.hide_hints {
        format!(" Available — search: {}▏ ", app.model_filter)
    } else {
        format!(
            " Available — search: {}▏  (Ctrl+S fav · Ctrl+T reason) ",
            app.model_filter
        )
    };
    let avail_list = panel_list(avail_items, &title, !fav_focused);
    f.render_stateful_widget(avail_list, avail_outer, &mut app.avail_state);
}

fn model_items(app: &App, models: &[&Model]) -> Vec<ListItem<'static>> {
    models
        .iter()
        .map(|m| {
            let marker = if app.favorites.contains(&m.id) {
                "★ "
            } else if app.last_used.contains_key(&m.id) {
                "• "
            } else {
                "  "
            };
            // Reasoning badge: [r:high] if set, [r] if supported but off.
            let badge = match app.reasoning_of(&m.id) {
                Some(effort) => format!("  [r:{effort}]"),
                None if m.supports_reasoning => "  [r]".to_string(),
                None => String::new(),
            };
            ListItem::new(format!("{marker}{}{badge}", m.id))
        })
        .collect()
}

fn panel_list<'a>(items: Vec<ListItem<'a>>, title: &str, focused: bool) -> List<'a> {
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title.to_string()),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ")
}

fn render_session_popup(f: &mut Frame, app: &App) {
    use crate::app::SessionMode;
    let area = centered(f.area(), 64, 74);
    f.render_widget(Clear, area);

    let sessions = app.filtered_sessions();
    let width = area.width.saturating_sub(4) as usize; // inside border + highlight symbol
    let dim = Style::default().fg(Color::DarkGray);

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            // id on top (model-generated slug, else a uuid prefix), with the
            // created-at date right-aligned on the same row.
            let id = s.slug.clone().unwrap_or_else(|| format!("{}…", &s.id[..8.min(s.id.len())]));
            let when = fmt_created(&s.created_at);
            let gap = width.saturating_sub(id.chars().count() + 1 + when.chars().count() + 2);
            let top = Line::from(vec![
                Span::styled(format!("#{id}"), Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            // title (truncated) with the model dimmed after it.
            let title = truncate(&s.title, width.saturating_sub(s.model.chars().count() + 5));
            let body = Line::from(vec![
                Span::styled(title, Style::default().fg(Color::White)),
                Span::styled(format!("  {}", s.model), dim),
            ]);
            ListItem::new(vec![top, body, Line::from("")])
        })
        .collect();

    // Title bar doubles as the search box / rename field / delete prompt.
    let title = match app.session_mode {
        SessionMode::Rename => format!(" rename: {}▏  (Enter save · Esc cancel) ", app.session_edit),
        SessionMode::ConfirmDelete => {
            let name = app.selected_session().map(|s| s.title).unwrap_or_default();
            format!(" delete \"{}\"? (Ctrl+D confirm · Esc cancel) ", truncate(&name, 30))
        }
        SessionMode::Browse => {
            let keys = if app.settings.hide_hints { "" } else { "  (Ctrl+R rename · Ctrl+D delete)" };
            format!(" session — search: {}▏{keys} ", app.session_filter)
        }
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !sessions.is_empty() {
        state.select(Some(app.session_selected.min(sessions.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Short absolute timestamp from an rfc3339 string (falls back to the raw text).
fn fmt_created(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%b %-d, %H:%M").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

/// Truncate `s` to `max` chars, appending `…` when it overflows.
fn truncate(s: &str, max: usize) -> String {
    let max = max.max(1);
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
}

fn render_space_popup(f: &mut Frame, app: &App) {
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    let area = centered(f.area(), 50, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    let spaces = app.filtered_spaces();
    let items: Vec<ListItem> = spaces
        .iter()
        .map(|s| {
            let n = app.db.count_sessions(&s.id).unwrap_or(0);
            let mark = if s.name == app.active_space.name { "● " } else { "  " };
            let name = if s.name == DEFAULT_SPACE {
                format!("{mark}{} (default)", s.name)
            } else {
                format!("{mark}{}", s.name)
            };
            let line = Line::from(vec![
                Span::styled(name, Style::default().fg(Color::White)),
                Span::styled(format!("  {n} session{}", if n == 1 { "" } else { "s" }), dim),
                Span::styled(format!("  · {}", fmt_created(&s.created_at)), dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = match app.space_mode {
        SpaceMode::Create => format!(" new space: {}▏  (Enter create · Esc cancel) ", app.space_edit),
        SpaceMode::Rename => format!(" rename: {}▏  (Enter save · Esc cancel) ", app.space_edit),
        SpaceMode::ConfirmDelete => {
            let name = app.selected_space().map(|s| s.name).unwrap_or_default();
            format!(" delete \"{name}\"? sessions move to default. (Ctrl+D confirm · Esc cancel) ")
        }
        SpaceMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+N new · Ctrl+R rename · Ctrl+D delete · Ctrl+E instructions · Ctrl+K memory)"
            };
            format!(" space — search: {}▏{keys} ", app.space_filter)
        }
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !spaces.is_empty() {
        state.select(Some(app.space_selected.min(spaces.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_skills_popup(f: &mut Frame, app: &App) {
    use crate::app::SkillsMode;
    let area = centered(f.area(), 56, 50);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = app
        .skills
        .iter()
        .map(|s| {
            ListItem::new(Line::from(vec![
                Span::styled(s.name.clone(), Style::default().fg(Color::White)),
                Span::styled(format!("  {}", s.description), dim),
            ]))
        })
        .collect();

    let title = match app.skills_mode {
        SkillsMode::Install => {
            format!(" install: {}▏  owner/repo/path  (Enter install · Esc cancel) ", app.skills_edit)
        }
        SkillsMode::ConfirmRemove => {
            let name = app.skills.get(app.skills_selected).map(|s| s.name.as_str()).unwrap_or("");
            format!(" remove \"{name}\"? (Ctrl+D confirm · Esc cancel) ")
        }
        SkillsMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+N install from GitHub · Ctrl+D remove · Ctrl+E edit)"
            };
            format!(" skills{keys} ")
        }
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.skills.is_empty() {
        state.select(Some(app.skills_selected.min(app.skills.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Context breakdown popup (Ctrl+I): estimated tokens spent on system
/// instructions, memory, conversation, and (pending) skills.
fn render_context_popup(f: &mut Frame, app: &App) {
    let area = centered(f.area(), 56, 40);
    f.render_widget(Clear, area);
    let b = app.context_breakdown();
    let dim = Style::default().fg(Color::DarkGray);

    let pct_of = |tok: u64| -> String {
        match b.limit.filter(|&l| l > 0) {
            Some(l) => format!(" ({}%)", tok * 100 / l),
            None => String::new(),
        }
    };
    let row = |label: &'static str, tok: u64, color: Color| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<13}"), Style::default().fg(Color::White)),
            Span::styled(humanize(tok), Style::default().fg(color)),
            Span::styled(pct_of(tok), dim),
        ])
    };

    let mut lines = vec![
        row("System", b.system_tokens, Color::Cyan),
        row("Memory", b.memory_tokens, Color::Magenta),
        row("Skills", b.skills_tokens, Color::Yellow),
        row("Conversation", b.conversation_tokens, Color::Green),
    ];
    if b.compacted {
        lines.push(Line::from(Span::styled(
            "  ⤷ this session has been auto-compacted — press v to view/edit the digest",
            dim,
        )));
    }
    lines.push(Line::from(""));
    let total = b.system_tokens + b.memory_tokens + b.skills_tokens + b.conversation_tokens;
    let limit_s = b.limit.map(humanize).unwrap_or_else(|| "?".to_string());
    lines.push(Line::from(vec![
        Span::styled("Total        ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} / {}", humanize(total), limit_s), Style::default().fg(Color::Yellow)),
    ]));

    let hint = if b.compacted {
        "context — v views digest, Ctrl+G toggles, Esc closes"
    } else {
        "context — Ctrl+G toggles, Esc closes"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(hint_title(app, " context ", hint));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_copy_popup(f: &mut Frame, app: &App) {
    let area = centered(f.area(), 50, 60);
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .copy_options
        .iter()
        .map(|o| ListItem::new(o.label.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(hint_title(app, " copy ", "copy — ↑/↓, Enter, Esc")),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    let mut state = ListState::default();
    if !app.copy_options.is_empty() {
        state.select(Some(app.copy_selected.min(app.copy_options.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A popup title: `plain` when hints are hidden, otherwise `" {with_hint} "`.
fn hint_title(app: &App, plain: &str, with_hint: &str) -> String {
    if app.settings.hide_hints {
        plain.to_string()
    } else {
        format!(" {with_hint} ")
    }
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
