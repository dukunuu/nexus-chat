//! `PowerPoint` (`.pptx`) to deck v2. The inverse of `export/pptx`: every
//! shape is placed absolutely (`mode: none`), faithful first; nothing tries
//! to infer auto-layout. What has no deck equivalent is approximated and
//! reported as a warning, never as a failure.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::deck::model::round2;
use crate::deck::validate::MAX_NODES;
use crate::deck::{
    Align, Deck, Fit, Font, Kind, Node, Rect, Sizing, Slide, SlideSize, Theme as DeckTheme,
    Transform, VAlign,
};

use super::Imported;
use super::xml::El;

mod color;
pub(crate) mod package;
mod props;
mod shapes;
mod text;
mod timing;

use color::{Rgba, Theme};
use package::{Package, Rels};
use props::{ListStyle, list_style};

// -- warnings ------------------------------------------------------------------

/// Warnings, each said once: single events with their place, and tallies of
/// kinds of approximation that happen many times.
#[derive(Default)]
struct Warnings {
    lines: Vec<String>,
    tallies: Vec<(String, usize)>,
    names: Vec<(String, BTreeMap<String, usize>)>,
}

impl Warnings {
    pub fn push(&mut self, line: String) {
        if !self.lines.contains(&line) && self.lines.len() < 200 {
            self.lines.push(line);
        }
    }

    pub fn tally(&mut self, what: &str) {
        match self.tallies.iter_mut().find(|(w, _)| w == what) {
            Some((_, n)) => *n += 1,
            None => self.tallies.push((what.to_string(), 1)),
        }
    }

    pub fn names(&mut self, what: &str, name: &str) {
        let slot = if let Some(i) = self.names.iter().position(|(w, _)| w == what) {
            i
        } else {
            self.names.push((what.to_string(), BTreeMap::new()));
            self.names.len() - 1
        };
        *self.names[slot].1.entry(name.to_string()).or_insert(0) += 1;
    }

    fn finish(self) -> Vec<String> {
        let mut out = self.lines;
        for (what, n) in self.tallies {
            out.push(if n > 1 {
                format!("{what} ({n} times)")
            } else {
                what
            });
        }
        for (what, names) in self.names {
            let list = names
                .iter()
                .map(|(name, n)| {
                    if *n > 1 {
                        format!("{name} x{n}")
                    } else {
                        name.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("{what}: {list}"));
        }
        out
    }
}

// -- the shared, read-only environment -----------------------------------------

struct Master {
    root: El,
    rels: Rels,
    theme: Theme,
    title: ListStyle,
    body: ListStyle,
    other: ListStyle,
}

impl Master {
    fn tree(&self) -> Option<&El> {
        self.root.path(&["cSld", "spTree"])
    }
}

struct Layout {
    root: El,
    rels: Rels,
    master: String,
}

impl Layout {
    fn tree(&self) -> Option<&El> {
        self.root.path(&["cSld", "spTree"])
    }
}

/// Deck-wide conversion facts.
struct Env {
    /// Pixels per EMU.
    pub scale: f64,
    pub default_style: ListStyle,
    roles: [(&'static str, Rgba); 4],
    heading: String,
    body: String,
    deck_theme: DeckTheme,
}

fn clean_family(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '"' | '\'' | '\\' | ';' | '<' | '>' | '{' | '}')
        })
        .take(80)
        .collect();
    if clean.trim().is_empty() {
        "Inter".into()
    } else {
        clean
    }
}

impl Env {
    /// A color as the deck stores it: a theme role when it is one, else hex.
    pub fn paint(&self, color: Rgba) -> String {
        if color.a >= 0.995 {
            for (role, known) in &self.roles {
                if known.hex() == color.hex() {
                    return (*role).to_string();
                }
            }
        }
        color.hex()
    }

    /// A family name as the deck stores it: `body`/`heading` when it is one.
    pub fn family_role(&self, name: &str) -> String {
        if name == self.body {
            "body".into()
        } else if name == self.heading {
            "heading".into()
        } else {
            clean_family(name)
        }
    }

