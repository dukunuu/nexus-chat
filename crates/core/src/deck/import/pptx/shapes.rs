//! Shape trees to deck nodes: shapes, text boxes, pictures, connectors,
//! groups, tables, charts and `SmartArt`.

use std::io::Cursor;

use crate::deck::model::round2;
use crate::deck::{AutoLayout, Font, Kind, Node, Position, Rect, Shape, Sizing, Stroke, assets};

use super::super::xml::El;
use super::color::Rgba;
use super::props::run_props;
use super::text::{self, Role};
use super::{Chain, Cx, State};

/// Frames may nest this deep (the deck's own limit is 8).
const MAX_FRAME_DEPTH: usize = 7;
/// Pixels beyond this are clamped; the deck accepts +-20000.
const LIMIT: f64 = 19_000.0;
/// Largest picture, in pixels, that is decoded to crop it.
const MAX_CROP_PIXELS: u64 = 48_000_000;

// -- geometry ------------------------------------------------------------------

/// A shape's `a:xfrm` / `p:xfrm`, in EMU and degrees.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Xfrm {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub rot: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub child_off: (f64, f64),
    pub child_ext: (f64, f64),
}

pub(super) fn xfrm(el: &El) -> Option<Xfrm> {
    let off = el.kid("off")?;
    let ext = el.kid("ext")?;
    let chof = el.kid("chOff");
    let chex = el.kid("chExt");
    let (x, y) = (off.num("x")?, off.num("y")?);
    let (w, h) = (ext.num("cx")?, ext.num("cy")?);
    Some(Xfrm {
        x,
        y,
        w,
        h,
        rot: el.num("rot").unwrap_or(0.0) / 60_000.0,
        flip_h: el.flag("flipH").unwrap_or(false),
        flip_v: el.flag("flipV").unwrap_or(false),
        child_off: chof.map_or((x, y), |c| {
            (c.num("x").unwrap_or(x), c.num("y").unwrap_or(y))
        }),
        child_ext: chex.map_or((w, h), |c| {
            (c.num("cx").unwrap_or(w), c.num("cy").unwrap_or(h))
        }),
    })
}

/// The mapping from a coordinate space (EMU) to pixels inside the nearest
/// frame: `base + (v - origin) * k`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Space {
    pub origin: (f64, f64),
    pub base: (f64, f64),
    pub k: (f64, f64),
}

impl Space {
    pub fn root(scale: f64) -> Self {
        Self {
            origin: (0.0, 0.0),
            base: (0.0, 0.0),
            k: (scale, scale),
        }
    }

    pub fn rect(&self, xf: &Xfrm) -> Rect {
        Rect {
            x: self.base.0 + (xf.x - self.origin.0) * self.k.0,
            y: self.base.1 + (xf.y - self.origin.1) * self.k.1,
            w: xf.w * self.k.0,
            h: xf.h * self.k.1,
        }
    }
}

// -- non-visual properties -----------------------------------------------------

fn nv(el: &El) -> Option<&El> {
    el.kids.iter().find(|k| k.name.starts_with("nv"))
}

fn cnv(el: &El) -> Option<&El> {
    nv(el)?.kid("cNvPr")
}

pub(super) fn ph(el: &El) -> Option<&El> {
    nv(el)?.path(&["nvPr", "ph"])
}

fn name_of(el: &El) -> Option<String> {
    cnv(el)
        .and_then(|c| c.attr("name"))
        .map(|n| n.chars().take(80).collect::<String>())
        .filter(|n| !n.trim().is_empty())
}

/// The shape property element (`p:spPr`, `p:grpSpPr`).
fn sp_pr(el: &El) -> Option<&El> {
    el.kid("spPr").or_else(|| el.kid("grpSpPr"))
}

fn is_footer(ph: &El) -> bool {
    matches!(ph.attr("type"), Some("dt" | "ftr" | "sldNum" | "hdr"))
}

fn ph_kind(ph: &El, to_master: bool) -> String {
    let ty = ph.attr("type").unwrap_or("obj");
    match ty {
        "title" | "ctrTitle" => "title".into(),
        "body" | "subTitle" | "obj" | "tbl" | "chart" | "clipArt" | "dgm" | "media" | "pic"
        | "object"
            if to_master =>
        {
            "body".into()
        }
        other => other.into(),
    }
}

/// The placeholder in `tree` that `want` inherits from.
fn find_ph<'a>(tree: &'a El, want: &El, to_master: bool) -> Option<&'a El> {
    let candidates = || {
        tree.kids
            .iter()
            .filter(|k| matches!(k.name.as_str(), "sp" | "pic" | "graphicFrame"))
            .filter_map(|k| ph(k).map(|p| (k, p)))
    };
    if !to_master
        && let Some(idx) = want.attr("idx").filter(|i| *i != "0")
        && let Some((found, _)) = candidates().find(|(_, p)| p.attr("idx") == Some(idx))
    {
        return Some(found);
    }
    let kind = ph_kind(want, to_master);
    candidates()
        .find(|(_, p)| ph_kind(p, to_master) == kind)
        .map(|(k, _)| k)
}

fn chain_for<'a>(cx: &Cx<'a>, el: &'a El) -> Chain<'a> {
    let Some(want) = ph(el) else {
        return Chain {
            layout: None,
            master: None,
        };
    };
    let layout = cx.layout_tree.and_then(|tree| find_ph(tree, want, false));
    let master = cx
        .master_tree
        .and_then(|tree| find_ph(tree, layout.and_then(ph).unwrap_or(want), true));
    Chain { layout, master }
}

/// A layout placeholder, including empty picture and text slots.
pub(super) fn template_slot(cx: &Cx, st: &mut State, el: &El, slot: &str) -> Option<Node> {
    let placeholder = ph(el)?;
    let chain = chain_for(cx, el);
    let inherited: Vec<_> = std::iter::once(el)
        .chain(chain.layout)
        .chain(chain.master)
        .collect();
    let xf = first_xfrm(&inherited, false)?;
    let rect = Space::root(cx.env.scale).rect(&xf);
    let kind = placeholder.attr("type").unwrap_or("obj");
    let node_kind = if kind == "pic" {
        Kind::Image {
            src: placeholder_src(st)?,
            fit: crate::deck::Fit::Cover,
        }
    } else {
        let mut body = el.kid("txBody").cloned().unwrap_or_default();
        body.name = "txBody".into();
        let mut paragraph = body.kid("p").cloned().unwrap_or_default();
        paragraph.name = "p".into();
        let properties = paragraph
            .find("rPr")
            .or_else(|| paragraph.kid("endParaRPr"))
            .cloned();
        paragraph.kids.retain(|k| k.name == "pPr");
        let mut run =
            crate::deck::import::xml::parse(&format!("<a:r><a:t>{slot}</a:t></a:r>")).ok()?;
        if let Some(mut properties) = properties {
            properties.name = "rPr".into();
            run.kids.insert(0, properties);
        }
        paragraph.kids.push(run);
        body.kids.retain(|k| k.name != "p");
        body.kids.push(paragraph);
        let role = if matches!(kind, "title" | "ctrTitle") {
            Role::Title
        } else {
            Role::Body
        };
        let text = text::build(cx, st, &body, &chain, role, None)?;
        Kind::Text {
            text: slot.into(),
            style: text.style.into(),
            font: text.font,
        }
    };
    if !st.reserve(1) {
        return None;
    }
    let mut node = make(st, node_kind, name_of(el), rect, xf.rot);
    node.slot = slot.into();
    node.source_placeholder = Some(crate::deck::model::Placeholder {
        kind: kind.into(),
        idx: placeholder.attr("idx").unwrap_or("0").into(),
    });
    Some(node)
}

