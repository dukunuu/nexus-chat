//! One offline semantic HTML file, with embedded fonts and media.
use super::super::{
    Doc, Kind, ListItem,
    inline::{self, escape},
    layout::{self, LAYOUT},
    model::Tone,
};
use anyhow::Result;
use base64::Engine as _;
use std::{fmt::Write as _, path::Path};
fn rich(text: &str) -> String {
    let mut out = String::new();
    for run in inline::runs(text) {
        let mut text = escape(&run.text).replace('\n', "<br>");
        if run.code {
            text = format!("<code>{text}</code>");
        }
        if run.bold {
            text = format!("<strong>{text}</strong>");
        }
        if run.italic {
            text = format!("<em>{text}</em>");
        }
        if run.strike {
            text = format!("<s>{text}</s>");
        }
        if let Some(id) = run.footnote {
            text = format!("<sup><a href=\"#note-{}\">{text}</a></sup>", escape(&id));
        }
        if let Some(link) = run.link.filter(|l| {
            l.starts_with("https://")
                || l.starts_with("http://")
                || l.starts_with("mailto:")
                || l.starts_with('#')
        }) {
            text = format!(
                "<a href=\"{}\" rel=\"noreferrer\">{text}</a>",
                escape(&link)
            );
        }
        out.push_str(&text);
    }
    out
}
fn list(items: &[ListItem], ordered: bool) -> String {
    let tag = if ordered { "ol" } else { "ul" };
    let mut out = format!("<{tag}>");
    for item in items {
        let checkbox = item.checked.map_or_else(String::new, |c| {
            format!(
                "<input type=\"checkbox\" disabled{}>",
                if c { " checked" } else { "" }
            )
        });
        let _ = write!(
            out,
            "<li>{checkbox}{}{} </li>",
            rich(&item.text),
            if item.children.is_empty() {
                String::new()
            } else {
                list(&item.children, ordered)
            }
        );
    }
    let _ = write!(out, "</{tag}>");
    out
}
/// Export semantic HTML, with all font/image resources in data URIs.
#[allow(clippy::too_many_lines)]
pub fn export(doc: &Doc, assets: Option<&Path>, warnings: &mut Vec<String>) -> Result<String> {
    let theme = inline::theme(doc);
    let mut css = String::new();
    let db = crate::deck::text::deck_fonts(&theme, assets);
    for family in theme.fonts.values() {
        let query = resvg::usvg::fontdb::Query {
            families: &[resvg::usvg::fontdb::Family::Name(family)],
            ..Default::default()
        };
        if let Some(id) = db.query(&query) {
            if let Some(data) = db.with_face_data(id, |bytes, _| {
                base64::engine::general_purpose::STANDARD.encode(bytes)
            }) {
                let _ = write!(
                    css,
                    "@font-face{{font-family:'{}';src:url(data:font/ttf;base64,{data})}}",
                    family.replace(['\'', '<', '>'], "")
                );
            }
        } else {
            warnings.push(format!(
                "Font {family} is unavailable; HTML uses the system fallback."
            ));
        }
    }
    for (key, selector) in [
        ("title", ".title"),
        ("h1", "h1"),
        ("h2", "h2"),
        ("h3", "h3"),
        ("body", "body"),
        ("quote", "blockquote"),
        ("caption", "figcaption"),
        ("code", "pre,code"),
    ] {
        let font = theme.resolve_font(key, &crate::deck::Font::default());
        let stack = crate::deck::text::font_stack(&theme, &font.family).replace(['<', '>'], "");
        let _ = write!(
            css,
            "{selector}{{font-family:{stack};font-size:{}pt;font-weight:{};font-style:{};line-height:{};color:{}}}",
            font.size,
            font.weight,
            if font.italic { "italic" } else { "normal" },
            font.line_height,
            crate::deck::text::color(&theme, &font.color)
        );
    }
    let c = |role| crate::deck::text::color(&theme, role);
    let tones =
        [Tone::Warn, Tone::Success, Tone::Note]
            .into_iter()
            .fold(String::new(), |mut out, tone| {
                let _ = write!(
                    out,
                    "aside.{}{{border-left-color:{}}}",
                    tone_name(tone),
                    layout::tone_color(&theme, tone)
                );
                out
            });
    let (w, h) = doc.page.size.points();
    let [top, right, bottom, left] = doc.page.margins_pt;
    let _ = write!(
        css,
        "*{{box-sizing:border-box}}body{{background:{};margin:2em auto;max-width:{}pt;padding:0 1em}}p,ul,ol,table,figure,pre,blockquote,aside{{margin:0 0 {}pt}}h1,h2,h3{{margin:{}pt 0 {}pt;break-after:avoid}}ul,ol{{padding-left:{}pt}}blockquote{{padding:{}pt;border-left:{}pt solid {}}}aside.callout{{padding:{}pt;background:{};border-left:{}pt solid {}}}{}li{{margin:0 0 {}pt}}table{{width:100%;border-collapse:collapse}}th,td{{padding:{}pt;border:.5pt solid {}}}thead{{display:table-header-group}}pre{{white-space:pre-wrap;padding:{}pt;background:{}}}figcaption{{margin-top:{}pt}}img{{max-width:100%;height:auto}}figure.inline img{{max-width:50%}}figure.wide img{{max-width:85%}}a{{color:{}}}hr{{border:0;border-bottom:.5pt solid {}}}.page-break{{break-before:page}}@page{{size:{w}pt {h}pt;margin:{top}pt {right}pt {bottom}pt {left}pt}}@media print{{body{{margin:0;padding:0;max-width:none}}figure,pre,aside.callout{{break-inside:avoid}}p,li{{widows:2;orphans:2}}}}",
        c("bg"),
        w - left - right,
        LAYOUT.block_gap,
        LAYOUT.heading_above,
        LAYOUT.heading_below,
        LAYOUT.list_indent,
        LAYOUT.quote_padding,
        LAYOUT.bar_width,
        c("accent"),
        LAYOUT.callout_padding,
        c("surface"),
        LAYOUT.bar_width,
        c("accent"),
        tones,
        LAYOUT.list_gap,
        LAYOUT.cell_padding,
        c("border"),
        LAYOUT.code_padding,
        c("surface"),
        LAYOUT.caption_gap,
        c("accent"),
        c("border")
    );
    let mut body = String::new();
    for block in &doc.blocks {
        let id = escape(&block.id);
        match &block.kind {
            Kind::Title { text } => {
                let _ = write!(body, "<h1 id=\"{id}\" class=\"title\">{}</h1>", rich(text));
            }
            Kind::Heading { level, text } => {
                let _ = write!(body, "<h{level} id=\"{id}\">{}</h{level}>", rich(text));
            }
            Kind::Paragraph { text } => {
                let _ = write!(body, "<p id=\"{id}\">{}</p>", rich(text));
            }
            Kind::Quote { text } => {
                let _ = write!(body, "<blockquote id=\"{id}\">{}</blockquote>", rich(text));
            }
            Kind::Callout { text, tone } => {
                let _ = write!(
                    body,
                    "<aside id=\"{id}\" class=\"callout {}\">{}</aside>",
                    tone_name(*tone),
                    rich(text)
                );
            }
            Kind::Code { text, lang } => {
                let _ = write!(
                    body,
                    "<pre id=\"{id}\"><code class=\"language-{}\">{}</code></pre>",
                    escape(lang),
                    escape(text)
                );
            }
            Kind::List { items, ordered } => {
                let _ = write!(body, "<div id=\"{id}\">{}</div>", list(items, *ordered));
            }
            Kind::Table {
                rows,
                header,
                align,
            } => {
                let _ = write!(body, "<table id=\"{id}\">");
                for (index, row) in rows.iter().enumerate() {
                    if *header && index == 0 {
                        body.push_str("<thead>");
                    } else if index == usize::from(*header) {
                        body.push_str("<tbody>");
                    }
                    body.push_str("<tr>");
                    for (column, cell) in row.iter().enumerate() {
                        let tag = if *header && index == 0 { "th" } else { "td" };
                        let alignment = match align.get(column) {
                            Some(crate::deck::Align::Right) => "right",
                            Some(crate::deck::Align::Center) => "center",
                            _ => "left",
                        };
                        let _ = write!(
                            body,
                            "<{tag} style=\"text-align:{alignment}\">{}</{tag}>",
                            rich(cell)
                        );
                    }
                    body.push_str("</tr>");
                    if *header && index == 0 {
                        body.push_str("</thead>");
                    }
                }
                if rows.len() > usize::from(*header) {
                    body.push_str("</tbody>");
                }
                body.push_str("</table>");
            }
            Kind::Image {
                src,
                alt,
                caption,
                width,
            } => {
                let class = match width {
                    super::super::model::ImageWidth::Inline => "inline",
                    super::super::model::ImageWidth::Wide => "wide",
                    super::super::model::ImageWidth::Full => "full",
                };
                let _ = write!(body, "<figure id=\"{id}\" class=\"{class}\">");
                if let Some(data) = crate::deck::render::image_data(src, assets) {
                    let _ = write!(
                        body,
                        "<img alt=\"{}\" src=\"data:image/png;base64,{data}\">",
                        escape(alt)
                    );
                } else {
                    warnings.push(format!("{id}: image unavailable; exported alt text."));
                    body.push_str(&escape(alt));
                }
                if !caption.is_empty() {
                    let _ = write!(body, "<figcaption>{}</figcaption>", rich(caption));
                }
                body.push_str("</figure>");
            }
            Kind::Chart { chart } => {
                let mut chart_svg = String::new();
                crate::deck::render::chart(
                    &mut chart_svg,
                    chart.chart,
                    &chart.labels,
                    &chart.series,
                    &theme,
                );
                let image = format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"640\" height=\"360\" viewBox=\"0 0 320 180\">{chart_svg}</svg>"
                );
                let data = base64::engine::general_purpose::STANDARD.encode(image);
                let _ = write!(
                    body,
                    "<figure id=\"{id}\"><img alt=\"Chart\" src=\"data:image/svg+xml;base64,{data}\"></figure>"
                );
            }
            Kind::Divider => {
                let _ = write!(body, "<hr id=\"{id}\">");
            }
            Kind::PageBreak => {
                let _ = write!(body, "<div id=\"{id}\" class=\"page-break\"></div>");
            }
            Kind::Toc => {
                let _ = write!(body, "<nav id=\"{id}\" aria-label=\"Contents\"><ol>");
                for heading in doc
                    .blocks
                    .iter()
                    .filter(|b| matches!(b.kind, Kind::Heading { .. }))
                {
                    let _ = write!(
                        body,
                        "<li><a href=\"#{}\">{}</a></li>",
                        escape(&heading.id),
                        rich(heading.text().unwrap_or_default())
                    );
                }
                body.push_str("</ol></nav>");
            }
        }
    }
    if !doc.footnotes.is_empty() {
        body.push_str("<aside aria-label=\"Footnotes\"><ol>");
        for (id, text) in &doc.footnotes {
            let _ = write!(body, "<li id=\"note-{}\">{}</li>", escape(id), rich(text));
        }
        body.push_str("</ol></aside>");
    }
    Ok(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; font-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{css}</style></head><body>{body}</body></html>",
        escape(&doc.title)
    ))
}

fn tone_name(tone: Tone) -> &'static str {
    match tone {
        Tone::Info => "info",
        Tone::Warn => "warn",
        Tone::Success => "success",
        Tone::Note => "note",
    }
}
