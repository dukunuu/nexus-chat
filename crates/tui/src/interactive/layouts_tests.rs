use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

fn doc(frame: &str, accent: &str, view: Value) -> String {
    json!({"version":1,"title":"Overview","frame":frame,"accent":accent,"view":view}).to_string()
}
fn entry(source: &str) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(source).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    }
}
fn buffer_text(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}
fn render_at(source: &str, width: u16) -> (Buffer, Vec<String>) {
    let (canvas, _) = entry(source).canvas(width, &Theme::default(), false, true);
    let rows = buffer_text(&canvas);
    (canvas, rows)
}
fn render(source: &str) -> Vec<String> {
    render_at(source, 80).1
}
/// Expected row of a card: content padded inside the frame's side borders.
fn framed(left: char, content: &str, right: char, width: usize) -> String {
    let pad = width - 2 - content.width();
    format!("{left}{content}{}{right}", " ".repeat(pad))
}

fn rust_vs_go() -> String {
    doc(
        "rounded",
        "accent2",
        json!([
            {"type":"columns","items":[
                {"title":"RUST","items":[
                    {"text":"tiny static binaries","mark":"ok"},
                    {"text":"no runtime","mark":"ok"},
                    {"text":"slow compiles","mark":"no"}]},
                {"title":"GO","items":[
                    {"text":"builds in seconds","mark":"ok"},
                    {"text":"simple concurrency","mark":"ok"},
                    {"text":"GC pauses","mark":"no"}]}]},
            {"type":"callout","tone":"tip","text":"Go if you want to ship this week; Rust if size matters."}
        ]),
    )
}
fn release() -> String {
    doc(
        "heavy",
        "success",
        json!([
            {"type":"stats","items":[
                {"value":"12","label":"commits"},
                {"value":"3","label":"fixes"},
                {"value":"0","label":"regressions"},
                {"value":"1.4k","label":"tests"}]},
            {"type":"steps","items":[
                {"title":"readable tool calls","state":"done"},
                {"title":"steady context bar","state":"done"},
                {"title":"web editors","state":"todo"}]}
        ]),
    )
}
fn cards() -> String {
    doc(
        "double",
        "accent",
        json!([{"type":"cards","items":[
            {"title":"Speed","text":"Builds finish in seconds.","badge":"fast","tone":"success"},
            {"title":"Safety","text":"No data races at all."},
            {"title":"Size","text":"Small static binaries.","tone":"warning"},
            {"title":"Cost","text":"Slower compile times."}]}]),
    )
}
fn hero() -> String {
    doc(
        "dashed",
        "warning",
        json!([
            {"type":"hero","title":"Nexus 0.2","subtitle":"Designed replies land today","badge":"NEW"},
            {"type":"divider","label":"highlights"},
            {"type":"quote","text":"Simple things stay simple.","by":"Ada"}
        ]),
    )
}

#[test]
fn columns_and_callout_render_at_80_columns_in_a_rounded_frame() {
    let rows = render(&rust_vs_go());
    let mut expected = vec![
        format!("╭─ Overview {}╮", "─".repeat(80 - "╭─ Overview ╮".width())),
        framed('│', "", '│', 80),
        framed('│', " RUST                                   GO", '│', 80),
        framed(
            '│',
            " ✓ tiny static binaries                 ✓ builds in seconds",
            '│',
            80,
        ),
        framed(
            '│',
            " ✓ no runtime                           ✓ simple concurrency",
            '│',
            80,
        ),
        framed(
            '│',
            " ✗ slow compiles                        ✗ GC pauses",
            '│',
            80,
        ),
        framed('│', "", '│', 80),
        framed(
            '│',
            " ▌ Go if you want to ship this week; Rust if size matters.",
            '│',
            80,
        ),
        framed('│', "", '│', 80),
    ];
    expected.push(format!("╰{}╯", "─".repeat(78)));
    let trimmed: Vec<_> = expected
        .iter()
        .map(|row| row.trim_end().to_string())
        .collect();
    assert_eq!(rows, trimmed);
    // Static documents carry no expand glyph and no keyboard hint.
    let text = rows.join("\n");
    assert!(
        !text.contains('⤢') && !text.contains("F2 keyboard"),
        "{text}"
    );
}