    /// The fields of `font` that differ from the named text style.
    pub fn delta(&self, style: &str, font: &Font) -> Font {
        let base = self.deck_theme.resolve_font(style, &Font::default());
        let same_family = font
            .family
            .as_deref()
            .is_none_or(|f| self.deck_theme.family(f) == self.deck_theme.family(&base.family));
        let color_base = self
            .deck_theme
            .colors
            .get(&base.color)
            .unwrap_or(&base.color);
        let color_now = font
            .color
            .as_ref()
            .map(|c| self.deck_theme.colors.get(c).unwrap_or(c));
        let size_same = font.size.is_none_or(|s| (s - base.size).abs() < 0.5);
        Font {
            family: if same_family {
                None
            } else {
                font.family.clone()
            },
            size: if size_same {
                None
            } else {
                font.size.map(round2)
            },
            weight: font.weight.filter(|w| *w != base.weight),
            italic: font.italic.filter(|i| *i != base.italic),
            line_height: font
                .line_height
                .filter(|l| (l - base.line_height).abs() > 0.01)
                .map(round2),
            letter_spacing: font
                .letter_spacing
                .filter(|s| (s - base.letter_spacing).abs() > 0.001)
                .map(|s| (s * 1000.0).round() / 1000.0),
            color: match (font.color.as_ref(), color_now) {
                (Some(c), Some(now)) if !now.eq_ignore_ascii_case(color_base) => Some(c.clone()),
                _ => None,
            },
            align: font.align.filter(|a| *a != base.align),
            valign: font.valign.filter(|v| *v != base.valign),
            transform: font.transform.filter(|t| *t != base.transform),
        }
    }
}

/// What one shape tree is converted against.
#[derive(Clone, Copy)]
struct Cx<'a> {
    pub env: &'a Env,
    pub theme: &'a Theme,
    pub master: &'a Master,
    pub layout_tree: Option<&'a El>,
    pub master_tree: Option<&'a El>,
    pub rels: &'a Rels,
    pub part: &'a str,
    pub slide: usize,
    /// Slide shapes record their ids for animations.
    pub record: bool,
    /// Master and layout shapes skip their placeholders.
    pub decor: bool,
}

/// The layout and master shapes a placeholder inherits from.
struct Chain<'a> {
    pub layout: Option<&'a El>,
    pub master: Option<&'a El>,
}

/// Conversion state that changes as shapes are read.
struct State {
    pkg: Package,
    assets: PathBuf,
    pub warn: Warnings,
    next: u32,
    pub images: HashMap<String, Option<String>>,
    pub placeholder: Option<String>,
    pub spids: HashMap<String, String>,
    budget: usize,
    limit_said: bool,
}

impl State {
    pub fn fresh(&mut self) -> String {
        self.next += 1;
        format!("n{}", self.next)
    }

    /// Reserve room for `n` nodes on this slide.
    pub fn reserve(&mut self, n: usize) -> bool {
        if self.budget >= n {
            self.budget -= n;
            return true;
        }
        if !self.limit_said {
            self.limit_said = true;
            self.warn.push(format!(
                "a slide has more than {MAX_NODES} shapes; the rest were left out"
            ));
        }
        false
    }
}

// -- the package ---------------------------------------------------------------

fn load_master(pkg: &mut Package, part: &str) -> Result<Master> {
    let root = pkg.xml(part)?;
    let rels = pkg.rels(part);
    let theme_xml = rels
        .first("/theme")
        .and_then(|r| pkg.xml(&r.target).ok())
        .unwrap_or_default();
    let theme = Theme::from_xml(&theme_xml, Some(&root));
    let style = |name: &str| {
        root.path(&["txStyles", name])
            .map(|el| list_style(el, &theme))
            .unwrap_or_default()
    };
    let (title, body, other) = (style("titleStyle"), style("bodyStyle"), style("otherStyle"));
    Ok(Master {
        root,
        rels,
        theme,
        title,
        body,
        other,
    })
}