// -- paint ---------------------------------------------------------------------

/// A fill as the file states it.
enum Paint {
    None,
    Color(Rgba),
    /// A radial gradient: stops from the middle (0) to the edge (1).
    Glow(Vec<(f64, Rgba)>),
}

fn paint_of(cx: &Cx, st: &mut State, props: &El) -> Option<Paint> {
    for kid in &props.kids {
        match kid.name.as_str() {
            "noFill" => return Some(Paint::None),
            "solidFill" => return cx.theme.color(kid).map(Paint::Color),
            "gradFill" => {
                let stops: Vec<(f64, Rgba)> = kid
                    .kid("gsLst")
                    .map(|l| l.kids_named("gs"))
                    .into_iter()
                    .flatten()
                    .filter_map(|gs| {
                        Some((
                            gs.num("pos").unwrap_or(0.0) / 100_000.0,
                            cx.theme.color(gs)?,
                        ))
                    })
                    .collect();
                if kid.kid("path").is_some() && stops.len() > 1 {
                    st.warn
                        .tally("Radial gradients are drawn as stacked translucent ellipses");
                    return Some(Paint::Glow(stops));
                }
                if props.name != "spPr"
                    || kid.kid("lin").is_none()
                    || !(2..=8).contains(&stops.len())
                {
                    st.warn
                        .tally("Unsupported gradients are drawn as their first color");
                }
                return stops.first().map(|s| Paint::Color(s.1));
            }
            "pattFill" => {
                st.warn
                    .tally("Pattern fills are drawn as their foreground color");
                return kid
                    .kid("fgClr")
                    .and_then(|c| cx.theme.color(c))
                    .map(Paint::Color);
            }
            "blipFill" => {
                st.warn
                    .tally("Picture fills inside shapes are not imported");
                return Some(Paint::None);
            }
            _ => {}
        }
    }
    None
}

#[derive(Default)]
struct LineSpec {
    paint: Option<Paint>,
    /// EMU.
    width: Option<f64>,
    tail: Option<bool>,
    head: Option<bool>,
}

fn end_arrow(ln: &El, name: &str) -> Option<bool> {
    ln.kid(name)
        .map(|e| e.attr("type").is_some_and(|t| t != "none"))
}

/// What a shape's style references (`p:style`) contribute.
#[derive(Default)]
struct StyleRefs {
    fill: Option<Rgba>,
    line: Option<(Rgba, f64)>,
    font: Option<Rgba>,
}

fn style_refs(cx: &Cx, el: &El) -> StyleRefs {
    let Some(style) = el.kid("style") else {
        return StyleRefs::default();
    };
    let index = |r: &El| r.num("idx").unwrap_or(0.0) as usize;
    StyleRefs {
        fill: style
            .kid("fillRef")
            .filter(|r| index(r) > 0)
            .and_then(|r| cx.theme.color(r)),
        line: style.kid("lnRef").filter(|r| index(r) > 0).and_then(|r| {
            let width = cx
                .theme
                .line_widths
                .get(index(r) - 1)
                .copied()
                .unwrap_or(9525.0);
            cx.theme.color(r).map(|c| (c, width))
        }),
        font: style.kid("fontRef").and_then(|r| cx.theme.color(r)),
    }
}

struct Look {
    fill: Option<Rgba>,
    glow: Option<Vec<(f64, Rgba)>>,
    stroke: Option<(Rgba, f64)>,
    tail: bool,
    head: bool,
}

fn look(cx: &Cx, st: &mut State, chain_els: &[&El], refs: &StyleRefs) -> Look {
    let mut fill = None;
    let mut spec = LineSpec::default();
    for el in chain_els {
        let Some(props) = sp_pr(el) else { continue };
        if props
            .kid("effectLst")
            .is_some_and(|e| e.kids.iter().any(|effect| effect.name != "outerShdw"))
        {
            st.warn
                .tally("Effects other than outer shadows are not imported");
        }
        if fill.is_none() {
            fill = paint_of(cx, st, props);
        }
        if let Some(ln) = props.kid("ln") {
            if spec.paint.is_none() {
                spec.paint = paint_of(cx, st, ln);
            }
            if spec.width.is_none() {
                spec.width = ln.num("w");
            }
            if spec.tail.is_none() {
                spec.tail = end_arrow(ln, "tailEnd");
            }
            if spec.head.is_none() {
                spec.head = end_arrow(ln, "headEnd");
            }
        }
    }
    let mut glow = None;
    let fill = match fill {
        Some(Paint::Color(c)) => Some(c),
        Some(Paint::Glow(stops)) => {
            glow = Some(stops);
            None
        }
        Some(Paint::None) => None,
        None => refs.fill,
    };
    let stroke = match spec.paint {
        Some(Paint::None | Paint::Glow(_)) => None,
        Some(Paint::Color(c)) => Some((c, spec.width.or(refs.line.map(|l| l.1)).unwrap_or(9525.0))),
        None => refs
            .line
            .map(|(c, w)| (c, spec.width.unwrap_or(w)))
            .filter(|(_, w)| *w > 0.0),
    };
    Look {
        fill,
        glow,
        stroke,
        tail: spec.tail.unwrap_or(false),
        head: spec.head.unwrap_or(false),
    }
}

// -- node helpers --------------------------------------------------------------

fn clamp(v: f64) -> f64 {
    round2(v.clamp(-LIMIT, LIMIT))
}

fn make(st: &mut State, kind: Kind, name: Option<String>, r: Rect, rot: f64) -> Node {
    let mut node = Node::new(&st.fresh(), kind);
    node.name = name;
    node.sizing = Sizing::fixed(clamp(r.w.max(0.0)), clamp(r.h.max(0.0)));
    node.position = Position::Absolute {
        x: clamp(r.x),
        y: clamp(r.y),
    };
    let rot = rot.rem_euclid(360.0);
    node.rotation = (rot > 0.01 && rot < 359.99).then(|| round2(rot));
    node
}

fn stroke_of(cx: &Cx, stroke: Option<(Rgba, f64)>) -> Option<Stroke> {
    stroke.map(|(color, width)| Stroke {
        color: cx.env.paint(color),
        width: round2((width * cx.env.scale).clamp(0.5, 200.0)),
        dash: crate::deck::Dash::Solid,
    })
}

fn hidden(el: &El) -> bool {
    cnv(el).and_then(|c| c.flag("hidden")).unwrap_or(false)
}

fn record(cx: &Cx, st: &mut State, el: &El, id: &str) {
    if cx.record
        && let Some(spid) = cnv(el).and_then(|c| c.attr("id"))
    {
        st.spids.insert(spid.to_string(), id.to_string());
    }
}

// -- the tree --------------------------------------------------------------------

