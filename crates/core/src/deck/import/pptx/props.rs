//! Text properties as `DrawingML` states them: run, paragraph and body
//! properties, and list styles (nine levels) that cascade.

use crate::deck::{Align, VAlign};

use super::super::xml::El;
use super::color::{Rgba, Theme};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Line {
    /// A multiple of the natural line height (1.0 is single spacing).
    Pct(f64),
    /// An exact line pitch in points.
    Pts(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bullet {
    None,
    Char,
    Numbered,
}

/// What a run (`a:rPr`, `a:defRPr`) says; every field is optional so that
/// layers can be merged.
#[derive(Clone, Debug, Default)]
pub(super) struct RunProps {
    /// Points.
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub family: Option<String>,
    pub color: Option<Rgba>,
    /// Hundredths of a point.
    pub spacing: Option<f64>,
    pub caps: Option<bool>,
    /// The relationship id of a hyperlink.
    pub link: Option<String>,
}

impl RunProps {
    /// Replace every field `over` sets.
    pub fn over(&mut self, over: &Self) {
        macro_rules! take {
            ($($f:ident),*) => { $(if over.$f.is_some() { self.$f.clone_from(&over.$f); })* };
        }
        take!(size, bold, italic, family, color, spacing, caps, link);
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ParaProps {
    pub align: Option<Align>,
    pub line: Option<Line>,
    pub bullet: Option<Bullet>,
    pub run: RunProps,
}

impl ParaProps {
    pub fn over(&mut self, over: &Self) {
        if over.align.is_some() {
            self.align = over.align;
        }
        if over.line.is_some() {
            self.line = over.line;
        }
        if over.bullet.is_some() {
            self.bullet = over.bullet;
        }
        self.run.over(&over.run);
    }
}

/// Nine paragraph levels.
#[derive(Clone, Debug, Default)]
pub(super) struct ListStyle(pub [ParaProps; 9]);

impl ListStyle {
    pub fn level(&self, level: usize) -> &ParaProps {
        &self.0[level.min(8)]
    }
}

/// Parse `a:rPr`, `a:defRPr` or `a:endParaRPr`.
pub(super) fn run_props(el: &El, theme: &Theme) -> RunProps {
    let mut out = RunProps {
        size: el.num("sz").map(|v| v / 100.0).filter(|v| *v > 0.0),
        bold: el.flag("b"),
        italic: el.flag("i"),
        spacing: el.num("spc"),
        caps: el.attr("cap").map(|c| c == "all"),
        ..RunProps::default()
    };
    if let Some(fill) = el.kid("solidFill") {
        out.color = theme.color(fill);
    }
    if let Some(typeface) = el.kid("latin").and_then(|l| l.attr("typeface")) {
        out.family = theme.font_ref(typeface);
    }
    out.link = el
        .kid("hlinkClick")
        .and_then(|h| h.rel("id"))
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    out
}

/// Parse `a:pPr` or one `a:lvlNpPr`.
pub(super) fn para_props(el: &El, theme: &Theme) -> ParaProps {
    let align = el.attr("algn").map(|a| match a {
        "ctr" => Align::Center,
        "r" => Align::Right,
        _ => Align::Left,
    });
    let line = el.kid("lnSpc").and_then(|spacing| {
        if let Some(v) = spacing.kid("spcPct").and_then(|p| p.num("val")) {
            Some(Line::Pct(v / 100_000.0))
        } else {
            spacing
                .kid("spcPts")
                .and_then(|p| p.num("val"))
                .map(|v| Line::Pts(v / 100.0))
        }
    });
    let bullet = el.kids.iter().find_map(|kid| match kid.name.as_str() {
        "buNone" => Some(Bullet::None),
        "buChar" | "buBlip" => Some(Bullet::Char),
        "buAutoNum" => Some(Bullet::Numbered),
        _ => None,
    });
    ParaProps {
        align,
        line,
        bullet,
        run: el
            .kid("defRPr")
            .map(|d| run_props(d, theme))
            .unwrap_or_default(),
    }
}

/// Parse an `a:lstStyle`, `p:titleStyle`, `p:bodyStyle`, `p:otherStyle` or the
/// presentation's `p:defaultTextStyle`.
pub(super) fn list_style(el: &El, theme: &Theme) -> ListStyle {
    let mut out = ListStyle::default();
    for (i, level) in out.0.iter_mut().enumerate() {
        if let Some(props) = el.kid(&format!("lvl{}pPr", i + 1)) {
            *level = para_props(props, theme);
        }
    }
    out
}

/// What `a:bodyPr` says about the text box.
#[derive(Clone, Debug, Default)]
pub(super) struct BodyProps {
    /// Left, top, right and bottom insets in EMU.
    pub insets: [Option<f64>; 4],
    pub anchor: Option<VAlign>,
    pub wrap_none: Option<bool>,
    pub vertical: Option<bool>,
    /// From `a:normAutofit`: the font and line spacing reductions.
    pub font_scale: Option<f64>,
    pub line_reduction: Option<f64>,
}

pub(super) fn body_props(el: &El) -> BodyProps {
    let autofit = el.kid("normAutofit");
    BodyProps {
        insets: [
            el.num("lIns"),
            el.num("tIns"),
            el.num("rIns"),
            el.num("bIns"),
        ],
        anchor: el.attr("anchor").map(|a| match a {
            "ctr" => VAlign::Middle,
            "b" => VAlign::Bottom,
            _ => VAlign::Top,
        }),
        wrap_none: el.attr("wrap").map(|w| w == "none"),
        vertical: el.attr("vert").map(|v| v != "horz"),
        font_scale: autofit
            .and_then(|a| a.num("fontScale"))
            .map(|v| v / 100_000.0),
        line_reduction: autofit
            .and_then(|a| a.num("lnSpcReduction"))
            .map(|v| v / 100_000.0),
    }
}

impl BodyProps {
    /// Keep what `self` sets; take the rest from `other`.
    pub fn or(mut self, other: &Self) -> Self {
        for (mine, theirs) in self.insets.iter_mut().zip(other.insets) {
            if mine.is_none() {
                *mine = theirs;
            }
        }
        self.anchor = self.anchor.or(other.anchor);
        self.wrap_none = self.wrap_none.or(other.wrap_none);
        self.vertical = self.vertical.or(other.vertical);
        self.font_scale = self.font_scale.or(other.font_scale);
        self.line_reduction = self.line_reduction.or(other.line_reduction);
        self
    }

    /// The four insets with the `PowerPoint` defaults filled in.
    pub fn resolved_insets(&self) -> [f64; 4] {
        let [l, t, r, b] = self.insets;
        [
            l.unwrap_or(91_440.0),
            t.unwrap_or(45_720.0),
            r.unwrap_or(91_440.0),
            b.unwrap_or(45_720.0),
        ]
    }
}