/// The style of the first paragraph level, as a deck text style.
fn style_font(env: &Env, master: &Master, style: &ListStyle, anchor: VAlign) -> Font {
    let mut merged = env.default_style.level(0).clone();
    merged.over(style.level(0));
    let run = &merged.run;
    let size_pt = run.size.unwrap_or(18.0);
    let line = match merged.line {
        Some(props::Line::Pct(p)) => p,
        Some(props::Line::Pts(points)) => points / size_pt / 1.2,
        None => 1.0,
    };
    Font {
        family: run.family.as_ref().map(|f| env.family_role(f)),
        size: Some(round2(size_pt * 12_700.0 * env.scale)),
        weight: Some(if run.bold.unwrap_or(false) { 700 } else { 400 }),
        italic: Some(run.italic.unwrap_or(false)),
        line_height: Some(round2((line * 1.2).clamp(0.5, 5.0))),
        letter_spacing: Some((run.spacing.map_or(0.0, |s| s / 100.0 / size_pt)).clamp(-0.5, 2.0)),
        color: Some(
            env.paint(
                run.color
                    .or_else(|| master.theme.scheme("tx1"))
                    .unwrap_or_else(|| Rgba::rgb(0, 0, 0)),
            ),
        ),
        align: Some(merged.align.unwrap_or(Align::Left)),
        valign: Some(anchor),
        transform: Some(if run.caps.unwrap_or(false) {
            Transform::Uppercase
        } else {
            Transform::None
        }),
    }
}

fn master_anchor(master: &Master, title: bool) -> VAlign {
    let Some(tree) = master.tree() else {
        return VAlign::Top;
    };
    tree.kids_named("sp")
        .find(|sp| {
            let kind = sp
                .path(&["nvSpPr", "nvPr", "ph"])
                .and_then(|p| p.attr("type"))
                .unwrap_or("");
            if title {
                matches!(kind, "title" | "ctrTitle")
            } else {
                kind == "body"
            }
        })
        .and_then(|sp| sp.path(&["txBody", "bodyPr"]))
        .and_then(|b| b.attr("anchor"))
        .map_or(VAlign::Top, |a| match a {
            "ctr" => VAlign::Middle,
            "b" => VAlign::Bottom,
            _ => VAlign::Top,
        })
}

fn deck_theme(env: &mut Env, master: &Master) {
    let font =
        |name: &Option<String>, fallback: &str| clean_family(name.as_deref().unwrap_or(fallback));
    env.heading = font(&master.theme.major, "Inter");
    env.body = font(&master.theme.minor, "Inter");
    let color = |name: &str, fallback: &str| {
        master
            .theme
            .scheme(name)
            .or_else(|| Rgba::parse(fallback))
            .unwrap_or_else(|| Rgba::rgb(0, 0, 0))
    };
    env.roles = [
        ("bg", color("bg1", "#ffffff")),
        ("fg", color("tx1", "#000000")),
        ("accent", color("accent1", "#4472c4")),
        ("muted", color("tx2", "#44546a")),
    ];
    let mut theme = DeckTheme::default();
    for (role, rgba) in &env.roles {
        theme.colors.insert((*role).into(), rgba.hex());
    }
    for n in 2..=6 {
        if let Some(c) = master.theme.scheme(&format!("accent{n}")) {
            theme.colors.insert(format!("accent{n}"), c.hex());
        }
    }
    theme.fonts.insert("heading".into(), env.heading.clone());
    theme.fonts.insert("body".into(), env.body.clone());
    env.deck_theme = theme.clone();
    let title = style_font(env, master, &master.title, master_anchor(master, true));
    let mut body = style_font(env, master, &master.body, master_anchor(master, false));
    // The deck's body style has no bullets; the Markdown does.
    body.family = Some("body".into());
    let mut title = title;
    title.family = Some(title.family.unwrap_or_else(|| "heading".into()));
    theme.text_styles.insert("title".into(), title);
    theme.text_styles.insert("body".into(), body);
    env.deck_theme = theme;
}

struct SlideRef {
    part: String,
    layout: Option<String>,
}

#[derive(Default)]
struct Parts {
    masters: HashMap<String, Master>,
    layouts: HashMap<String, Layout>,
}

impl Parts {
    fn ensure_layout(&mut self, pkg: &mut Package, part: &str) -> bool {
        if self.layouts.contains_key(part) {
            return true;
        }
        let Ok(root) = pkg.xml(part) else {
            return false;
        };
        let rels = pkg.rels(part);
        let Some(master) = rels.first("/slideMaster").map(|r| r.target.clone()) else {
            return false;
        };
        if !self.masters.contains_key(&master) {
            match load_master(pkg, &master) {
                Ok(m) => {
                    self.masters.insert(master.clone(), m);
                }
                Err(_) => return false,
            }
        }
        self.layouts
            .insert(part.to_string(), Layout { root, rels, master });
        true
    }
}

