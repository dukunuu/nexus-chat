//! Bounded component drawings. Child regions reserve every branch's height so
//! local navigation never moves following transcript content.

use super::{
    Control, Entry, accent_color, branch_regions, clipped, control_style, display, glyph,
    tone_color,
};
use crate::theme::Theme;
use nexus_core::interactive::{ChartKind, ChartSeries};
use nexus_core::interactive::{Node, Session, Tone};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

pub(super) fn control_count(node: &Node) -> usize {
    match node {
        Node::Buttons { items, .. } => items.len(),
        Node::Checklist { items } => items.len(),
        Node::Accordion { items } => items.len(),
        Node::Input { .. }
        | Node::Toggle { .. }
        | Node::Select { .. }
        | Node::Stepper { .. }
        | Node::Tabs { .. }
        | Node::Form { .. }
        | Node::Rating { .. }
        | Node::Slider { .. }
        | Node::Code { .. } => 1,
        Node::Table {
            select,
            sortable,
            page_size,
            search,
            ..
        } => usize::from(select.is_some() || *sortable || page_size.is_some() || *search),
        _ => 0,
    }
}
fn branch_height(nodes: &[Node], session: &Session, width: u16) -> u16 {
    branch_regions(session, nodes, Rect::new(0, 0, width, u16::MAX))
        .last()
        .map_or(0, |area| area.bottom())
}
pub(super) fn height(node: &Node, session: &Session, width: u16) -> Option<u16> {
    Some(match node {
        Node::Tabs { items, .. } => {
            1 + items
                .iter()
                .map(|item| branch_height(&item.view, session, width))
                .max()
                .unwrap_or(0)
        }
        Node::Accordion { items } => items
            .iter()
            .map(|item| 1 + branch_height(&item.view, session, width))
            .sum(),
        Node::Form { fields, .. } => 2 + branch_height(fields, session, width),
        Node::Chart { rows, .. } => *rows,
        Node::Timeline { .. }
        | Node::Code { .. }
        | Node::Badge { .. }
        | Node::Meter { .. }
        | Node::Slider { .. }
        | Node::Rating { .. } => node.rows(),
        _ => return None,
    })
}
pub(super) fn image_count(node: &Node) -> usize {
    match node {
        Node::Image { .. } | Node::Hero { image: Some(_), .. } => 1,
        Node::Cards { items } => items.iter().filter(|item| item.image.is_some()).count(),
        Node::Buttons { items, .. } => items.iter().filter(|item| item.image.is_some()).count(),
        _ => 0,
    }
}
pub(super) fn visible_regions(session: &Session, width: u16) -> Vec<(&Node, Rect)> {
    fn visit<'a>(
        session: &'a Session,
        nodes: &'a [Node],
        area: Rect,
        out: &mut Vec<(&'a Node, Rect)>,
    ) {
        for (node, region) in nodes.iter().zip(branch_regions(session, nodes, area)) {
            out.push((node, region));
            let index = session.document().node_index(node);
            match node {
                Node::Tabs { items, .. } => {
                    if let Some(item) = items.get(session.tab(index)) {
                        visit(
                            session,
                            &item.view,
                            body(region, 1, region.height.saturating_sub(1)),
                            out,
                        );
                    }
                }
                Node::Accordion { items } => {
                    let mut offset = 0;
                    for (section, item) in items.iter().enumerate() {
                        let height = branch_height(&item.view, session, region.width);
                        if session.section_open(index, section) {
                            visit(session, &item.view, body(region, offset + 1, height), out);
                        }
                        offset += 1 + height;
                    }
                }
                Node::Form { fields, .. } => visit(
                    session,
                    fields,
                    body(region, 1, region.height.saturating_sub(2)),
                    out,
                ),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(
        session,
        session.document().nodes(),
        Rect::new(2, 2, width.saturating_sub(4), u16::MAX),
        &mut out,
    );
    out
}
fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(area.x, area.y + offset, area.width, 1)
}
fn color(tone: Tone, session: &Session, theme: &Theme) -> Color {
    tone_color(Some(tone), accent_color(session.document(), theme), theme)
}
fn body(area: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(area.x, area.y + offset, area.width, height)
}

#[allow(clippy::too_many_lines)]
pub(super) fn render(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Option<(usize, Vec<(Rect, usize)>)> {
    let index = session.document().node_index(node);
    let mut next = control + control_count(node);
    let mut hits = Vec::new();
    match node {
        Node::Tabs { items, .. } => {
            let count = u16::try_from(items.len()).unwrap_or(1);
            for (tab, item) in items.iter().enumerate() {
                let tab = u16::try_from(tab).unwrap_or(0);
                let start = area.width * tab / count;
                let end = area.width * (tab + 1) / count;
                let selected = session.tab(index) == usize::from(tab);
                Paragraph::new(clipped(
                    &format!(" {} ", display(session, &item.label)),
                    end - start,
                ))
                .style(
                    control_style(theme, focus == Some(control))
                        .fg(if selected { theme.accent } else { theme.fg_dim })
                        .add_modifier(if selected {
                            Modifier::UNDERLINED
                        } else {
                            Modifier::empty()
                        }),
                )
                .render(Rect::new(area.x + start, area.y, end - start, 1), buffer);
            }
            hits.push((row(area, 0), control));
            if let Some(item) = items.get(session.tab(index)) {
                let regions = branch_regions(
                    session,
                    &item.view,
                    body(area, 1, area.height.saturating_sub(1)),
                );
                hits.extend(Entry::branch(
                    &item.view, session, &regions, theme, focus, can_send, buffer, &mut next,
                ));
            }
        }
        Node::Accordion { items } => {
            let mut offset = 0;
            for (section, item) in items.iter().enumerate() {
                let open = session.section_open(index, section);
                let header = row(area, offset);
                Paragraph::new(format!(
                    "{} {}",
                    if open {
                        glyph::EXPANDED
                    } else {
                        glyph::SELECTED
                    },
                    display(session, &item.title)
                ))
                .style(control_style(theme, focus == Some(control + section)))
                .render(header, buffer);
                hits.push((header, control + section));
                let height = branch_height(&item.view, session, area.width);
                if open {
                    let regions =
                        branch_regions(session, &item.view, body(area, offset + 1, height));
                    hits.extend(Entry::branch(
                        &item.view, session, &regions, theme, focus, can_send, buffer, &mut next,
                    ));
                }
                offset += 1 + height;
            }
        }
        Node::Form {
            title,
            fields,
            submit_label,
            action,
        } => {
            Paragraph::new(display(session, title))
                .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD))
                .render(row(area, 0), buffer);
            let regions = branch_regions(
                session,
                fields,
                body(area, 1, area.height.saturating_sub(2)),
            );
            hits.extend(Entry::branch(
                fields, session, &regions, theme, focus, can_send, buffer, &mut next,
            ));
            let submit = row(area, area.height.saturating_sub(1));
            let enabled =
                session.form_valid(index) && (can_send || !session.document().sends(action));
            Paragraph::new(format!(
                "[ {}{} ]",
                display(session, submit_label),
                if session.document().sends(action) {
                    " ↵"
                } else {
                    ""
                }
            ))
            .style(if enabled {
                control_style(theme, focus == Some(control))
            } else {
                Style::default()
                    .fg(theme.fg_dim)
                    .add_modifier(Modifier::DIM)
            })
            .render(submit, buffer);
            hits.push((submit, control));
        }
        Node::Chart {
            kind,
            series,
            labels,
            ..
        } => chart(*kind, series, labels, session, area, theme, buffer),
        Node::Timeline { items } => {
            let mut y = 0;
            for item in items {
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{} {} ", glyph::DOT, display(session, &item.time)),
                        Style::default().fg(color(item.tone, session, theme)),
                    ),
                    Span::styled(display(session, &item.title), Style::default().fg(theme.fg)),
                ]))
                .render(row(area, y), buffer);
                y += 1;
                if let Some(text) = &item.text {
                    Paragraph::new(format!("│ {}", display(session, text)))
                        .style(Style::default().fg(theme.fg_dim))
                        .render(row(area, y), buffer);
                    y += 1;
                }
            }
        }
        Node::Rating { key, label, max } => {
            let value = session.selected_row(key);
            let stars = (0..*max)
                .map(|star| {
                    if usize::from(star) < value {
                        glyph::FAVORITE
                    } else {
                        glyph::RING
                    }
                })
                .collect::<String>();
            Paragraph::new(format!("{}: {stars}", display(session, label)))
                .style(control_style(theme, focus == Some(control)))
                .render(area, buffer);
            hits.push((area, control));
        }
        Node::Code { source, lang } => {
            let lang = lang.as_deref().unwrap_or("plain");
            Paragraph::new(format!("{lang}  [Copy]"))
                .style(control_style(theme, focus == Some(control)))
                .render(row(area, 0), buffer);
            let fence = "`".repeat(
                source
                    .split(|ch| ch != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    .max(3)
                    + 1,
            );
            let highlight_lang =
                if lang.eq_ignore_ascii_case("nexus-ui") || lang.eq_ignore_ascii_case("mermaid") {
                    "plain"
                } else {
                    lang
                };
            let rendered = crate::ui::markdown::render(
                &format!("{fence}{highlight_lang}\n{source}\n{fence}"),
                source
                    .lines()
                    .map(UnicodeWidthStr::width)
                    .max()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_add(4),
                crate::ui::markdown::MdColors {
                    heading: theme.accent,
                    rule: theme.border_dim,
                    code: theme.fg,
                },
            );
            let lines: Vec<_> = rendered
                .lines
                .into_iter()
                .skip(1)
                .take(source.lines().count().max(1))
                .map(|line| {
                    let count = line.spans.len().saturating_sub(2);
                    Line::from(
                        line.spans
                            .into_iter()
                            .skip(1)
                            .take(count)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            Paragraph::new(lines).render(body(area, 1, area.height.saturating_sub(1)), buffer);
            hits.push((row(area, 0), control));
        }
        Node::Badge { text, tone } => {
            Paragraph::new(format!("[{}]", display(session, text)))
                .style(
                    Style::default()
                        .fg(color(*tone, session, theme))
                        .add_modifier(Modifier::BOLD),
                )
                .render(area, buffer);
        }
        Node::Meter {
            label,
            value,
            max,
            tone,
        } => {
            let (value, max) = session.progress(value, max).unwrap_or_default();
            let label = format!("{} {value}/{max}", display(session, label));
            ratatui::widgets::Gauge::default()
                .ratio(if max > 0.0 { value / max } else { 0.0 })
                .label(label)
                .gauge_style(
                    Style::default()
                        .fg(color(*tone, session, theme))
                        .bg(theme.raised),
                )
                .render(area, buffer);
        }
        Node::Slider {
            key,
            label,
            min,
            max,
            ..
        } => {
            let value = session.value(key);
            let ratio = (value.parse::<f64>().unwrap_or(*min) - min) / (max - min);
            let label_width = slider_offset(label, session, area.width);
            Paragraph::new(clipped(
                &format!("{}: {value}", display(session, label)),
                label_width,
            ))
            .style(control_style(theme, focus == Some(control)))
            .render(Rect::new(area.x, area.y, label_width, 1), buffer);
            let track = area.width.saturating_sub(label_width);
            for x in 0..track {
                let thumb =
                    (f64::from(x) - ratio * f64::from(track.saturating_sub(1))).abs() <= 0.5;
                buffer.set_string(
                    area.x + label_width + x,
                    area.y,
                    if thumb { glyph::DOT } else { "─" },
                    control_style(theme, focus == Some(control)),
                );
            }
            hits.push((Rect::new(area.x + label_width, area.y, track, 1), control));
        }
        _ => return None,
    }
    Some((next - control, hits))
}
fn slider_offset(label: &str, session: &Session, width: u16) -> u16 {
    u16::try_from(display(session, label).width() + 6)
        .unwrap_or(u16::MAX)
        .min(width / 2)
}

pub(super) fn click(entry: &mut Entry, control: &Control, pos: Position, area: Rect) -> bool {
    let Ok(session) = &mut entry.session else {
        return true;
    };
    let result = match control {
        Control::Tabs(node) => {
            let Some(Node::Tabs { items, .. }) = session.document().node(*node) else {
                return true;
            };
            let tab = usize::from(pos.x - area.x) * items.len() / usize::from(area.width.max(1));
            session.select_tab(*node, tab)
        }
        Control::Rating(node) => {
            let Some(Node::Rating { label, max, .. }) = session.document().node(*node) else {
                return true;
            };
            let offset = u16::try_from(display(session, label).width() + 2).unwrap_or(u16::MAX);
            let value = pos
                .x
                .saturating_sub(area.x + offset)
                .saturating_add(1)
                .min(*max);
            session.rate(*node, value)
        }
        Control::Slider(node) => session.slide(
            *node,
            f64::from(pos.x - area.x) / f64::from(area.width.saturating_sub(1).max(1)),
        ),
        _ => return false,
    };
    entry.error = result
        .err()
        .map_or_else(String::new, |error| error.to_string());
    true
}

pub(super) fn click_table(
    session: &mut Session,
    node: usize,
    row: u16,
    x: u16,
    area: Rect,
) -> anyhow::Result<()> {
    let Some(Node::Table {
        columns,
        sortable,
        page_size,
        search,
        select,
        ..
    }) = session.document().node(node)
    else {
        return Ok(());
    };
    if *search && row == 0 {
        return session.search_table(node, session.table_state(node).query, true);
    }
    let header = u16::from(*search);
    if row == header && *sortable {
        let lead = u16::from(select.is_some());
        let column = usize::from(x.saturating_sub(lead)) * columns.len()
            / usize::from(area.width.saturating_sub(lead).max(1));
        return session.sort_table(node, column.min(columns.len() - 1));
    }
    if page_size.is_some() && row + 1 == session.document().node(node).expect("table node").rows() {
        return session.page_table(node, x >= area.width / 2);
    }
    if row > header && select.is_some() {
        let rows = session.table_rows(node);
        let size = page_size.unwrap_or(rows.len()).max(1);
        let page = session
            .table_state(node)
            .page
            .min(rows.len().div_ceil(size).saturating_sub(1));
        if let Some(selected) = rows.get(page * size + usize::from(row - header - 1)) {
            return session.select_row(node, *selected);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn chart(
    kind: ChartKind,
    series: &[ChartSeries],
    labels: &[String],
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let values: Vec<Vec<f64>> = series
        .iter()
        .map(|series| {
            series
                .values
                .iter()
                .map(|value| session.number(value).unwrap_or_default())
                .collect()
        })
        .collect();
    let tones: Vec<_> = series
        .iter()
        .map(|item| color(item.tone, session, theme))
        .collect();
    let legend = Line::from(
        series
            .iter()
            .enumerate()
            .map(|(index, item)| {
                Span::styled(
                    format!(
                        "{} {}{} ",
                        glyph::DOT,
                        display(session, &item.name),
                        if kind == ChartKind::Pie {
                            format!(
                                " {}",
                                values[index]
                                    .iter()
                                    .map(|value| value.max(0.0))
                                    .sum::<f64>()
                            )
                        } else {
                            String::new()
                        }
                    ),
                    Style::default().fg(tones[index]),
                )
            })
            .collect::<Vec<_>>(),
    );
    Paragraph::new(legend).render(row(area, 0), buffer);
    if area.width < 16 || area.height < 4 {
        for (index, (item, values)) in series
            .iter()
            .zip(&values)
            .enumerate()
            .take(usize::from(area.height.saturating_sub(1)))
        {
            Paragraph::new(format!(
                "{}: {}",
                display(session, &item.name),
                values.iter().sum::<f64>()
            ))
            .style(Style::default().fg(tones[index]))
            .render(row(area, u16::try_from(index).unwrap_or(0) + 1), buffer);
        }
        return;
    }
    let plot = Rect::new(area.x + 6, area.y + 1, area.width - 6, area.height - 2);
    if kind == ChartKind::Pie {
        ring(&values, &tones, plot, buffer);
    } else {
        let min = values.iter().flatten().copied().fold(0.0, f64::min);
        let max = values
            .iter()
            .flatten()
            .copied()
            .fold(0.0, f64::max)
            .max(min + f64::EPSILON);
        Paragraph::new(clipped(&format!("{max}"), 5))
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(area.x, plot.y, 5, 1), buffer);
        Paragraph::new(clipped(&format!("{min}"), 5))
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(area.x, plot.bottom() - 1, 5, 1), buffer);
        let samples = values.iter().map(Vec::len).max().unwrap_or(1);
        for (index, values) in values.iter().enumerate() {
            for x in 0..plot.width {
                let sample = f64::from(x)
                    * f64::from(u16::try_from(samples.saturating_sub(1)).unwrap_or(0))
                    / f64::from(plot.width.saturating_sub(1).max(1));
                let nearest = (0..samples)
                    .min_by(|a, b| {
                        (f64::from(u16::try_from(*a).unwrap_or(0)) - sample)
                            .abs()
                            .total_cmp(&(f64::from(u16::try_from(*b).unwrap_or(0)) - sample).abs())
                    })
                    .unwrap_or(0);
                let left = (0..samples)
                    .rev()
                    .find(|n| f64::from(u16::try_from(*n).unwrap_or(0)) <= sample)
                    .unwrap_or(0);
                let value = if matches!(kind, ChartKind::Line | ChartKind::Area) {
                    let a = values.get(left).copied().unwrap_or_default();
                    let b = values.get(left + 1).copied().unwrap_or(a);
                    a + (b - a) * (sample - f64::from(u16::try_from(left).unwrap_or(0)))
                } else {
                    values.get(nearest).copied().unwrap_or_default()
                };
                if nearest >= values.len() {
                    continue;
                }
                let ratio = (value - min) / (max - min);
                let level = ratio * f64::from(plot.height.saturating_sub(1));
                let baseline = (-min / (max - min)) * f64::from(plot.height.saturating_sub(1));
                for y in 0..plot.height {
                    let pos = f64::from(plot.height - 1 - y);
                    let point = (pos - level).abs() <= 0.5;
                    let fill = pos >= baseline.min(level) && pos <= baseline.max(level);
                    let visible = match kind {
                        ChartKind::Line => point,
                        ChartKind::Area => point || fill,
                        ChartKind::Scatter => {
                            point
                                && usize::from(x)
                                    == nearest * usize::from(plot.width.saturating_sub(1))
                                        / samples.saturating_sub(1).max(1)
                        }
                        ChartKind::Bar => fill && usize::from(x) % series.len().max(1) == index,
                        ChartKind::Pie => false,
                    };
                    if visible {
                        buffer.set_string(
                            plot.x + x,
                            plot.y + y,
                            if kind == ChartKind::Area && !point {
                                "░"
                            } else if kind == ChartKind::Bar {
                                glyph::SECTION
                            } else {
                                glyph::DOT
                            },
                            Style::default().fg(tones[index]),
                        );
                    }
                }
            }
        }
    }
    let first = labels
        .first()
        .map_or_else(|| "0".into(), |s| display(session, s));
    let last = labels.last().map_or_else(
        || {
            values
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(1)
                .saturating_sub(1)
                .to_string()
        },
        |s| display(session, s),
    );
    let axis = body(area, area.height - 1, 1);
    Paragraph::new(format!("      {}", clipped(&first, plot.width / 2)))
        .style(Style::default().fg(theme.fg_dim))
        .render(axis, buffer);
    Paragraph::new(clipped(&last, plot.width / 2))
        .right_aligned()
        .style(Style::default().fg(theme.fg_dim))
        .render(axis, buffer);
}
fn ring(values: &[Vec<f64>], colors: &[Color], area: Rect, buffer: &mut Buffer) {
    let width = area.width.min(area.height.saturating_mul(2));
    let area = Rect::new(
        area.x + (area.width - width) / 2,
        area.y,
        width,
        area.height,
    );
    let totals: Vec<_> = values
        .iter()
        .map(|values| values.iter().map(|v| v.max(0.0)).sum::<f64>())
        .collect();
    let total: f64 = totals.iter().sum();
    if total <= 0.0 {
        return;
    }
    for y in 0..area.height {
        for x in 0..area.width {
            let dx = (2.0 * f64::from(x) + 1.0 - f64::from(area.width)) / f64::from(area.width);
            let dy = (2.0 * f64::from(y) + 1.0 - f64::from(area.height)) / f64::from(area.height);
            let radius = dx.hypot(dy);
            if !(0.45..=1.0).contains(&radius) {
                continue;
            }
            let angle = (dy.atan2(dx) + std::f64::consts::PI) / std::f64::consts::TAU * total;
            let mut cumulative = 0.0;
            let slice = totals
                .iter()
                .position(|value| {
                    cumulative += value;
                    angle < cumulative
                })
                .unwrap_or(totals.len() - 1);
            buffer.set_string(
                area.x + x,
                area.y + y,
                glyph::SECTION,
                Style::default().fg(colors[slice]),
            );
        }
    }
}