#[test]
fn accent_colors_come_from_the_theme_roles() {
    let theme = Theme::default();
    for (accent, color) in [
        ("accent", theme.accent),
        ("accent2", theme.accent2),
        ("success", theme.success),
        ("warning", theme.warning),
        ("error", theme.error),
        ("muted", theme.fg_dim),
    ] {
        let source = doc(
            "rounded",
            accent,
            json!([{"type":"callout","tone":"tip","text":"Note"}]),
        );
        let (canvas, _) = entry(&source).canvas(60, &theme, false, true);
        assert_eq!(canvas[(3, 0)].fg, color, "{accent} title");
        assert_eq!(canvas[(2, 2)].fg, color, "{accent} tip rail");
        assert_eq!(canvas[(2, 2)].symbol(), "▌");
        assert_eq!(canvas[(0, 0)].fg, theme.border_dim, "frame stays chrome");
    }
    let danger = doc(
        "rounded",
        "accent2",
        json!([{"type":"callout","tone":"danger","text":"Note"}]),
    );
    assert_eq!(
        entry(&danger).canvas(60, &theme, false, true).0[(2, 2)].fg,
        theme.error
    );
}

#[test]
fn stats_and_steps_render_at_80_columns_in_a_heavy_frame() {
    let rows = render(&release());
    assert_eq!(rows.len(), 8, "{rows:#?}");
    assert_eq!(
        rows[0],
        format!("┏━ Overview {}┓", "━".repeat(80 - "┏━ Overview ┓".width()))
    );
    assert_eq!(rows[7], format!("┗{}┛", "━".repeat(78)));
    assert!(rows[2].starts_with("┃ 12 "), "{}", rows[2]);
    for (value, label) in [
        ("12", "commits"),
        ("3", "fixes"),
        ("0", "regressions"),
        ("1.4k", "tests"),
    ] {
        let column = rows[2].find(value).unwrap();
        assert!(
            rows[3].get(column..).unwrap().starts_with(label),
            "{rows:#?}"
        );
    }
    assert!(rows[5].contains("● readable tool calls"), "{}", rows[5]);
    assert!(rows[5].contains("○ web editors"), "{}", rows[5]);
    assert!(rows[5].ends_with('┃'));
}

#[test]
fn stat_deltas_use_signed_arrows_in_success_and_error() {
    let theme = Theme::default();
    let source = doc(
        "rounded",
        "accent2",
        json!([{"type":"stats","items":[
            {"value":"9","label":"up","delta":"+12%"},
            {"value":"4","label":"down","delta":"-3"}]}]),
    );
    let (canvas, _) = entry(&source).canvas(80, &theme, false, true);
    let rows = buffer_text(&canvas);
    let up = rows.iter().position(|row| row.contains("▲ 12%")).unwrap();
    let down = rows.iter().position(|row| row.contains("▼ 3")).unwrap();
    let x_up = u16::try_from(rows[up].chars().position(|c| c == '▲').unwrap()).unwrap();
    let x_down = u16::try_from(rows[down].chars().position(|c| c == '▼').unwrap()).unwrap();
    assert_eq!(canvas[(x_up, u16::try_from(up).unwrap())].fg, theme.success);
    assert_eq!(
        canvas[(x_down, u16::try_from(down).unwrap())].fg,
        theme.error
    );
}

#[test]
fn steps_fall_back_to_a_connected_timeline_when_they_do_not_fit() {
    let source = doc(
        "rounded",
        "accent2",
        json!([{"type":"steps","items":[
            {"title":"Design the schema","text":"Nodes and limits","state":"done"},
            {"title":"Build the renderer","state":"current"},
            {"title":"Ship it","state":"todo"}]}]),
    );
    let rows = render_at(&source, 40).1;
    let text = rows.join("\n");
    assert!(text.contains("● Design the schema"), "{text}");
    assert!(text.contains("◉ Build the renderer"), "{text}");
    assert!(text.contains("○ Ship it"), "{text}");
    assert!(text.contains("│ Nodes and limits"), "{text}");
    assert!(rows.iter().any(|row| row.starts_with("│ │")), "{text}");
    // One row when the same steps fit.
    let wide = render_at(&source, 120).1;
    assert!(
        wide.iter()
            .any(|row| row.contains("● Design the schema — Nodes and limits"))
    );
    assert!(
        wide.iter()
            .any(|row| row.contains("◉ Build the renderer   ○ Ship it"))
    );
}