fn core_title(pkg: &mut Package) -> Option<String> {
    let rels = pkg.rels("");
    let part = rels
        .first("/metadata/core-properties")
        .map(|r| r.target.clone())
        .or_else(|| {
            pkg.has("docProps/core.xml")
                .then(|| "docProps/core.xml".to_string())
        })?;
    let root = pkg.xml(&part).ok()?;
    let title = root.kid("title")?.text.trim().to_string();
    (!title.is_empty()).then_some(title)
}

fn notes_text(pkg: &mut Package, rels: &Rels) -> String {
    let Some(rel) = rels.first("/notesSlide") else {
        return String::new();
    };
    let Ok(root) = pkg.xml(&rel.target.clone()) else {
        return String::new();
    };
    let Some(tree) = root.path(&["cSld", "spTree"]) else {
        return String::new();
    };
    let body = tree.kids_named("sp").find(|sp| {
        sp.path(&["nvSpPr", "nvPr", "ph"])
            .is_some_and(|p| p.attr("type") == Some("body"))
    });
    let Some(body) = body.and_then(|sp| sp.kid("txBody")) else {
        return String::new();
    };
    let mut lines: Vec<String> = body
        .kids_named("p")
        .map(|p| {
            p.kids
                .iter()
                .filter(|k| matches!(k.name.as_str(), "r" | "fld"))
                .map(|k| k.kid("t").map_or("", |t| t.text.as_str()))
                .collect::<String>()
        })
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// A slide's background: a color, or a picture at the back of the slide.
enum Background {
    Color(Rgba),
    Picture(String),
}

fn background(cx: &Cx, st: &mut State, root: &El) -> Option<Background> {
    let bg = root.path(&["cSld", "bg"])?;
    if let Some(props) = bg.kid("bgPr") {
        for kid in &props.kids {
            match kid.name.as_str() {
                "solidFill" => return cx.theme.color(kid).map(Background::Color),
                "gradFill" => {
                    st.warn
                        .tally("Gradient fills are drawn as their first color");
                    return kid
                        .kid("gsLst")
                        .and_then(|l| l.kid("gs"))
                        .and_then(|gs| cx.theme.color(gs))
                        .map(Background::Color);
                }
                "pattFill" => {
                    st.warn
                        .tally("Pattern fills are drawn as their foreground color");
                    return kid
                        .kid("fgClr")
                        .and_then(|c| cx.theme.color(c))
                        .map(Background::Color);
                }
                "blipFill" => {
                    let rid = kid.kid("blip").and_then(|b| b.rel("embed"))?;
                    return shapes::picture_src(cx, st, rid, None).map(Background::Picture);
                }
                _ => {}
            }
        }
        return None;
    }
    bg.kid("bgRef")
        .and_then(|r| cx.theme.color(r))
        .map(Background::Color)
}

fn first_title(slide: &Slide) -> Option<String> {
    slide.nodes().into_iter().find_map(|n| match &n.kind {
        Kind::Text { text, style, .. } if style == "title" => text
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(100).collect()),
        _ => None,
    })
}