/// Convert the children of a shape tree (or group) into nodes appended to `out`.
pub(super) fn tree(
    cx: &Cx,
    st: &mut State,
    root: &El,
    space: &Space,
    depth: usize,
    out: &mut Vec<Node>,
) {
    for el in &root.kids {
        let start = out.len();
        match el.name.as_str() {
            "sp" => shape(cx, st, el, space, out),
            "cxnSp" => connector(cx, st, el, space, out),
            "pic" => picture(cx, st, el, space, out),
            "grpSp" => group(cx, st, el, space, depth, out),
            "graphicFrame" => graphic_frame(cx, st, el, space, out),
            "AlternateContent" => {
                let chosen = el
                    .kid("Fallback")
                    .filter(|f| !f.kids.is_empty())
                    .or_else(|| el.kid("Choice"));
                if let Some(chosen) = chosen {
                    tree(cx, st, chosen, space, depth, out);
                }
            }
            _ => {}
        }
        if let Some(node) = out.get_mut(start) {
            native_effects(cx, el, node);
        }
        // Like the exporter’s Frame background marker, these names identify
        // an otherwise ordinary editable shape as the preceding image's scrim.
        if out.len() == start + 1 && start > 0 {
            let (before, after) = out.split_at_mut(start);
            let (image, overlay) = (&mut before[start - 1], &after[0]);
            if matches!(
                overlay.name.as_deref(),
                Some("Image overlay" | "Background overlay")
            ) && matches!(image.kind, Kind::Image { .. })
                && (image.opacity - 1.0).abs() < f64::EPSILON
                && image.sizing == overlay.sizing
                && image.position == overlay.position
                && image.rotation == overlay.rotation
                && let Kind::Shape {
                    fill: Some(fill), ..
                } = &overlay.kind
            {
                image.overlay = Some(crate::deck::Overlay { fill: fill.clone() });
                let _ = out.pop();
            }
        }
    }
}