#[test]
fn cards_form_a_bordered_responsive_grid() {
    let rows = render(&cards());
    let text = rows.join("\n");
    assert!(rows[0].starts_with("╔═ Overview "), "{text}");
    // 80 columns fit three cards per row; four cards balance into a 2x2 grid.
    let tops: Vec<_> = rows.iter().filter(|row| row.contains('┌')).collect();
    assert_eq!(tops.len(), 2, "{text}");
    assert_eq!(tops[0].matches('┌').count(), 2, "{text}");
    assert_eq!(tops[1].matches('┌').count(), 2, "{text}");
    assert!(text.contains("[ fast ]"), "{text}");
    assert!(text.contains("Builds finish in"), "{text}");
    let narrow = render_at(&cards(), 30).1;
    assert!(
        narrow
            .iter()
            .filter(|row| row.contains('┌'))
            .all(|row| row.matches('┌').count() == 1)
    );
    let theme = Theme::default();
    let (canvas, _) = entry(&cards()).canvas(80, &theme, false, true);
    let top = rows.iter().position(|row| row.contains('┌')).unwrap();
    let x = u16::try_from(rows[top].chars().position(|c| c == '┌').unwrap()).unwrap();
    assert_eq!(
        canvas[(x, u16::try_from(top).unwrap())].fg,
        theme.border_dim
    );
    // The first card's title uses its tone, the second falls back to the document accent.
    assert_eq!(
        canvas[(x + 2, u16::try_from(top + 1).unwrap())].fg,
        theme.success
    );
}

#[test]
fn hero_with_badge_places_the_badge_on_the_right() {
    let rows = render(&hero());
    let text = rows.join("\n");
    assert!(
        rows[0].starts_with("╌ Overview ")
            || rows[0].starts_with("┌╌ Overview ")
            || rows[0].contains("Overview"),
        "{text}"
    );
    let title = rows.iter().find(|row| row.contains("Nexus 0.2")).unwrap();
    assert!(
        title.trim_end_matches(['╎', ' ']).ends_with("[ NEW ]"),
        "{title}"
    );
    assert!(text.contains("Designed replies land today"), "{text}");
    assert!(text.contains(" highlights "), "{text}");
    assert!(text.contains("│ Simple things stay simple."), "{text}");
    assert!(text.contains("— Ada"), "{text}");
}

#[test]
fn plain_frame_has_only_a_title_rule() {
    let source = doc("plain", "accent2", json!([{"type":"text","text":"Body"}]));
    let rows = render(&source);
    assert_eq!(
        rows[0],
        format!("─ Overview {}", "─".repeat(80 - "─ Overview ".width()))
    );
    assert!(
        rows.iter().all(|row| !row.contains(['│', '╰', '╭'])),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| row.trim() == "Body"));
}

#[test]
fn frames_pick_their_border_glyphs() {
    for (frame, corner, rule) in [
        ("rounded", "╭", "─"),
        ("double", "╔", "═"),
        ("heavy", "┏", "━"),
        ("dashed", "┌", "╌"),
    ] {
        let source = doc(frame, "accent", json!([{"type":"text","text":"Body"}]));
        let rows = render_at(&source, 40).1;
        assert!(
            rows[0].starts_with(&format!("{corner}{rule} Overview ")),
            "{frame}: {}",
            rows[0]
        );
    }
}

#[test]
fn columns_stack_below_sixty_columns() {
    let source = rust_vs_go();
    let wide = render_at(&source, 60).1;
    let row = wide.iter().find(|row| row.contains("RUST")).unwrap();
    assert!(row.contains("GO"), "side by side at 60: {row}");
    let narrow = render_at(&source, 59).1;
    let rust = narrow.iter().position(|row| row.contains("RUST")).unwrap();
    let go = narrow.iter().position(|row| row.contains("GO")).unwrap();
    assert!(go > rust + 3, "{narrow:#?}");
    assert!(!narrow[rust].contains("GO"));
    assert!(narrow[go + 1].contains("✓ builds in seconds"));
}