/// Import a `.pptx` package into a deck, copying its pictures into `assets`.
///
/// # Errors
/// When `bytes` are not a `.pptx` package, or the deck would be invalid or too
/// large. Anything the deck cannot represent becomes a warning instead.
#[allow(clippy::too_many_lines)]
pub fn import_pptx(bytes: &[u8], assets: &Path) -> Result<Imported> {
    let mut pkg = Package::open(bytes)?;
    let root_rels = pkg.rels("");
    let presentation_part = root_rels
        .first("/officeDocument")
        .map(|r| r.target.clone())
        .filter(|p| pkg.has(p))
        .or_else(|| {
            pkg.has("ppt/presentation.xml")
                .then(|| "ppt/presentation.xml".to_string())
        })
        .context("not a PowerPoint presentation: the package has no presentation part")?;
    let presentation = pkg.xml(&presentation_part)?;
    if presentation.name != "presentation" {
        bail!("not a PowerPoint presentation: unexpected main part");
    }
    let presentation_rels = pkg.rels(&presentation_part);
    let (size_x, size_y) = presentation
        .kid("sldSz")
        .map_or((12_192_000.0, 6_858_000.0), |s| {
            (
                s.num("cx").unwrap_or(12_192_000.0),
                s.num("cy").unwrap_or(6_858_000.0),
            )
        });
    if size_x < 1.0 || size_y < 1.0 {
        bail!("the presentation has no slide size");
    }
    let mut slides = Vec::new();
    for id in presentation
        .kid("sldIdLst")
        .map(|l| l.kids_named("sldId"))
        .into_iter()
        .flatten()
    {
        let Some(rel) = id.rel("id").and_then(|r| presentation_rels.get(r)) else {
            continue;
        };
        if !pkg.has(&rel.target) {
            continue;
        }
        slides.push(SlideRef {
            part: rel.target.clone(),
            layout: None,
        });
    }
    if slides.is_empty() {
        bail!("the presentation has no slides");
    }
    let mut parts = Parts::default();
    let mut slide_data = Vec::new();
    for slide in &mut slides {
        let rels = pkg.rels(&slide.part);
        slide.layout = rels.first("/slideLayout").map(|r| r.target.clone());
        if let Some(layout) = slide.layout.clone()
            && !parts.ensure_layout(&mut pkg, &layout)
        {
            slide.layout = None;
        }
        slide_data.push(rels);
    }
    // A deck whose slides name no usable layout falls back to its first master.
    if parts.masters.is_empty() {
        let first = presentation_rels
            .first("/slideMaster")
            .map(|r| r.target.clone())
            .context("the presentation has no slide master")?;
        let master = load_master(&mut pkg, &first)?;
        parts.masters.insert(first, master);
    }
    let first_master = slides
        .iter()
        .find_map(|s| s.layout.as_ref().and_then(|l| parts.layouts.get(l)))
        .map(|l| l.master.clone())
        .or_else(|| parts.masters.keys().next().cloned())
        .context("no slide master")?;
    let scale = 1920.0 / size_x;
    let mut env = Env {
        scale,
        default_style: ListStyle::default(),
        roles: [("bg", Rgba::rgb(255, 255, 255)); 4],
        heading: "Inter".into(),
        body: "Inter".into(),
        deck_theme: DeckTheme::default(),
    };
    let first = &parts.masters[&first_master];
    if let Some(style) = presentation.kid("defaultTextStyle") {
        env.default_style = list_style(style, &first.theme);
    }
    deck_theme(&mut env, first);
    let mut warnings = Warnings::default();
    let height = round2(size_y * scale);
    if !(180.0..=4320.0).contains(&height) {
        warnings.push(format!(
            "the slide shape ({} by {} inches) is outside what decks support and was adjusted",
            round2(size_x / 914_400.0),
            round2(size_y / 914_400.0)
        ));
    }
    let size = SlideSize {
        w: 1920.0,
        h: height.clamp(180.0, 4320.0),
    };
    let mut st = State {
        pkg,
        assets: assets.to_path_buf(),
        warn: warnings,
        next: 0,
        images: HashMap::new(),
        placeholder: None,
        spids: HashMap::new(),
        budget: MAX_NODES,
        limit_said: false,
    };
    let mut out_slides = Vec::new();
    let mut hidden = 0;
    for (index, (slide_ref, rels)) in slides.iter().zip(&slide_data).enumerate() {
        let number = index + 1;
        let root = match st.pkg.xml(&slide_ref.part) {
            Ok(root) => root,
            Err(error) => {
                st.warn
                    .push(format!("slide {number}: could not be read ({error:#})"));
                out_slides.push(Slide::new(&format!("s{number}")));
                continue;
            }
        };
        if root.attr("show") == Some("0") {
            hidden += 1;
        }
        let layout = slide_ref.layout.as_ref().and_then(|l| parts.layouts.get(l));
        let master = layout
            .and_then(|l| parts.masters.get(&l.master))
            .unwrap_or(first);
        out_slides.push(convert_slide(
            &mut st,
            &env,
            master,
            layout,
            &root,
            rels,
            &slide_ref.part,
            number,
            size,
        ));
    }
    if hidden > 0 {
        st.warn.push(format!(
            "{hidden} hidden slide{} imported as normal slides",
            if hidden == 1 { " was" } else { "s were" }
        ));
    }
    let title = core_title(&mut st.pkg)
        .or_else(|| out_slides.iter().find_map(first_title))
        .unwrap_or_else(|| "Imported deck".into());
    let mut theme = env.deck_theme.clone();
    theme.size = size;
    let deck = Deck {
        version: 2,
        title: title.chars().take(120).collect(),
        design: None,
        design_rev: None,
        theme,
        slides: out_slides,
    };
    let warnings = st.warn.finish();
    let json = serde_json::to_string(&deck)?;
    if json.len() > crate::deck::MAX_SOURCE {
        bail!(
            "the imported deck is too large for studio ({} MB of text; the limit is 2 MB)",
            json.len().div_ceil(1024 * 1024)
        );
    }
    let deck = Deck::check(&json).map_err(|errors| {
        anyhow::anyhow!(
            "the imported deck is not valid: {}",
            errors
                .iter()
                .take(5)
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    Ok(Imported { deck, warnings })
}

#[allow(clippy::too_many_arguments)]
fn convert_slide(
    st: &mut State,
    env: &Env,
    master: &Master,
    layout: Option<&Layout>,
    root: &El,
    rels: &Rels,
    part: &str,
    number: usize,
    size: SlideSize,
) -> Slide {
    st.budget = MAX_NODES;
    st.limit_said = false;
    st.spids.clear();
    let mut slide = Slide::new(&format!("s{number}"));
    slide.name = root
        .path(&["cSld"])
        .and_then(|c| c.attr("name"))
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.chars().take(80).collect());
    let slide_cx = Cx {
        env,
        theme: &master.theme,
        master,
        layout_tree: layout.and_then(Layout::tree),
        master_tree: master.tree(),
        rels,
        part,
        slide: number,
        record: true,
        decor: false,
    };
    let mut nodes = Vec::new();
    // Background: the slide's own, else the layout's, else the master's.
    let mut found = background(&slide_cx, st, root);
    if found.is_none()
        && let Some(layout) = layout
    {
        let layout_cx = Cx {
            rels: &layout.rels,
            part: "layout",
            record: false,
            ..slide_cx
        };
        found = background(&layout_cx, st, &layout.root);
    }
    if found.is_none() {
        let master_cx = Cx {
            rels: &master.rels,
            part: "master",
            record: false,
            ..slide_cx
        };
        found = background(&master_cx, st, &master.root);
    }
    match found {
        Some(Background::Color(color)) => {
            let value = env.paint(color);
            slide.fill = (value != "bg").then_some(value);
        }
        Some(Background::Picture(src)) if st.reserve(1) => {
            let mut node = Node::new(
                &st.fresh(),
                Kind::Image {
                    src,
                    fit: Fit::Cover,
                },
            );
            node.name = Some("Background".into());
            node.sizing = Sizing::fixed(size.w, size.h);
            node.position = crate::deck::Position::Absolute { x: 0.0, y: 0.0 };
            nodes.push(node);
        }
        _ => {}
    }
    let space = shapes::Space::root(env.scale);
    // Decoration shared through the layout and master, behind the slide's own.
    if root.attr("showMasterSp") != Some("0")
        && let Some(layout) = layout
    {
        if layout.root.attr("showMasterSp") != Some("0")
            && let Some(tree) = master.tree()
        {
            let cx = Cx {
                rels: &master.rels,
                part: "master",
                record: false,
                decor: true,
                ..slide_cx
            };
            shapes::tree(&cx, st, tree, &space, 0, &mut nodes);
        }
        if let Some(tree) = layout.tree() {
            let cx = Cx {
                rels: &layout.rels,
                part: "layout",
                record: false,
                decor: true,
                ..slide_cx
            };
            shapes::tree(&cx, st, tree, &space, 0, &mut nodes);
        }
    }
    if let Some(tree) = root.path(&["cSld", "spTree"]) {
        shapes::tree(&slide_cx, st, tree, &space, 0, &mut nodes);
    }
    slide.children = nodes;
    slide.notes = notes_text(&mut st.pkg, rels);
    slide.transition = timing::transition(root, &mut st.warn);
    let spids = std::mem::take(&mut st.spids);
    timing::builds(root, &mut slide, &spids, &mut st.warn);
    slide
}

#[allow(dead_code)]
fn unused(_: Rect) {}
