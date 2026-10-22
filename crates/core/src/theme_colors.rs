//! Terminal-theme discovery shared by the TUI and studio, without UI types.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::Deserialize;

/// Semantic roles as plain hex colors, safe to send to a canvas frontend.
pub type ThemeColors = BTreeMap<String, String>;

/// A terminal palette. Unspecified ANSI roles retain their terminal identity
/// in the TUI; `colors` gives standalone frontends their xterm equivalents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub colors: ThemeColors,
    rgb: BTreeMap<String, [u8; 3]>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            colors: [
                ("bg", 0),
                ("fg", 15),
                ("fg_dim", 8),
                ("accent", 6),
                ("accent2", 5),
                ("border", 6),
                ("border_dim", 8),
                ("success", 2),
                ("warning", 3),
                ("error", 1),
                ("diff_add_bg", 22),
                ("diff_del_bg", 52),
                ("user_msg", 6),
                ("assistant_msg", 15),
                ("tool_msg", 3),
                ("research_msg", 5),
                ("raised", 236),
                ("control", 238),
                ("selection", 239),
            ]
            .map(|(role, index)| (role.into(), indexed_hex(index)))
            .into(),
            rgb: BTreeMap::new(),
        }
    }
}

/// A 256-color terminal index as an opaque CSS hex color.
#[must_use]
pub fn indexed_hex(index: u8) -> String {
    let rgb = match index {
        0..=15 => [
            [0, 0, 0],
            [205, 0, 0],
            [0, 205, 0],
            [205, 205, 0],
            [0, 0, 238],
            [205, 0, 205],
            [0, 205, 205],
            [229, 229, 229],
            [127, 127, 127],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [92, 92, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ][usize::from(index)],
        16..=231 => {
            let n = index - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            [level(n / 36), level(n / 6 % 6), level(n % 6)]
        }
        _ => [8 + (index - 232) * 10; 3],
    };
    hex(rgb)
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

/// The subset of Alacritty's configuration used by nexus.
#[derive(Deserialize)]
pub struct Alacritty {
    colors: ColorsSection,
}

#[derive(Deserialize)]
struct ColorsSection {
    primary: Primary,
    normal: Colors,
    bright: Colors,
}

#[derive(Deserialize)]
struct Primary {
    background: String,
    foreground: String,
}

#[derive(Deserialize)]
struct Colors {
    black: String,
    red: String,
    green: String,
    yellow: String,
    magenta: String,
    cyan: String,
}

impl Palette {
    /// Explicit RGB value of a role, or `None` for a terminal ANSI fallback.
    #[must_use]
    pub fn rgb(&self, role: &str) -> Option<[u8; 3]> {
        self.rgb.get(role).copied()
    }

    fn set(&mut self, role: &str, rgb: [u8; 3]) {
        self.colors.insert(role.into(), hex(rgb));
        self.rgb.insert(role.into(), rgb);
    }

    /// Interpret an Alacritty/omarchy palette with nexus's existing roles.
    #[must_use]
    pub fn from_alacritty(a: &Alacritty) -> Self {
        let mut palette = Self::default();
        let n = &a.colors.normal;
        for (role, value) in [
            ("bg", &a.colors.primary.background),
            ("fg", &a.colors.primary.foreground),
            ("fg_dim", &a.colors.bright.black),
            ("accent", &n.cyan),
            ("accent2", &n.magenta),
            ("border", &n.cyan),
            ("border_dim", &a.colors.bright.black),
            ("success", &n.green),
            ("warning", &n.yellow),
            ("error", &n.red),
            ("user_msg", &n.cyan),
            ("assistant_msg", &a.colors.primary.foreground),
            ("tool_msg", &n.yellow),
            ("research_msg", &n.magenta),
        ] {
            if let Some(rgb) = parse_hex(value) {
                palette.set(role, rgb);
            }
        }
        palette.derive(None);
        palette
    }

    /// Interpret Ghostty's theme with inline config overrides already applied.
    #[must_use]
    pub fn from_ghostty(colors: &HashMap<String, String>) -> Option<Self> {
        let get = |key: &str| colors.get(key).and_then(|v| parse_hex(v));
        let mut palette = Self::default();
        palette.set("bg", get("background")?);
        palette.set("fg", get("foreground")?);
        palette.set("assistant_msg", get("foreground")?);
        for (role, index) in [
            ("fg_dim", 8),
            ("accent", 6),
            ("accent2", 5),
            ("border", 6),
            ("border_dim", 8),
            ("success", 2),
            ("warning", 3),
            ("error", 1),
            ("user_msg", 6),
            ("tool_msg", 3),
            ("research_msg", 5),
        ] {
            if let Some(rgb) = get(&format!("palette.{index}")) {
                palette.set(role, rgb);
            }
        }
        palette.derive(get("selection-background"));
        Some(palette)
    }

    fn derive(&mut self, selection: Option<[u8; 3]>) {
        if let (Some(bg), Some(fg)) = (self.rgb("bg"), self.rgb("fg")) {
            self.set("control", blend(bg, fg, 0.16));
            self.set("raised", blend(bg, fg, 0.07));
            self.set("border_dim", blend(bg, fg, 0.24));
            // As in the TUI, an ANSI accent leaves the selection at the
            // background if there is no explicit selection color.
            let accent = self.rgb("accent").map_or(bg, |a| blend(bg, a, 0.35));
            self.set("selection", selection.unwrap_or(accent));
        }
        if let (Some(bg), Some(success)) = (self.rgb("bg"), self.rgb("success")) {
            self.set("diff_add_bg", blend(bg, success, 0.18));
        }
        if let (Some(bg), Some(error)) = (self.rgb("bg"), self.rgb("error")) {
            self.set("diff_del_bg", blend(bg, error, 0.18));
        }
    }
}

fn blend(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let value = (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8;
        value
    })
}

/// Ghostty `key = value` lines; repeated palette entries keep their indices.
#[must_use]
pub fn ghostty_pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            match (k, v.split_once('=')) {
                ("palette", Some((n, hex))) => (format!("palette.{}", n.trim()), hex.trim().into()),
                _ => (k.to_string(), v.to_string()),
            }
        })
        .collect()
}

