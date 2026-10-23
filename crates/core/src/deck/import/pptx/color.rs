//! Theme colors and fonts, and `DrawingML` color resolution.

// Colour maths reads best with the conventional r, g, b, h, s, l names.
#![allow(clippy::many_single_char_names)]

use std::collections::HashMap;
use std::fmt::Write as _;

use super::super::xml::El;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
            a: 1.0,
        }
    }

    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// `#rrggbb`, or `#rrggbbaa` when not opaque.
    pub fn hex(self) -> String {
        let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        let mut out = format!(
            "#{:02x}{:02x}{:02x}",
            byte(self.r),
            byte(self.g),
            byte(self.b)
        );
        if self.a < 0.995 {
            let _ = write!(out, "{:02x}", byte(self.a * 255.0));
        }
        out
    }

    fn hsl(self) -> (f64, f64, f64) {
        let (r, g, b) = (self.r / 255.0, self.g / 255.0, self.b / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = f64::midpoint(max, min);
        if (max - min).abs() < f64::EPSILON {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if (max - r).abs() < f64::EPSILON {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() < f64::EPSILON {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s, l)
    }

    fn from_hsl(h: f64, s: f64, l: f64, a: f64) -> Self {
        fn channel(p: f64, q: f64, mut t: f64) -> f64 {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        }
        if s <= 0.0 {
            let v = l * 255.0;
            return Self {
                r: v,
                g: v,
                b: v,
                a,
            };
        }
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        Self {
            r: channel(p, q, h + 1.0 / 3.0) * 255.0,
            g: channel(p, q, h) * 255.0,
            b: channel(p, q, h - 1.0 / 3.0) * 255.0,
            a,
        }
    }
}

/// The colors and fonts of one theme part, plus the master's color map.
#[derive(Default, Debug)]
pub(super) struct Theme {
    pub colors: HashMap<String, Rgba>,
    pub major: Option<String>,
    pub minor: Option<String>,
    /// `a:lnStyleLst` line widths in EMU, by 0-based style index.
    pub line_widths: Vec<f64>,
    /// `bg1`, `tx1`, … to a scheme color name (`lt1`, `dk1`, …).
    pub map: HashMap<String, String>,
}

fn preset(name: &str) -> Option<Rgba> {
    let (r, g, b) = match name {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "gray" | "grey" => (128, 128, 128),
        "dkGray" | "dkGrey" => (169, 169, 169),
        "ltGray" | "ltGrey" => (211, 211, 211),
        "orange" => (255, 165, 0),
        "purple" => (128, 0, 128),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "navy" => (0, 0, 128),
        "maroon" => (128, 0, 0),
        "silver" => (192, 192, 192),
        _ => return None,
    };
    Some(Rgba::rgb(r, g, b))
}

impl Theme {
    pub fn from_xml(theme: &El, master: Option<&El>) -> Self {
        let mut out = Self::default();
        if let Some(scheme) = theme.path(&["themeElements", "clrScheme"]) {
            for color in &scheme.kids {
                let found = color.kids.first().and_then(|c| match c.name.as_str() {
                    "srgbClr" => c.attr("val").and_then(Rgba::parse),
                    "sysClr" => c.attr("lastClr").and_then(Rgba::parse).or_else(|| {
                        Some(if c.attr("val") == Some("window") {
                            Rgba::rgb(255, 255, 255)
                        } else {
                            Rgba::rgb(0, 0, 0)
                        })
                    }),
                    _ => None,
                });
                if let Some(rgba) = found {
                    out.colors.insert(color.name.clone(), rgba);
                }
            }
        }
        if let Some(fonts) = theme.path(&["themeElements", "fontScheme"]) {
            let latin = |name: &str| {
                fonts
                    .kid(name)?
                    .kid("latin")?
                    .attr("typeface")
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
            };
            out.major = latin("majorFont");
            out.minor = latin("minorFont");
        }
        if let Some(styles) = theme.path(&["themeElements", "fmtScheme", "lnStyleLst"]) {
            out.line_widths = styles
                .kids_named("ln")
                .map(|l| l.num("w").unwrap_or(9525.0))
                .collect();
        }
        for (key, value) in [
            ("bg1", "lt1"),
            ("tx1", "dk1"),
            ("bg2", "lt2"),
            ("tx2", "dk2"),
        ] {
            out.map.insert(key.into(), value.into());
        }
        if let Some(map) = master.and_then(|m| m.kid("clrMap")) {
            for (key, value) in &map.attrs {
                out.map.insert(key.clone(), value.clone());
            }
        }
        out
    }

    /// A scheme color by name (`accent1`, `tx1`, `bg2`, `lt1`, …).
    pub fn scheme(&self, name: &str) -> Option<Rgba> {
        let name = self.map.get(name).map_or(name, String::as_str);
        self.colors.get(name).copied()
    }

    /// The color a `solidFill`-like element names (`srgbClr`, `schemeClr`, …),
    /// with its modifiers applied. `element` is the element holding the color.
    pub fn color(&self, element: &El) -> Option<Rgba> {
        let node = element.kids.iter().find(|k| {
            matches!(
                k.name.as_str(),
                "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr"
            )
        })?;
        let mut color = match node.name.as_str() {
            "srgbClr" => Rgba::parse(node.attr("val")?)?,
            "schemeClr" => self.scheme(node.attr("val")?)?,
            "sysClr" => node
                .attr("lastClr")
                .and_then(Rgba::parse)
                .unwrap_or_else(|| Rgba::rgb(0, 0, 0)),
            "prstClr" => preset(node.attr("val")?)?,
            _ => {
                let c = |k: &str| (node.num(k).unwrap_or(0.0) / 1000.0).clamp(0.0, 100.0) * 2.55;
                Rgba {
                    r: c("r"),
                    g: c("g"),
                    b: c("b"),
                    a: 1.0,
                }
            }
        };
        for modifier in &node.kids {
            let Some(v) = modifier.num("val").map(|v| v / 100_000.0) else {
                continue;
            };
            match modifier.name.as_str() {
                "alpha" => color.a = v.clamp(0.0, 1.0),
                "alphaMod" => color.a = (color.a * v).clamp(0.0, 1.0),
                "alphaOff" => color.a = (color.a + v).clamp(0.0, 1.0),
                "lumMod" | "lumOff" | "satMod" | "satOff" | "hueOff" => {
                    let (h, s, l) = color.hsl();
                    let (h, s, l) = match modifier.name.as_str() {
                        "lumMod" => (h, s, (l * v).clamp(0.0, 1.0)),
                        "lumOff" => (h, s, (l + v).clamp(0.0, 1.0)),
                        "satMod" => (h, (s * v).clamp(0.0, 1.0), l),
                        "satOff" => (h, (s + v).clamp(0.0, 1.0), l),
                        _ => ((h + v / 216.0).rem_euclid(1.0), s, l),
                    };
                    color = Rgba::from_hsl(h, s, l, color.a);
                }
                "tint" => {
                    let t = v.clamp(0.0, 1.0);
                    color.r = color.r * t + 255.0 * (1.0 - t);
                    color.g = color.g * t + 255.0 * (1.0 - t);
                    color.b = color.b * t + 255.0 * (1.0 - t);
                }
                "shade" => {
                    let t = v.clamp(0.0, 1.0);
                    color.r *= t;
                    color.g *= t;
                    color.b *= t;
                }
                _ => {}
            }
        }
        Some(color)
    }

    /// The family a typeface reference names: a literal family, or a theme
    /// font reference (`+mj-lt`, `+mn-lt`) looked up here.
    pub fn font_ref(&self, typeface: &str) -> Option<String> {
        match typeface {
            "+mj-lt" => self.major.clone(),
            "+mn-lt" => self.minor.clone(),
            t if t.starts_with('+') => None,
            t => Some(t.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::xml::parse;
    use super::*;

    fn theme() -> Theme {
        let xml = parse(
            "<theme><themeElements><clrScheme><dk1><sysClr val=\"windowText\" lastClr=\"000000\"/></dk1><lt1><sysClr val=\"window\" lastClr=\"FFFFFF\"/></lt1><dk2><srgbClr val=\"1F497D\"/></dk2><lt2><srgbClr val=\"EEECE1\"/></lt2><accent1><srgbClr val=\"4F81BD\"/></accent1></clrScheme><fontScheme><majorFont><latin typeface=\"Cambria\"/></majorFont><minorFont><latin typeface=\"Calibri\"/></minorFont></fontScheme></themeElements></theme>",
        )
        .unwrap();
        Theme::from_xml(&xml, None)
    }

    fn color(theme: &Theme, xml: &str) -> String {
        theme
            .color(&parse(&format!("<f>{xml}</f>")).unwrap())
            .unwrap()
            .hex()
    }

    #[test]
    fn scheme_and_srgb_colors_with_modifiers() {
        let t = theme();
        assert_eq!(color(&t, "<srgbClr val=\"FF0000\"/>"), "#ff0000");
        assert_eq!(color(&t, "<schemeClr val=\"tx1\"/>"), "#000000");
        assert_eq!(color(&t, "<schemeClr val=\"bg1\"/>"), "#ffffff");
        assert_eq!(color(&t, "<schemeClr val=\"accent1\"/>"), "#4f81bd");
        assert_eq!(
            color(
                &t,
                "<srgbClr val=\"000000\"><alpha val=\"50000\"/></srgbClr>"
            ),
            "#00000080"
        );
        assert_eq!(
            color(
                &t,
                "<schemeClr val=\"bg1\"><lumMod val=\"50000\"/></schemeClr>"
            ),
            "#808080"
        );
        assert_eq!(
            color(
                &t,
                "<schemeClr val=\"tx1\"><lumMod val=\"50000\"/><lumOff val=\"50000\"/></schemeClr>"
            ),
            "#808080"
        );
        assert_eq!(color(&t, "<prstClr val=\"red\"/>"), "#ff0000");
    }

    #[test]
    fn fonts_and_references() {
        let t = theme();
        assert_eq!(t.font_ref("+mj-lt").as_deref(), Some("Cambria"));
        assert_eq!(t.font_ref("+mn-lt").as_deref(), Some("Calibri"));
        assert_eq!(t.font_ref("+mn-ea"), None);
        assert_eq!(t.font_ref("Arial").as_deref(), Some("Arial"));
    }

    #[test]
    fn master_color_map_swaps_roles() {
        let xml = parse("<theme><themeElements><clrScheme><dk1><srgbClr val=\"000000\"/></dk1><lt1><srgbClr val=\"FFFFFF\"/></lt1></clrScheme></themeElements></theme>").unwrap();
        let master = parse("<sldMaster><clrMap bg1=\"dk1\" tx1=\"lt1\"/></sldMaster>").unwrap();
        let t = Theme::from_xml(&xml, Some(&master));
        assert_eq!(t.scheme("bg1").unwrap().hex(), "#000000");
        assert_eq!(t.scheme("tx1").unwrap().hex(), "#ffffff");
    }
}
