//! Word design extraction, resolving style inheritance by id and built-in names.
use crate::{
    deck::{
        Font,
        import::{pptx::package::Package, xml::El},
    },
    design::{Design, derive_styles},
    doc::model::Paper,
};
use anyhow::Result;

fn val<'a>(el: &'a El, child: &str) -> Option<&'a str> {
    el.kid(child)?.attr_local("val")
}
fn number(el: &El, key: &str) -> Option<f64> {
    el.attr_local(key)?
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}

pub(super) fn role(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Some("body"),
        "title" => Some("title"),
        "heading 1" => Some("h1"),
        "heading 2" => Some("h2"),
        "heading 3" => Some("h3"),
        "quote" => Some("quote"),
        "caption" => Some("caption"),
        "code" | "html preformatted" => Some("code"),
        _ => None,
    }
}
pub(super) fn monospace(name: &str) -> bool {
    ["mono", "consolas", "courier", "menlo", "monaco"]
        .iter()
        .any(|m| name.to_ascii_lowercase().contains(m))
}

fn font(style: &El, design: &Design) -> Font {
    let Some(run) = style.kid("rPr") else {
        return Font::default();
    };
    let family = run.kid("rFonts").and_then(|f| {
        f.attr_local("ascii").map(str::to_string).or_else(|| {
            f.attr_local("asciiTheme").map(|theme| {
                if theme.starts_with("major") {
                    "heading".into()
                } else {
                    "body".into()
                }
            })
        })
    });
    let family = family.map(|f| {
        design
            .fonts
            .iter()
            .find(|(_, v)| **v == f)
            .map_or(f.clone(), |(k, _)| k.clone())
    });
    let on = |name| {
        run.kid(name)
            .map(|el| !matches!(el.attr_local("val"), Some("0" | "false" | "off")))
    };
    Font {
        family,
        size: run
            .kid("sz")
            .and_then(|s| number(s, "val"))
            .map(|n| n / 2.0),
        weight: on("b").map(|b| if b { 700 } else { 400 }),
        italic: on("i"),
        color: run.kid("color").and_then(|c| {
            c.attr_local("themeColor")
                .and_then(|name| match name {
                    "accent1" => Some("accent"),
                    "text1" | "dark1" => Some("fg"),
                    "background1" | "light1" => Some("bg"),
                    "text2" | "dark2" => Some("muted"),
                    _ if design.colors.contains_key(name) => Some(name),
                    _ => None,
                })
                .map(str::to_string)
                .or_else(|| {
                    c.attr_local("val")
                        .filter(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_hexdigit()))
                        .map(|v| format!("#{v}"))
                })
        }),
        ..Font::default()
    }
}
fn inherited(style: &El, styles: &El, design: &Design, depth: usize) -> Font {
    let base = if depth < 20 {
        val(style, "basedOn")
            .and_then(|id| {
                styles
                    .kids_named("style")
                    .find(|s| s.attr_local("styleId") == Some(id))
            })
            .map(|s| inherited(s, styles, design, depth + 1))
            .unwrap_or_default()
    } else {
        Font::default()
    };
    base.merged(&font(style, design))
}
fn field_name(code: &str) -> Option<&'static str> {
    match code
        .split_whitespace()
        .next()?
        .to_ascii_uppercase()
        .as_str()
    {
        "PAGE" => Some("{page}"),
        "NUMPAGES" => Some("{pages}"),
        "TITLE" => Some("{title}"),
        _ => None,
    }
}
fn header_text(root: &El, warnings: &mut Vec<String>) -> String {
    fn walk(
        el: &El,
        out: &mut String,
        in_field: &mut bool,
        code: &mut String,
        warnings: &mut Vec<String>,
    ) {
        if el.name == "fldSimple" {
            if let Some(value) = el.attr_local("instr").and_then(field_name) {
                out.push_str(value);
            } else {
                warnings.push("Unsupported header/footer field omitted from preview".into());
            }
            return;
        }
        if el.name == "fldChar" {
            match el.attr_local("fldCharType") {
                Some("begin") => {
                    *in_field = true;
                    code.clear();
                }
                Some("separate") => {
                    if let Some(value) = field_name(code) {
                        out.push_str(value);
                    } else {
                        warnings
                            .push("Unsupported header/footer field omitted from preview".into());
                    }
                }
                Some("end") => *in_field = false,
                _ => {}
            }
        }
        if el.name == "instrText" {
            code.push_str(&el.text);
        }
        if el.name == "t" && !*in_field {
            out.push_str(&el.text);
        }
        if matches!(el.name.as_str(), "drawing" | "pict" | "tbl") {
            warnings.push(
                "Header/footer artwork and tables are retained in Word, but omitted from preview"
                    .into(),
            );
        }
        for child in &el.kids {
            walk(child, out, in_field, code, warnings);
        }
        if el.name == "p" && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    let mut out = String::new();
    walk(root, &mut out, &mut false, &mut String::new(), warnings);
    out.trim_end().into()
}

#[allow(clippy::too_many_lines)] // One bounded Word-to-design conversion.
pub(super) fn extract(bytes: &[u8]) -> Result<(Design, Vec<String>)> {
    let mut pkg = Package::open(bytes)?;
    let mut design = Design::default();
    let mut warnings = Vec::new();
    if pkg.has("word/theme/theme1.xml") {
        let theme = crate::deck::import::pptx::theme_tokens(&pkg.xml("word/theme/theme1.xml")?);
        design.colors = theme.colors;
        design.fonts = theme.fonts;
    } else {
        warnings.push("No Word theme; default tokens used".into());
    }
    let styles = if pkg.has("word/styles.xml") {
        pkg.xml("word/styles.xml")?
    } else {
        warnings.push("No Word styles; default text styles used".into());
        El::default()
    };
    let defaults = styles
        .path(&["docDefaults", "rPrDefault"])
        .map(|el| font(el, &design))
        .unwrap_or_default();
    let normal = styles
        .kids_named("style")
        .find(|s| val(s, "name").is_some_and(|name| name.eq_ignore_ascii_case("normal")));
    let body = normal
        .map(|s| defaults.merged(&inherited(s, &styles, &design, 0)))
        .unwrap_or(defaults);
    design.scale.base_pt = body.size.unwrap_or(11.0).clamp(1.0, 200.0);
    (design.slide.text_styles, design.doc.text_styles) = derive_styles(design.scale, &design.fonts);
    let mut unmapped = 0;
    for style in styles.kids_named("style") {
        let name = val(style, "name").unwrap_or("");
        let extracted = body.merged(&inherited(style, &styles, &design, 0));
        let mono = extracted.family.as_deref().is_some_and(monospace);
        if let Some(role) = role(name).or(if mono { Some("code") } else { None }) {
            let base = design
                .doc
                .text_styles
                .get(role)
                .cloned()
                .unwrap_or_default();
            design
                .doc
                .text_styles
                .insert(role.into(), base.merged(&extracted));
            if style.path(&["pPr", "spacing"]).is_some() {
                warnings.push(format!("{name}: paragraph spacing is retained in Word; preview uses doc::LAYOUT (no spacing overrides in schema)"));
            }
            if style.kid("rPr").is_some_and(|r| {
                r.kids
                    .iter()
                    .any(|k| !matches!(k.name.as_str(), "rFonts" | "sz" | "b" | "i" | "color"))
            }) || style
                .kid("pPr")
                .is_some_and(|p| p.kids.iter().any(|k| k.name != "spacing"))
            {
                warnings.push(format!("{name}: additional style properties are retained in Word, but omitted from preview"));
            }
        } else {
            unmapped += 1;
        }
    }
    if unmapped > 0 {
        warnings.push(format!("{unmapped} other Word styles are not mapped to design roles; retained in the template package"));
    }
    for (role, exponent) in [("title", 4.0), ("h1", 3.0)] {
        if let Some(size) = design.doc.text_styles[role]
            .size
            .filter(|s| *s > design.scale.base_pt)
        {
            design.scale.ratio = (size / design.scale.base_pt)
                .powf(1.0 / exponent)
                .clamp(1.0, 2.0);
            break;
        }
    }
    design.slide.text_styles = derive_styles(design.scale, &design.fonts).0;
    let document = pkg.xml("word/document.xml")?;
    let mut sections = Vec::new();
    document.descendants("sectPr", &mut sections);
    if let Some(section) = sections.last() {
        if let Some(size) = section.kid("pgSz") {
            let w = number(size, "w").unwrap_or(11906.0) / 20.0;
            let h = number(size, "h").unwrap_or(16838.0) / 20.0;
            design.doc.page.size = if (w - 612.0).abs() < 1.0 && (h - 792.0).abs() < 1.0 {
                Paper::Letter
            } else if (w - 595.28).abs() < 1.0 && (h - 841.89).abs() < 1.0 {
                Paper::A4
            } else {
                Paper::Custom { w, h }
            };
        }
        if let Some(margins) = section.kid("pgMar") {
            for (index, name) in ["top", "right", "bottom", "left"].iter().enumerate() {
                if let Some(value) = number(margins, name) {
                    design.doc.page.margins_pt[index] = value / 20.0;
                }
            }
        }
        let rels = pkg.rels("word/document.xml");
        for (kind, target) in [
            ("headerReference", &mut design.doc.page.header),
            ("footerReference", &mut design.doc.page.footer),
        ] {
            target.clear();
            if let Some(reference) = section
                .kids_named(kind)
                .find(|r| r.attr_local("type") == Some("default"))
                && let Some(rel) = reference
                    .rel("id")
                    .and_then(|id| rels.get(id))
                    .filter(|r| !r.external)
            {
                *target = header_text(&pkg.xml(&rel.target)?, &mut warnings);
            }
        }
        if sections.len() > 1 {
            warnings.push("Multiple sections: only final page setup imported".into());
        }
    }
    warnings.sort();
    warnings.dedup();
    Ok((design, warnings))
}