#[test]
fn reserved_height_equals_drawn_height_at_60_80_and_120() {
    let long =
        "A fairly long sentence that has to wrap at narrower widths so heights must be measured.";
    let sources = [
        rust_vs_go(),
        release(),
        cards(),
        hero(),
        doc(
            "plain",
            "muted",
            json!([{"type":"callout","tone":"warning","title":"Heads up","text":long}]),
        ),
        doc(
            "rounded",
            "error",
            json!([
                {"type":"hero","title":long,"subtitle":long,"badge":"BADGE"},
                {"type":"steps","items":[{"title":long,"text":long,"state":"current"},{"title":"b"}]},
                {"type":"quote","text":long,"by":long},
                {"type":"divider"},
                {"type":"stats","items":[{"value":"1","label":long,"delta":"-2"},{"value":"2","label":"b"},{"value":"3","label":"c"}]}
            ]),
        ),
    ];
    for source in &sources {
        for width in [60, 80, 120] {
            let reserved = source_height(source, width);
            let (canvas, rows) = render_at(source, width);
            assert_eq!(canvas.area.height, reserved, "{width}\n{}", rows.join("\n"));
            // Nothing is clipped: the last content row sits above the bottom padding.
            let regions = node_regions(entry(source).session.as_ref().unwrap(), width);
            assert_eq!(card_height(&regions), reserved);
        }
    }
}

#[test]
fn static_documents_are_not_focusable_expandable_or_pinned() {
    let source = rust_vs_go();
    let placement = Placement {
        row: 0,
        width: 80,
        owner: 1,
        ordinal: 0,
        height: source_height(&source, 80),
        source: source.clone(),
        complete: true,
    };
    let mut widgets = Widgets::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(false);
            widgets.paint(
                f,
                f.area(),
                0,
                std::slice::from_ref(&placement),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
    assert!(widgets.hits.is_empty());
    assert!(
        !widgets.focus_latest(),
        "static layouts are never focusable"
    );
    assert!(!widgets.pinned);
    // Scrolled past, a layout stays in the history instead of pinning over it.
    terminal
        .draw(|f| {
            widgets.begin_frame(false);
            widgets.paint(
                f,
                f.area(),
                30,
                std::slice::from_ref(&placement),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
    assert!(!widgets.pinned);
    let text = buffer_text(terminal.backend().buffer()).join("\n");
    assert!(!text.contains("pinned"), "{text}");
}

#[test]
fn interactive_panels_are_counted_but_layouts_are_not() {
    let mut widgets = Widgets::default();
    for index in 0..100 {
        let source = doc(
            "rounded",
            "accent",
            json!([{"type":"text","text":format!("layout {index}")}]),
        );
        widgets.register(&Key {
            owner: 0,
            ordinal: index,
            source,
        });
    }
    assert_eq!(widgets.entries.len(), 100);
    let counter = include_str!("../../../core/tests/fixtures/interactive/counter.json");
    widgets.register(&Key {
        owner: 9,
        ordinal: 0,
        source: counter.into(),
    });
    assert!(widgets.entries.contains_key(&Key {
        owner: 9,
        ordinal: 0,
        source: counter.into()
    }));
}

#[test]
fn cards_in_a_row_share_one_height_and_badges_sit_on_the_title_row() {
    let lines = render(&cards());
    // Each grid row's two cards start and end on the same rows.
    let tops: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.matches('┌').count() == 2)
        .map(|(row, _)| row)
        .collect();
    let bottoms: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.matches('└').count() == 2)
        .map(|(row, _)| row)
        .collect();
    assert_eq!(tops.len(), 2, "{lines:#?}");
    assert_eq!(bottoms.len(), 2, "{lines:#?}");
    let speed = lines.iter().find(|line| line.contains("Speed")).unwrap();
    assert!(
        speed.contains("[ fast ]"),
        "badge on the title row: {speed}"
    );
}