fn ghostty_config_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::home_dir() else {
        return Vec::new();
    };
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let mac = home.join("Library/Application Support/com.mitchellh.ghostty");
    [
        xdg.join("ghostty/config"),
        xdg.join("ghostty/config.ghostty"),
        mac.join("config"),
        mac.join("config.ghostty"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .collect()
}

fn ghostty_theme_file(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if std::path::Path::new(name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let home = std::env::home_dir()?;
    [
        home.join(".config/ghostty/themes"),
        PathBuf::from("/Applications/Ghostty.app/Contents/Resources/ghostty/themes"),
        PathBuf::from("/usr/share/ghostty/themes"),
        PathBuf::from("/usr/local/share/ghostty/themes"),
    ]
    .into_iter()
    .map(|dir| dir.join(name))
    .find(|p| p.is_file())
}

fn ghostty_theme() -> Option<Palette> {
    let mut config = Vec::new();
    for path in ghostty_config_paths() {
        config.extend(ghostty_pairs(&std::fs::read_to_string(path).ok()?));
    }
    let name = config
        .iter()
        .rev()
        .find(|(k, _)| k == "theme")
        .map(|(_, v)| {
            v.split(',')
                .find_map(|part| part.trim().strip_prefix("dark:"))
                .unwrap_or_else(|| v.split(',').next().unwrap_or(v))
                .trim()
                .to_string()
        });
    let mut colors: HashMap<String, String> = name
        .and_then(|n| ghostty_theme_file(&n))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| ghostty_pairs(&t).into_iter().collect())
        .unwrap_or_default();
    colors.extend(config);
    Palette::from_ghostty(&colors)
}

fn omarchy_current_dir() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".config/omarchy/current"))
}

/// The active omarchy symlink target, if present.
#[must_use]
pub fn current_link_target() -> Option<PathBuf> {
    std::fs::read_link(omarchy_current_dir()?).ok()
}

/// Changes when the active omarchy theme or Ghostty configuration changes.
#[must_use]
pub fn source_stamp() -> String {
    let ghostty: Vec<_> = ghostty_config_paths()
        .iter()
        .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect();
    format!("{:?}|{ghostty:?}", current_link_target())
}

/// The active omarchy, Ghostty, or built-in terminal palette, in that order.
#[must_use]
pub fn load() -> Palette {
    omarchy_current_dir()
        .map(|d| d.join("theme/alacritty.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<Alacritty>(&s).ok())
        .map(|a| Palette::from_alacritty(&a))
        .or_else(ghostty_theme)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_colors_preserve_ansi_and_derive_rgb_roles() {
        let fallback = Palette::default();
        assert_eq!(fallback.colors["control"], "#444444");
        assert_eq!(fallback.rgb("control"), None);
        let colors = ghostty_pairs("background = #ffffff\nforeground = #000000")
            .into_iter()
            .collect();
        let palette = Palette::from_ghostty(&colors).unwrap();
        assert_eq!(palette.colors["control"], "#d6d6d6");
        assert_eq!(palette.rgb("control"), Some([214; 3]));
        assert_eq!(palette.colors["selection"], "#ffffff");
    }

    #[test]
    fn studio_fixture_tokens_are_the_terminal_palettes() {
        let light: Alacritty = toml::from_str(include_str!(
            "../../studio/ui-dev/fixtures/theme-light.toml"
        ))
        .unwrap();
        for (palette, source) in [
            (
                Palette::default(),
                include_str!("../../studio/ui-dev/fixtures/theme-default.json"),
            ),
            (
                Palette::from_alacritty(&light),
                include_str!("../../studio/ui-dev/fixtures/theme-light.json"),
            ),
        ] {
            let expected: ThemeColors = serde_json::from_str(source).unwrap();
            for (role, color) in expected {
                assert_eq!(palette.colors[&role], color, "{role}");
            }
        }
    }

    #[test]
    fn theme_link_round_trips_only_plain_colors() {
        use crate::deck::link::{Line, Message};

        let message = Message::Theme {
            colors: Palette::default().colors,
        };
        let line = Line::encode(message.clone()).unwrap();
        assert_eq!(Line::decode(&line).unwrap(), message);
        assert!(line.contains("\"type\":\"Theme\""));
        assert!(!line.contains("rgb"));
    }

    #[test]
    fn unicode_and_malformed_hex_are_not_colors() {
        for bad in ["#123", "#éabcd", "#nope!!", "#ff00000"] {
            assert_eq!(parse_hex(bad), None);
        }
        assert_eq!(parse_hex("#AbCdEf"), Some([171, 205, 239]));
    }
}