fn native_effects(cx: &Cx, el: &El, node: &mut Node) {
    let Some(props) = sp_pr(el) else {
        return;
    };
    if let Some(shadow) = props.path(&["effectLst", "outerShdw"])
        && let Some(color) = cx.theme.color(shadow)
        && matches!(
            node.kind,
            Kind::Frame { .. } | Kind::Shape { .. } | Kind::Image { .. }
        )
    {
        let distance = shadow.num("dist").unwrap_or(0.0) * cx.env.scale;
        let direction = (shadow.num("dir").unwrap_or(0.0) / 60_000.0).to_radians();
        let width = match node.sizing.w {
            crate::deck::Dim::Fixed(width) => width,
            _ => 0.0,
        };
        node.shadow = Some(crate::deck::Shadow::Custom(crate::deck::ShadowSpec {
            x: round2((distance * direction.cos()).clamp(-2000.0, 2000.0)),
            y: round2((distance * direction.sin()).clamp(-2000.0, 2000.0)),
            blur: round2((shadow.num("blurRad").unwrap_or(0.0) * cx.env.scale).clamp(0.0, 200.0)),
            spread: round2(
                ((shadow.num("sx").unwrap_or(100_000.0) / 100_000.0 - 1.0) * width / 2.0)
                    .clamp(-200.0, 200.0),
            ),
            color: crate::deck::Color::Solid(cx.env.paint(color)),
        }));
    }
    let stroke = match &mut node.kind {
        Kind::Shape { stroke, .. } | Kind::Frame { stroke, .. } => stroke.as_mut(),
        _ => None,
    };
    if let Some(stroke) = stroke {
        stroke.dash = match props
            .path(&["ln", "prstDash"])
            .and_then(|dash| dash.attr("val"))
        {
            Some("dash" | "lgDash" | "sysDash") => crate::deck::Dash::Dash,
            Some("dot" | "sysDot") => crate::deck::Dash::Dot,
            _ => crate::deck::Dash::Solid,
        };
    }
    if let Some(gradient) = props
        .kid("gradFill")
        .filter(|gradient| gradient.kid("lin").is_some())
        && let Some(paint) = gradient_fill(cx, gradient)
    {
        match &mut node.kind {
            Kind::Shape { fill, .. } => *fill = Some(paint),
            Kind::Frame { fill, children, .. } => {
                if fill.is_none()
                    && let Some(Kind::Shape {
                        fill: child_fill, ..
                    }) = children.first_mut().map(|child| &mut child.kind)
                {
                    *child_fill = Some(paint);
                } else {
                    *fill = Some(paint);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn gradient_fill(cx: &Cx, gradient: &El) -> Option<crate::deck::Fill> {
    let stops = gradient
        .kid("gsLst")?
        .kids_named("gs")
        .map(|stop| {
            Some((
                stop.num("pos")? / 100_000.0,
                crate::deck::Color::Solid(cx.env.paint(cx.theme.color(stop)?)),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    if !(2..=8).contains(&stops.len())
        || !stops.iter().all(|(offset, _)| (0.0..=1.0).contains(offset))
        || !stops.windows(2).all(|pair| pair[0].0 <= pair[1].0)
    {
        return None;
    }
    if let Some(linear) = gradient.kid("lin") {
        Some(crate::deck::Fill::Linear {
            linear: crate::deck::effects::Linear {
                angle: (linear.num("ang").unwrap_or(0.0) / 60_000.0 + 90.0).rem_euclid(360.0),
                stops,
            },
        })
    } else if gradient.kid("path").and_then(|path| path.attr("path")) == Some("circle") {
        Some(crate::deck::Fill::Radial {
            radial: crate::deck::effects::Radial { stops },
        })
    } else {
        None
    }
}

fn first_xfrm(chain: &[&El], frame: bool) -> Option<Xfrm> {
    chain.iter().find_map(|el| {
        if frame {
            el.kid("xfrm").and_then(xfrm)
        } else {
            sp_pr(el)?.kid("xfrm").and_then(xfrm)
        }
    })
}

/// What a shape's outline is drawn as.
#[derive(Clone, Copy)]
enum Geometry {
    /// A rectangle with a corner radius (a fraction of its shorter side),
    /// occupying `part` (fractions x0, y0, x1, y1) of the shape's box.
    Rect {
        radius: f64,
        part: [f64; 4],
    },
    Ellipse {
        part: [f64; 4],
    },
    Line,
}

const WHOLE: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

impl Geometry {
    fn rect(radius: f64) -> Self {
        Self::Rect {
            radius,
            part: WHOLE,
        }
    }

    fn part(self) -> [f64; 4] {
        match self {
            Self::Rect { part, .. } | Self::Ellipse { part } => part,
            Self::Line => WHOLE,
        }
    }
}

/// A freeform path: its bounding box inside the shape's box, and whether it
/// is a rounded rectangle or an ellipse.
fn custom_geometry(st: &mut State, props: &El, ext: (f64, f64)) -> Geometry {
    let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let (mut lines, mut curves, mut arcs) = (0, 0, 0);
    let mut arc_radius: (f64, f64) = (0.0, 0.0);
    let mut size: Option<(f64, f64)> = None;
    let mut straight: Vec<(f64, f64)> = Vec::new();
    for path in props
        .kid("pathLst")
        .map(|l| l.kids_named("path"))
        .into_iter()
        .flatten()
    {
        if size.is_none()
            && let (Some(w), Some(h)) = (path.num("w"), path.num("h"))
            && w > 0.0
            && h > 0.0
        {
            size = Some((w, h));
        }
        for command in &path.kids {
            let points: Vec<(f64, f64)> = command
                .kids_named("pt")
                .filter_map(|p| Some((p.num("x")?, p.num("y")?)))
                .collect();
            match command.name.as_str() {
                "moveTo" | "lnTo" => {
                    lines += 1;
                    straight.extend(&points);
                }
                "cubicBezTo" | "quadBezTo" => curves += 1,
                "arcTo" => {
                    arcs += 1;
                    arc_radius.0 = arc_radius.0.max(command.num("wR").unwrap_or(0.0));
                    arc_radius.1 = arc_radius.1.max(command.num("hR").unwrap_or(0.0));
                }
                _ => {}
            }
            for (x, y) in points {
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x);
                bounds[3] = bounds[3].max(y);
            }
        }
    }
    if bounds[0] > bounds[2] || bounds[1] > bounds[3] {
        st.warn
            .names("Freeform shapes drawn as rectangles", "freeform");
        return Geometry::rect(0.0);
    }
    let (pw, ph) = size.unwrap_or((ext.0.max(1.0), ext.1.max(1.0)));
    let part = [
        (bounds[0] / pw).clamp(0.0, 1.0),
        (bounds[1] / ph).clamp(0.0, 1.0),
        (bounds[2] / pw).clamp(0.0, 1.0),
        (bounds[3] / ph).clamp(0.0, 1.0),
    ];
    if curves >= 4 && lines <= 1 {
        return Geometry::Ellipse { part };
    }
    let aligned = straight
        .windows(2)
        .all(|p| (p[0].0 - p[1].0).abs() < 1.0 || (p[0].1 - p[1].1).abs() < 1.0);
    if curves > 0 || (arcs == 0 && !aligned) {
        st.warn
            .names("Freeform shapes drawn as rectangles", "freeform");
    }
    let radius = if arcs > 0 {
        let sub = ((part[2] - part[0]) * ext.0).min((part[3] - part[1]) * ext.1);
        let r = (arc_radius.0 / pw * ext.0).min(arc_radius.1 / ph * ext.1);
        if sub > 0.0 {
            (r / sub).clamp(0.0, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    Geometry::Rect { radius, part }
}

/// The preset geometry of a shape, with the corner radius fraction of a
/// rounded rectangle.
fn geometry(st: &mut State, chain: &[&El], ext: (f64, f64)) -> Geometry {
    let props = chain
        .iter()
        .filter_map(|el| sp_pr(el))
        .find(|p| p.kid("prstGeom").is_some() || p.kid("custGeom").is_some());
    let Some(props) = props else {
        return Geometry::rect(0.0);
    };
    let Some(preset) = props.kid("prstGeom") else {
        return props
            .kid("custGeom")
            .map_or_else(|| Geometry::rect(0.0), |g| custom_geometry(st, g, ext));
    };
    let name = preset.attr("prst").unwrap_or("rect");
    let adjust = preset
        .path(&["avLst"])
        .and_then(|l| l.kids_named("gd").find(|g| g.attr("name") == Some("adj")))
        .and_then(|g| g.attr("fmla"))
        .and_then(|f| f.strip_prefix("val "))
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| (v / 100_000.0).clamp(0.0, 0.5));
    match name {
        "rect" | "flowChartProcess" | "flowChartPredefinedProcess" | "plaque" => {
            Geometry::rect(0.0)
        }
        "roundRect"
        | "flowChartAlternateProcess"
        | "round1Rect"
        | "round2SameRect"
        | "round2DiagRect"
        | "snipRoundRect" => Geometry::rect(adjust.unwrap_or(0.166_67)),
        "ellipse" | "flowChartConnector" | "flowChartTerminator" => {
            Geometry::Ellipse { part: WHOLE }
        }
        "line" | "straightConnector1" | "lineInv" => Geometry::Line,
        n if n.starts_with("bentConnector") || n.starts_with("curvedConnector") => {
            st.warn
                .tally("Elbow and curved connectors are drawn as straight lines");
            Geometry::Line
        }
        other => {
            st.warn.names("Preset shapes drawn as rectangles", other);
            Geometry::rect(0.0)
        }
    }
}

/// Where `part` of `rect` ends up when `rect` is turned by `rot` degrees
/// about its own center: a node placed there can carry the rotation itself.
fn turned_part(rect: Rect, part: [f64; 4], rot: f64) -> Rect {
    let sub = Rect {
        x: rect.x + part[0] * rect.w,
        y: rect.y + part[1] * rect.h,
        w: ((part[2] - part[0]) * rect.w).max(0.0),
        h: ((part[3] - part[1]) * rect.h).max(0.0),
    };
    if rot.rem_euclid(360.0) < 0.01 || part == WHOLE {
        return sub;
    }
    let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let (dx, dy) = (sub.x + sub.w / 2.0 - cx, sub.y + sub.h / 2.0 - cy);
    let turn = rot.to_radians();
    let (nx, ny) = (
        cx + dx * turn.cos() - dy * turn.sin(),
        cy + dx * turn.sin() + dy * turn.cos(),
    );
    Rect {
        x: nx - sub.w / 2.0,
        y: ny - sub.h / 2.0,
        ..sub
    }
}

/// A fill that fades out from the middle, as stacked translucent ellipses
/// (outermost first) inside a `w` by `h` box.
fn glow(cx: &Cx, st: &mut State, stops: &[(f64, Rgba)], w: f64, h: f64) -> Vec<Node> {
    const RINGS: usize = 10;
    let reach = stops.last().map_or(1.0, |s| s.0).clamp(0.05, 1.0);
    let at = |u: f64| -> Rgba {
        let mut previous = stops[0];
        for stop in stops {
            if u <= stop.0 {
                let span = (stop.0 - previous.0).max(1e-6);
                let t = ((u - previous.0) / span).clamp(0.0, 1.0);
                let mix = |a: f64, b: f64| a + (b - a) * t;
                return Rgba {
                    r: mix(previous.1.r, stop.1.r),
                    g: mix(previous.1.g, stop.1.g),
                    b: mix(previous.1.b, stop.1.b),
                    a: mix(previous.1.a, stop.1.a),
                };
            }
            previous = *stop;
        }
        previous.1
    };
    let mut out = Vec::new();
    let mut composed = 0.0_f64;
    for ring in 0..RINGS {
        let outer = reach * (RINGS - ring) as f64 / RINGS as f64;
        let middle = reach * (RINGS as f64 - ring as f64 - 0.5) / RINGS as f64;
        let target = at(middle);
        let layer = 1.0 - (1.0 - target.a) / (1.0 - composed).max(0.01);
        if layer < 0.004 {
            continue;
        }
        composed = 1.0 - (1.0 - composed) * (1.0 - layer);
        let (rx, ry) = (
            (outer * w / 2.0 * std::f64::consts::SQRT_2).min(w / 2.0),
            (outer * h / 2.0 * std::f64::consts::SQRT_2).min(h / 2.0),
        );
        let color = Rgba {
            a: layer.clamp(0.0, 1.0),
            ..target
        };
        out.push(make(
            st,
            Kind::Shape {
                shape: Shape::Ellipse,
                fill: Some(cx.env.paint(color).into()),
                stroke: None,
                radius: 0.0,
            },
            None,
            Rect {
                x: w / 2.0 - rx,
                y: h / 2.0 - ry,
                w: rx * 2.0,
                h: ry * 2.0,
            },
            0.0,
        ));
    }
    out
}

/// The nodes that draw a shape's outline and fill, positioned inside its box.
fn background(cx: &Cx, st: &mut State, geo: Geometry, look: &Look, rect: Rect) -> Vec<Node> {
    if let Some(stops) = &look.glow {
        return glow(cx, st, stops, rect.w, rect.h);
    }
    let part = geo.part();
    let own = Rect {
        x: part[0] * rect.w,
        y: part[1] * rect.h,
        w: (part[2] - part[0]) * rect.w,
        h: (part[3] - part[1]) * rect.h,
    };
    let (shape, radius) = match geo {
        Geometry::Ellipse { .. } => (Shape::Ellipse, 0.0),
        Geometry::Rect { radius, .. } => (Shape::Rect, radius),
        Geometry::Line => (Shape::Line, 0.0),
    };
    vec![make(
        st,
        Kind::Shape {
            shape,
            fill: Some(
                look.fill
                    .map_or_else(|| "#00000000".to_string(), |c| cx.env.paint(c))
                    .into(),
            ),
            stroke: stroke_of(cx, look.stroke),
            radius: round2((radius * own.w.min(own.h)).max(0.0)),
        },
        None,
        own,
        0.0,
    )]
}

/// An `sp` (or `SmartArt` `dsp:sp`): a text box, a placeholder, a plain shape,
/// or a line.
fn shape(cx: &Cx, st: &mut State, sp: &El, space: &Space, out: &mut Vec<Node>) {
    if hidden(sp) {
        return;
    }
    let placeholder = ph(sp);
    if placeholder.is_some_and(is_footer) || (cx.decor && placeholder.is_some()) {
        return;
    }
    let chain_data = chain_for(cx, sp);
    let chain_els: Vec<&El> = std::iter::once(sp)
        .chain(chain_data.layout)
        .chain(chain_data.master)
        .collect();
    let Some(xf) = first_xfrm(&chain_els, false) else {
        return;
    };
    let rect = space.rect(&xf);
    let refs = style_refs(cx, sp);
    let geo = geometry(st, &chain_els, (xf.w, xf.h));
    let look = look(cx, st, &chain_els, &refs);
    if matches!(geo, Geometry::Line) {
        line(cx, st, sp, &xf, space, &look, out);
        return;
    }
    let role = match placeholder.and_then(|p| p.attr("type")) {
        Some("title" | "ctrTitle") => Role::Title,
        Some(_) => Role::Body,
        None if placeholder.is_some() => Role::Body,
        None => Role::Other,
    };
    let text_out = sp.kid("txBody").and_then(|body| {
        text::build(
            cx,
            st,
            body,
            &chain_data,
            role,
            refs.font.filter(|_| placeholder.is_none()),
        )
    });
    let visible = look.fill.is_some() || look.stroke.is_some() || look.glow.is_some();
    if text_out.is_none() && !visible {
        return;
    }
    let name = name_of(sp);
    let mut parts = if visible {
        background(cx, st, geo, &look, rect)
    } else {
        Vec::new()
    };
    let Some(t) = text_out else {
        if !st.reserve(parts.len().max(1) + usize::from(parts.len() > 1)) {
            return;
        }
        let node = if parts.len() == 1 {
            // One node: it carries the rotation itself.
            let mut node = parts.remove(0);
            let place = turned_part(rect, geo.part(), xf.rot);
            node.name = name;
            node.sizing = Sizing::fixed(clamp(place.w), clamp(place.h));
            node.position = Position::Absolute {
                x: clamp(place.x),
                y: clamp(place.y),
            };
            let rot = xf.rot.rem_euclid(360.0);
            node.rotation = (rot > 0.01 && rot < 359.99).then(|| round2(rot));
            node
        } else {
            wrapper(st, name, rect, xf.rot, parts)
        };
        record(cx, st, sp, &node.id);
        out.push(node);
        return;
    };
    if !visible {
        if !st.reserve(1) {
            return;
        }
        let node = text_node(cx, st, name, &t, rect, &xf);
        record(cx, st, sp, &node.id);
        out.push(node);
        return;
    }
    if !st.reserve(parts.len() + 2) {
        return;
    }
    let own = Rect {
        x: 0.0,
        y: 0.0,
        w: rect.w,
        h: rect.h,
    };
    let mut label = text_node(cx, st, None, &t, own, &Xfrm::default());
    label.rotation = None;
    parts.push(label);
    let node = wrapper(st, name, rect, xf.rot, parts);
    record(cx, st, sp, &node.id);
    out.push(node);
}

/// A frame the size of `rect` holding `children`.
fn wrapper(
    st: &mut State,
    name: Option<String>,
    rect: Rect,
    rot: f64,
    children: Vec<Node>,
) -> Node {
    let mut node = make(
        st,
        Kind::Frame {
            layout: AutoLayout::default(),
            fill: None,
            stroke: None,
            radius: 0.0,
            clip: false,
            children,
        },
        name,
        rect,
        rot,
    );
    node.sizing = Sizing::fixed(clamp(rect.w), clamp(rect.h));
    node
}

/// A text node in `rect` (the shape box), inset by the body's margins.
fn text_node(
    cx: &Cx,
    st: &mut State,
    name: Option<String>,
    t: &text::TextOut,
    rect: Rect,
    xf: &Xfrm,
) -> Node {
    let [left, top, right, bottom] = t.body.resolved_insets().map(|v| v * cx.env.scale);
    let mut inner = Rect {
        x: rect.x + left,
        y: rect.y + top,
        w: (rect.w - left - right).max(1.0),
        h: (rect.h - top - bottom).max(1.0),
    };
    if t.body.wrap_none == Some(true) {
        // Unwrapped text may run past its box; leave it room on the open side.
        let slack = inner.w * 0.25;
        match t.font.align.unwrap_or_default() {
            crate::deck::Align::Left => inner.w += slack,
            crate::deck::Align::Right => {
                inner.x -= slack;
                inner.w += slack;
            }
            crate::deck::Align::Center => {
                inner.x -= slack / 2.0;
                inner.w += slack;
            }
        }
    }
    make(
        st,
        Kind::Text {
            text: t.markdown.clone(),
            style: t.style.into(),
            font: t.font.clone(),
        },
        name,
        inner,
        xf.rot,
    )
}

/// A straight line or arrow between the corners of the shape's box.
fn line(
    cx: &Cx,
    st: &mut State,
    el: &El,
    xf: &Xfrm,
    space: &Space,
    look: &Look,
    out: &mut Vec<Node>,
) {
    let Some((color, width_emu)) = look.stroke.or_else(|| look.fill.map(|c| (c, 12_700.0))) else {
        return;
    };
    let rect = space.rect(xf);
    let (cx0, cy0) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    // Endpoints before rotation, with the flips applied.
    let (mut sx, mut sy, mut ex, mut ey) = (rect.x, rect.y, rect.x + rect.w, rect.y + rect.h);
    if xf.flip_h {
        std::mem::swap(&mut sx, &mut ex);
    }
    if xf.flip_v {
        std::mem::swap(&mut sy, &mut ey);
    }
    let turn = xf.rot.to_radians();
    let rotate = |x: f64, y: f64| {
        let (dx, dy) = (x - cx0, y - cy0);
        (
            cx0 + dx * turn.cos() - dy * turn.sin(),
            cy0 + dx * turn.sin() + dy * turn.cos(),
        )
    };
    let (sx, sy) = rotate(sx, sy);
    let (ex, ey) = rotate(ex, ey);
    let length = (ex - sx).hypot(ey - sy);
    let mut angle = (ey - sy).atan2(ex - sx).to_degrees();
    let width_px = round2((width_emu * cx.env.scale).clamp(0.5, 200.0));
    // The deck draws an arrow head at the far end of its line.
    let head_only = look.head && !look.tail;
    if head_only {
        angle += 180.0;
    }
    let shape = if look.tail || look.head {
        Shape::Arrow
    } else {
        Shape::Line
    };
    if !st.reserve(1) {
        return;
    }
    let (mx, my) = (f64::midpoint(sx, ex), f64::midpoint(sy, ey));
    let thickness = width_px.max(1.0);
    let boxed = Rect {
        x: mx - length / 2.0,
        y: my - thickness / 2.0,
        w: length,
        h: thickness,
    };
    let node = make(
        st,
        Kind::Shape {
            shape,
            fill: None,
            stroke: Some(Stroke {
                color: cx.env.paint(color),
                width: width_px,
                dash: crate::deck::Dash::Solid,
            }),
            radius: 0.0,
        },
        name_of(el),
        boxed,
        angle,
    );
    record(cx, st, el, &node.id);
    out.push(node);
}

/// `p:cxnSp`: a connector, drawn as a straight line.
fn connector(cx: &Cx, st: &mut State, el: &El, space: &Space, out: &mut Vec<Node>) {
    if hidden(el) {
        return;
    }
    let Some(xf) = sp_pr(el).and_then(|p| p.kid("xfrm")).and_then(xfrm) else {
        return;
    };
    let refs = style_refs(cx, el);
    let chain_els = [el];
    let preset = sp_pr(el)
        .and_then(|p| p.kid("prstGeom"))
        .and_then(|g| g.attr("prst"))
        .unwrap_or("line");
    if preset.starts_with("bentConnector") || preset.starts_with("curvedConnector") {
        st.warn
            .tally("Elbow and curved connectors are drawn as straight lines");
    }
    let looked = look(cx, st, &chain_els, &refs);
    line(cx, st, el, &xf, space, &looked, out);
}

// -- pictures --------------------------------------------------------------------

fn crop_of(pic: &El) -> Option<[f64; 4]> {
    let rect = pic.path(&["blipFill", "srcRect"])?;
    let side = |k: &str| (rect.num(k).unwrap_or(0.0) / 100_000.0).clamp(0.0, 0.95);
    let crop = [side("l"), side("t"), side("r"), side("b")];
    (crop.iter().any(|c| *c > 0.005) && crop[0] + crop[2] < 0.98 && crop[1] + crop[3] < 0.98)
        .then_some(crop)
}

/// Re-encode `bytes` with `crop` (fractions l, t, r, b) cut away.
fn cropped(bytes: &[u8], crop: [f64; 4]) -> Option<Vec<u8>> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = reader.format();
    let (w, h) = reader.into_dimensions().ok()?;
    if u64::from(w) * u64::from(h) > MAX_CROP_PIXELS {
        return None;
    }
    let image = image::load_from_memory(bytes).ok()?;
    let (x, y) = (
        (f64::from(w) * crop[0]).round(),
        (f64::from(h) * crop[1]).round(),
    );
    let (cw, ch) = (
        (f64::from(w) * (1.0 - crop[0] - crop[2])).round().max(1.0),
        (f64::from(h) * (1.0 - crop[1] - crop[3])).round().max(1.0),
    );
    let piece = image.crop_imm(x as u32, y as u32, cw as u32, ch as u32);
    let mut out = Cursor::new(Vec::new());
    if format == Some(image::ImageFormat::Jpeg) {
        let rgb = piece.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
            .encode_image(&rgb)
            .ok()?;
    } else {
        piece.write_to(&mut out, image::ImageFormat::Png).ok()?;
    }
    Some(out.into_inner())
}

/// Store the picture a relationship points at, optionally cropped. Returns
/// the deck `src`, or `None` after warning.
pub(super) fn picture_src(
    cx: &Cx,
    st: &mut State,
    id: &str,
    crop: Option<[f64; 4]>,
) -> Option<String> {
    let key = format!("{}|{id}|{crop:?}", cx.part);
    if let Some(known) = st.images.get(&key) {
        return known.clone();
    }
    let result = (|| {
        let rel = cx.rels.get(id)?;
        if rel.external {
            st.warn
                .tally("Linked pictures (not embedded in the file) are not imported");
            return None;
        }
        let bytes = match st.pkg.media(&rel.target) {
            Ok(bytes) => bytes,
            Err(error) => {
                st.warn.push(format!(
                    "slide {}: a picture could not be read ({error})",
                    cx.slide
                ));
                return None;
            }
        };
        let stem = rel
            .target
            .rsplit('/')
            .next()
            .and_then(|f| f.rsplit_once('.').map(|(s, _)| s))
            .unwrap_or("image")
            .to_string();
        let bytes = if let Some(crop) = crop {
            if let Some(done) = cropped(&bytes, crop) {
                st.warn
                    .tally("Cropped pictures are cropped into a copy of the image");
                done
            } else {
                st.warn
                    .tally("Pictures too large to crop keep their uncropped image");
                bytes
            }
        } else {
            bytes
        };
        match assets::store_image_bytes(&bytes, &rel.target, &stem, &st.assets) {
            Ok(src) => Some(src),
            Err(error) => {
                let ext = rel
                    .target
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(
                    ext.as_str(),
                    "emf" | "wmf" | "svg" | "gif" | "tif" | "tiff" | "bmp"
                ) {
                    st.warn.names(
                        "Pictures in unsupported formats replaced by a placeholder",
                        &ext,
                    );
                } else {
                    st.warn.push(format!(
                        "slide {}: a picture was replaced by a placeholder ({error})",
                        cx.slide
                    ));
                }
                None
            }
        }
    })();
    st.images.insert(key, result.clone());
    result
}

fn placeholder_src(st: &mut State) -> Option<String> {
    if let Some(known) = &st.placeholder {
        return Some(known.clone());
    }
    let mut image = image::RgbImage::from_pixel(160, 90, image::Rgb([228, 228, 224]));
    for x in 0..160 {
        for y in 0..90 {
            if x < 2 || y < 2 || x >= 158 || y >= 88 {
                image.put_pixel(x, y, image::Rgb([180, 180, 172]));
            }
        }
    }
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    let src =
        assets::store_image_bytes(&png.into_inner(), "placeholder", "placeholder", &st.assets)
            .ok()?;
    st.placeholder = Some(src.clone());
    Some(src)
}

fn image_node(st: &mut State, src: String, name: Option<String>, rect: Rect, rot: f64) -> Node {
    make(
        st,
        Kind::Image {
            src,
            fit: crate::deck::Fit::Cover,
        },
        name,
        rect,
        rot,
    )
}

fn picture_geometry(pic: &El, node: &mut Node, rect: Rect) {
    if let Some(geom) = sp_pr(pic).and_then(|props| props.kid("prstGeom")) {
        match geom.attr("prst") {
            Some("ellipse") => node.mask = Some(crate::deck::Mask::Circle),
            Some("roundRect") => {
                let fraction = geom
                    .path(&["avLst", "gd"])
                    .and_then(|gd| gd.attr("fmla"))
                    .and_then(|formula| formula.strip_prefix("val "))
                    .and_then(|number| number.parse::<f64>().ok())
                    .unwrap_or(16_667.0)
                    / 100_000.0;
                node.image_radius = Some(round2(fraction.clamp(0.0, 0.5) * rect.w.min(rect.h)));
            }
            _ => {}
        }
    }
}

fn picture(cx: &Cx, st: &mut State, pic: &El, space: &Space, out: &mut Vec<Node>) {
    if hidden(pic) {
        return;
    }
    let placeholder = ph(pic);
    if placeholder.is_some_and(is_footer) || (cx.decor && placeholder.is_some()) {
        return;
    }
    let chain_data = chain_for(cx, pic);
    let chain_els: Vec<&El> = std::iter::once(pic)
        .chain(chain_data.layout)
        .chain(chain_data.master)
        .collect();
    let Some(xf) = first_xfrm(&chain_els, false) else {
        return;
    };
    let Some(embed) = pic.path(&["blipFill", "blip"]).and_then(|b| b.rel("embed")) else {
        return;
    };
    if xf.flip_h || xf.flip_v {
        st.warn.tally("Flipped pictures are drawn unflipped");
    }
    if sp_pr(pic)
        .and_then(|p| p.kid("prstGeom"))
        .and_then(|g| g.attr("prst"))
        .is_some_and(|g| !matches!(g, "rect" | "roundRect" | "ellipse"))
    {
        st.warn
            .tally("Pictures cropped to a shape are drawn as rectangles");
    }
    if nv(pic)
        .and_then(|n| n.kid("nvPr"))
        .is_some_and(|p| p.kid("videoFile").is_some() || p.kid("audioFile").is_some())
    {
        st.warn
            .tally("Audio and video are not imported; the poster image stands in");
    }
    if !st.reserve(1) {
        return;
    }
    let rect = space.rect(&xf);
    let src = picture_src(cx, st, embed, crop_of(pic));
    let (src, missing) = match src {
        Some(src) => (src, false),
        None => match placeholder_src(st) {
            Some(src) => (src, true),
            None => return,
        },
    };
    let mut node = image_node(st, src, name_of(pic), rect, xf.rot);
    picture_geometry(pic, &mut node, rect);
    if let Some(inset) = pic.path(&["blipFill", "stretch", "fillRect"]) {
        let (l, r, t, b) = (
            inset.num("l").unwrap_or(0.0),
            inset.num("r").unwrap_or(0.0),
            inset.num("t").unwrap_or(0.0),
            inset.num("b").unwrap_or(0.0),
        );
        if l + r > 0.0 || t + b > 0.0 {
            if let Kind::Image { fit, .. } = &mut node.kind {
                *fit = crate::deck::Fit::Contain;
            }
            node.focus = Some(crate::deck::Focus {
                x: if l + r > 0.0 {
                    (l / (l + r)).clamp(0.0, 1.0)
                } else {
                    0.5
                },
                y: if t + b > 0.0 {
                    (t / (t + b)).clamp(0.0, 1.0)
                } else {
                    0.5
                },
            });
        }
    }
    if missing {
        node.name = Some("Picture (not imported)".into());
    }
    if let Some(amount) = pic
        .path(&["blipFill", "blip", "alphaModFix"])
        .and_then(|a| a.num("amt"))
    {
        node.opacity = round2((amount / 100_000.0).clamp(0.0, 1.0));
    }
    record(cx, st, pic, &node.id);
    out.push(node);
}

// -- groups ----------------------------------------------------------------------

fn group(cx: &Cx, st: &mut State, el: &El, space: &Space, depth: usize, out: &mut Vec<Node>) {
    if hidden(el) {
        return;
    }
    let Some(xf) = el.kid("grpSpPr").and_then(|p| p.kid("xfrm")).and_then(xfrm) else {
        return;
    };
    let rect = space.rect(&xf);
    let inner = Space {
        origin: xf.child_off,
        base: (0.0, 0.0),
        k: (
            rect.w / xf.child_ext.0.max(1.0),
            rect.h / xf.child_ext.1.max(1.0),
        ),
    };
    if xf.flip_h || xf.flip_v {
        st.warn.tally("Flipped groups are drawn unflipped");
    }
    if depth >= MAX_FRAME_DEPTH {
        // Too deep for a frame: the children join the enclosing one.
        st.warn
            .tally("Groups nested more than seven deep are flattened");
        let flat = Space {
            origin: xf.child_off,
            base: (rect.x, rect.y),
            k: inner.k,
        };
        tree(cx, st, el, &flat, depth, out);
        return;
    }
    if !st.reserve(1) {
        return;
    }
    let mut children = Vec::new();
    let mut fill = None;
    let mut stroke = None;
    let mut radius = 0.0;
    let mut remaining: &[El] = &el.kids;
    // A group the deck exported has its background as the first shape.
    if let Some(first) = el.kids.iter().find(|k| k.name == "sp")
        && name_of(first).as_deref() == Some("Frame background")
        && first.kid("txBody").is_none()
    {
        let refs = style_refs(cx, first);
        let looked = look(cx, st, &[first], &refs);
        fill = looked.fill.map(|c| cx.env.paint(c).into());
        stroke = stroke_of(cx, looked.stroke);
        if let Geometry::Rect { radius: f, .. } = geometry(st, &[first], (xf.w, xf.h)) {
            radius = round2(f * rect.w.min(rect.h));
        }
        let skip = el
            .kids
            .iter()
            .position(|k| std::ptr::eq(k, first))
            .unwrap_or(0);
        remaining = &el.kids[skip + 1..];
    }
    let holder = El {
        kids: remaining.to_vec(),
        ..El::default()
    };
    tree(cx, st, &holder, &inner, depth + 1, &mut children);
    let mut node = make(
        st,
        Kind::Frame {
            layout: AutoLayout::default(),
            fill,
            stroke,
            radius,
            clip: false,
            children,
        },
        name_of(el),
        rect,
        xf.rot,
    );
    node.sizing = Sizing::fixed(clamp(rect.w), clamp(rect.h));
    record(cx, st, el, &node.id);
    out.push(node);
}

// -- graphic frames --------------------------------------------------------------

fn graphic_frame(cx: &Cx, st: &mut State, el: &El, space: &Space, out: &mut Vec<Node>) {
    if hidden(el) || (cx.decor && ph(el).is_some()) {
        return;
    }
    let Some(xf) = el.kid("xfrm").and_then(xfrm) else {
        return;
    };
    let rect = space.rect(&xf);
    let Some(data) = el.path(&["graphic", "graphicData"]) else {
        return;
    };
    let uri = data.attr("uri").unwrap_or("");
    if uri.ends_with("/table") {
        if let Some(table) = data.kid("tbl") {
            self::table(cx, st, el, table, rect, &xf, out);
        }
    } else if uri.ends_with("/chart") {
        if !st.reserve(1) {
            return;
        }
        st.warn.push(format!(
            "slide {}: a chart was not imported; a placeholder stands in",
            cx.slide
        ));
        if let Some(src) = placeholder_src(st) {
            let mut node = image_node(st, src, Some("Chart (not imported)".into()), rect, xf.rot);
            node.name = Some("Chart (not imported)".into());
            record(cx, st, el, &node.id);
            out.push(node);
        }
    } else if uri.ends_with("/diagram") {
        smartart(cx, st, el, data, rect, &xf, out);
    } else if let Some(pic) = data.find("pic") {
        picture(cx, st, pic, &Space::root(cx.env.scale), out);
    } else {
        st.warn.push(format!(
            "slide {}: an embedded object was not imported",
            cx.slide
        ));
    }
}

fn cell_text(cell: &El) -> String {
    let Some(body) = cell.kid("txBody") else {
        return String::new();
    };
    body.kids_named("p")
        .map(|p| {
            p.kids
                .iter()
                .filter(|k| matches!(k.name.as_str(), "r" | "fld"))
                .map(|k| k.kid("t").map_or("", |t| t.text.as_str()))
                .collect::<String>()
        })
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn table(cx: &Cx, st: &mut State, el: &El, tbl: &El, rect: Rect, xf: &Xfrm, out: &mut Vec<Node>) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut bytes = 0;
    let mut sample: Option<RunSample> = None;
    for tr in tbl.kids_named("tr") {
        let mut row = Vec::new();
        for tc in tr.kids_named("tc") {
            let text = cell_text(tc);
            if sample.is_none() && !text.is_empty() {
                sample = RunSample::of(cx, tc);
            }
            bytes += text.len();
            row.push(text);
        }
        rows.push(row);
    }
    if rows.is_empty() {
        return;
    }
    if bytes > 7_000 {
        st.warn.push(format!(
            "slide {}: a table's text was cut short (8 KB limit)",
            cx.slide
        ));
        let mut kept = 0;
        rows.retain(|row| {
            kept += row.iter().map(String::len).sum::<usize>();
            kept <= 7_000
        });
    }
    st.warn.tally(
        "Tables keep their text only; cell fills, borders and merged cells are not imported",
    );
    if !st.reserve(1) {
        return;
    }
    let mut font = Font::default();
    if let Some(sample) = sample {
        font = sample.font(cx);
    }
    let node = make(
        st,
        Kind::Table {
            rows,
            style: "body".into(),
            font,
        },
        name_of(el),
        rect,
        xf.rot,
    );
    record(cx, st, el, &node.id);
    out.push(node);
}

/// The first run of a table cell, for the table's font.
struct RunSample {
    props: super::props::RunProps,
}

impl RunSample {
    fn of(cx: &Cx, cell: &El) -> Option<Self> {
        let run = cell.path(&["txBody"])?.kids_named("p").find_map(|p| {
            p.kids_named("r")
                .find(|r| r.kid("t").is_some_and(|t| !t.text.trim().is_empty()))
        })?;
        Some(Self {
            props: run
                .kid("rPr")
                .map(|r| run_props(r, cx.theme))
                .unwrap_or_default(),
        })
    }

    fn font(&self, cx: &Cx) -> Font {
        let size_pt = self.props.size.unwrap_or(18.0);
        let font = Font {
            family: self.props.family.clone().map(|f| cx.env.family_role(&f)),
            size: Some(round2(size_pt * 12_700.0 * cx.env.scale)),
            weight: Some(if self.props.bold.unwrap_or(false) {
                700
            } else {
                400
            }),
            italic: Some(self.props.italic.unwrap_or(false)),
            color: Some(
                cx.env.paint(
                    self.props
                        .color
                        .or_else(|| cx.theme.scheme("tx1"))
                        .unwrap_or_else(|| Rgba::rgb(0, 0, 0)),
                ),
            ),
            ..Font::default()
        };
        cx.env.delta("body", &font)
    }
}

// -- SmartArt --------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn smartart(
    cx: &Cx,
    st: &mut State,
    el: &El,
    data: &El,
    rect: Rect,
    xf: &Xfrm,
    out: &mut Vec<Node>,
) {
    // The drawing part is named by the data model's extension, through the
    // slide's own relationships.
    let model = data
        .find("relIds")
        .and_then(|r| r.rel("dm"))
        .and_then(|id| cx.rels.get(id))
        .map(|r| r.target.clone());
    let drawing = model
        .as_deref()
        .and_then(|part| st.pkg.xml(part).ok())
        .and_then(|root| {
            root.find("dataModelExt")
                .and_then(|e| e.attr("relId").map(str::to_string))
        });
    let drawing_root = drawing
        .and_then(|id| cx.rels.get(&id).map(|r| r.target.clone()))
        .and_then(|part| st.pkg.xml(&part).ok());
    if let Some(root) = drawing_root.as_ref().and_then(|r| r.kid("spTree")) {
        if !st.reserve(1) {
            return;
        }
        st.warn
            .tally("SmartArt is imported from its saved drawing as plain shapes");
        let inner = Space {
            origin: (0.0, 0.0),
            base: (0.0, 0.0),
            k: (cx.env.scale, cx.env.scale),
        };
        let mut children = Vec::new();
        let drawing_cx = Cx {
            record: false,
            layout_tree: None,
            master_tree: None,
            ..*cx
        };
        smartart_shapes(&drawing_cx, st, root, &inner, &mut children);
        let mut node = make(
            st,
            Kind::Frame {
                layout: AutoLayout::default(),
                fill: None,
                stroke: None,
                radius: 0.0,
                clip: false,
                children,
            },
            name_of(el),
            rect,
            xf.rot,
        );
        node.sizing = Sizing::fixed(clamp(rect.w), clamp(rect.h));
        out.push(node);
        return;
    }
    // No saved drawing: keep the diagram's text.
    let texts = model
        .as_deref()
        .and_then(|part| st.pkg.xml(part).ok())
        .map(|root| {
            let mut points = Vec::new();
            root.descendants("pt", &mut points);
            points
                .into_iter()
                .filter(|p| p.attr("type").is_none_or(|t| t == "node"))
                .map(|p| {
                    let mut runs = Vec::new();
                    p.descendants("t", &mut runs);
                    runs.iter()
                        .map(|t| t.text.as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .filter(|t| !t.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    st.warn.push(format!(
        "slide {}: SmartArt has no saved drawing; its text was kept as a list",
        cx.slide
    ));
    if texts.is_empty() || !st.reserve(1) {
        return;
    }
    let mut markdown = texts
        .iter()
        .map(|t| format!("- {}", super::text::plain_line(t)))
        .collect::<Vec<_>>()
        .join("\n");
    markdown.truncate(
        (0..=7_000.min(markdown.len()))
            .rev()
            .find(|i| markdown.is_char_boundary(*i))
            .unwrap_or(0),
    );
    let node = make(
        st,
        Kind::Text {
            text: markdown,
            style: "body".into(),
            font: Font::default(),
        },
        name_of(el),
        rect,
        xf.rot,
    );
    out.push(node);
}

/// The shapes of a `SmartArt` drawing: like `p:sp`, with the text in its own box.
fn smartart_shapes(cx: &Cx, st: &mut State, root: &El, space: &Space, out: &mut Vec<Node>) {
    for sp in root.kids_named("sp") {
        let Some(mut xf) = sp.kid("spPr").and_then(|p| p.kid("xfrm")).and_then(xfrm) else {
            continue;
        };
        let refs = style_refs(cx, sp);
        let chain_els = [sp];
        let geo = geometry(st, &chain_els, (xf.w, xf.h));
        let looked = look(cx, st, &chain_els, &refs);
        let text_xf = sp.kid("txXfrm").and_then(xfrm);
        let text_out = sp.kid("txBody").and_then(|body| {
            text::build(
                cx,
                st,
                body,
                &Chain {
                    layout: None,
                    master: None,
                },
                Role::Other,
                refs.font,
            )
        });
        let rect = space.rect(&xf);
        let name = name_of(sp);
        if looked.fill.is_some() || looked.stroke.is_some() {
            if !st.reserve(1) {
                return;
            }
            let (shape, radius) = match geo {
                Geometry::Ellipse { .. } => (Shape::Ellipse, 0.0),
                Geometry::Rect { radius: f, .. } => (Shape::Rect, round2(f * rect.w.min(rect.h))),
                Geometry::Line => (Shape::Rect, 0.0),
            };
            let node = make(
                st,
                Kind::Shape {
                    shape,
                    fill: Some(
                        looked
                            .fill
                            .map_or_else(|| "#00000000".to_string(), |c| cx.env.paint(c))
                            .into(),
                    ),
                    stroke: stroke_of(cx, looked.stroke),
                    radius,
                },
                name.clone(),
                rect,
                xf.rot,
            );
            out.push(node);
        }
        if let Some(t) = text_out {
            if !st.reserve(1) {
                return;
            }
            if let Some(tx) = text_xf {
                xf = tx;
            }
            let node = text_node(cx, st, name, &t, space.rect(&xf), &Xfrm::default());
            out.push(node);
        }
    }
}
