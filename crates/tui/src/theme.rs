//! Semantic color roles, sourced from the user's active omarchy theme
//! (`~/.config/omarchy/current/theme/alacritty.toml`) when present, falling
//! back to the app's original hardcoded ANSI palette otherwise. `events.rs`
//! polls `current_link_target()` and calls `load()` again when the omarchy
//! theme symlink changes, so switching themes in omarchy updates the running
//! app without a restart.

use std::path::PathBuf;

use ratatui::style::{Color, Style};
use serde::Deserialize;

/// How the TUI paints its general background.
///
/// Terminal cells do not support alpha blending. `Transparent` uses
/// [`Color::Reset`] so the terminal's own window opacity remains visible;
/// `Opaque` paints the active theme's background color into the cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundMode {
    #[default]
    Opaque,
    Transparent,
}

impl BackgroundMode {
    /// Database key used for this per-device UI preference.
    pub const SETTING_KEY: &str = "ui_background";

    /// Stable value persisted in the settings database.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
        }
    }

    /// Human-readable value used in the status line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.key()
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Opaque => Self::Transparent,
            Self::Transparent => Self::Opaque,
        }
    }

    /// Parse a `/theme` background argument.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("opaque")
            || value.eq_ignore_ascii_case("solid")
            || value.eq_ignore_ascii_case("on")
        {
            Some(Self::Opaque)
        } else if value.eq_ignore_ascii_case("transparent")
            || value.eq_ignore_ascii_case("terminal")
            || value.eq_ignore_ascii_case("reset")
            || value.eq_ignore_ascii_case("off")
        {
            Some(Self::Transparent)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    /// Background painted into the TUI surface; derived from `bg` and the
    /// user's [`BackgroundMode`].
    pub surface: Color,
    pub fg: Color,
    pub fg_dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub border: Color,
    pub border_dim: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub user_msg: Color,
    pub assistant_msg: Color,
    pub tool_msg: Color,
    pub research_msg: Color,
    /// One step up from the surface: selected rows, bubbles, menus. Follows
    /// the background mode — `Reset` when transparent, so nothing paints an
    /// opaque block over a see-through terminal.
    pub raised: Color,
    /// The opaque-mode `raised` shade, kept so switching modes can restore it.
    raised_opaque: Color,
    /// Text-selection highlight background.
    pub selection: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Black,
            surface: Color::Black,
            fg: Color::White,
            fg_dim: Color::DarkGray,
            accent: Color::Cyan,
            accent2: Color::Magenta,
            border: Color::Cyan,
            border_dim: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            user_msg: Color::Cyan,
            assistant_msg: Color::White,
            tool_msg: Color::Yellow,
            research_msg: Color::Magenta,
            // 256-color grays: the named ANSI palette has no in-between shade.
            raised: Color::Indexed(236),
            raised_opaque: Color::Indexed(236),
            selection: Color::Indexed(239),
        }
    }
}

#[derive(Deserialize)]
struct Alacritty {
    colors: ColorsSection,
}

#[derive(Deserialize)]
struct ColorsSection {
    primary: Primary,
    normal: Palette,
    bright: Palette,
}

#[derive(Deserialize)]
struct Primary {
    background: String,
    foreground: String,
}

#[derive(Deserialize)]
struct Palette {
    black: String,
    red: String,
    green: String,
    yellow: String,
    magenta: String,
    cyan: String,
}

fn hex_to_color(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

impl Theme {
    fn from_alacritty(a: &Alacritty) -> Self {
        let fallback = Self::default();
        let n = &a.colors.normal;
        let b = &a.colors.bright;
        let bg = hex_to_color(&a.colors.primary.background).unwrap_or(fallback.bg);
        Self {
            bg,
            surface: bg,
            fg: hex_to_color(&a.colors.primary.foreground).unwrap_or(fallback.fg),
            fg_dim: hex_to_color(&b.black).unwrap_or(fallback.fg_dim),
            accent: hex_to_color(&n.cyan).unwrap_or(fallback.accent),
            accent2: hex_to_color(&n.magenta).unwrap_or(fallback.accent2),
            border: hex_to_color(&n.cyan).unwrap_or(fallback.border),
            border_dim: hex_to_color(&b.black).unwrap_or(fallback.border_dim),
            success: hex_to_color(&n.green).unwrap_or(fallback.success),
            warning: hex_to_color(&n.yellow).unwrap_or(fallback.warning),
            error: hex_to_color(&n.red).unwrap_or(fallback.error),
            user_msg: hex_to_color(&n.cyan).unwrap_or(fallback.user_msg),
            assistant_msg: hex_to_color(&a.colors.primary.foreground)
                .unwrap_or(fallback.assistant_msg),
            tool_msg: hex_to_color(&n.yellow).unwrap_or(fallback.tool_msg),
            research_msg: hex_to_color(&n.magenta).unwrap_or(fallback.research_msg),
            raised: fallback.raised,
            raised_opaque: fallback.raised,
            selection: fallback.selection,
        }
        .with_derived_shades(None)
    }

    /// Fill the in-between shades from the real background/foreground when
    /// both are RGB: `raised` a small step toward the foreground, borders a
    /// quiet quarter step, and the selection from the theme when it names
    /// one. ANSI palettes keep their 256-color fallbacks.
    #[must_use]
    fn with_derived_shades(mut self, selection: Option<Color>) -> Self {
        if let (Color::Rgb(..), Color::Rgb(..)) = (self.bg, self.fg) {
            self.raised = blend(self.bg, self.fg, 0.07);
            self.raised_opaque = self.raised;
            self.border_dim = blend(self.bg, self.fg, 0.24);
            self.selection = selection.unwrap_or_else(|| blend(self.bg, self.accent, 0.35));
        }
        self
    }

    /// Apply a UI background mode without changing the palette colors.
    pub fn set_background_mode(&mut self, mode: BackgroundMode) {
        (self.surface, self.raised) = match mode {
            BackgroundMode::Opaque => (self.bg, self.raised_opaque),
            BackgroundMode::Transparent => (Color::Reset, Color::Reset),
        };
    }

    #[must_use]
    pub fn background_style(&self) -> Style {
        Style::default().bg(self.surface)
    }
}

/// `t` of the way from `a` to `b` (RGB only; anything else returns `a`).
fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let mix =
                |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => a,
    }
}

// ── Ghostty ───────────────────────────────────────────────────────────────

/// Ghostty config files, lowest precedence first (later keys win).
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

/// `key = value` lines; `palette = N=#hex` becomes key `palette.N`.
fn ghostty_pairs(text: &str) -> Vec<(String, String)> {
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

/// Where Ghostty keeps a theme named `name` (user dir, then bundled ones).
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

/// The theme Ghostty is showing: its `theme` file with the config's inline
/// colors layered on top. `light:X,dark:Y` picks the dark variant.
fn ghostty_theme() -> Option<Theme> {
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
    let mut colors: std::collections::HashMap<String, String> = name
        .and_then(|n| ghostty_theme_file(&n))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| ghostty_pairs(&t).into_iter().collect())
        .unwrap_or_default();
    colors.extend(config);
    Theme::from_ghostty(&colors)
}

impl Theme {
    fn from_ghostty(c: &std::collections::HashMap<String, String>) -> Option<Self> {
        let get = |k: &str| c.get(k).and_then(|v| hex_to_color(v));
        let bg = get("background")?;
        let fg = get("foreground")?;
        let fallback = Self::default();
        let pal = |n: u8, or: Color| get(&format!("palette.{n}")).unwrap_or(or);
        let cyan = pal(6, fallback.accent);
        let magenta = pal(5, fallback.accent2);
        let yellow = pal(3, fallback.warning);
        Some(
            Self {
                bg,
                surface: bg,
                fg,
                fg_dim: pal(8, fallback.fg_dim),
                accent: cyan,
                accent2: magenta,
                border: cyan,
                border_dim: pal(8, fallback.border_dim),
                success: pal(2, fallback.success),
                warning: yellow,
                error: pal(1, fallback.error),
                user_msg: cyan,
                assistant_msg: fg,
                tool_msg: yellow,
                research_msg: magenta,
                raised: fallback.raised,
                raised_opaque: fallback.raised,
                selection: fallback.selection,
            }
            .with_derived_shades(get("selection-background")),
        )
    }
}

/// Modification time of a file, as a comparable token.
fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn omarchy_current_dir() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".config/omarchy/current"))
}

/// The omarchy theme symlink's current target (e.g. `.../themes/retropc`), or
/// `None` off-omarchy. Together with the Ghostty config's mtime it forms the
/// stamp the event loop polls to pick up a theme switch live.
pub fn current_link_target() -> Option<PathBuf> {
    std::fs::read_link(omarchy_current_dir()?).ok()
}

/// A token that changes whenever the active theme source does.
pub fn source_stamp() -> String {
    let ghostty: Vec<_> = ghostty_config_paths().iter().map(|p| mtime(p)).collect();
    format!("{:?}|{ghostty:?}", current_link_target())
}

/// The active theme: omarchy's, else Ghostty's, else the built-in ANSI
/// palette (which the terminal maps onto its own colors).
pub fn load() -> Theme {
    omarchy_current_dir()
        .map(|d| d.join("theme/alacritty.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<Alacritty>(&s).ok())
        .map(|a| Theme::from_alacritty(&a))
        .or_else(ghostty_theme)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omarchy_alacritty_toml() {
        let toml = r##"
[colors.primary]
background = "#0B0C16"
foreground = "#ddf7ff"

[colors.normal]
black = "#0B0C16"
red = "#50f872"
green = "#4fe88f"
yellow = "#50f7d4"
blue = "#829dd4"
magenta = "#86a7df"
cyan = "#7cf8f7"
white = "#85E1FB"

[colors.bright]
black = "#6a6e95"
red = "#85ff9d"
green = "#9cf7c2"
yellow = "#a4ffec"
blue = "#c4d2ed"
magenta = "#cddbf4"
cyan = "#d1fffe"
white = "#ddf7ff"
"##;
        let a: Alacritty = toml::from_str(toml).unwrap();
        let theme = Theme::from_alacritty(&a);
        assert_eq!(theme.accent, Color::Rgb(0x7c, 0xf8, 0xf7));
        assert_eq!(theme.fg, Color::Rgb(0xdd, 0xf7, 0xff));
    }

    #[test]
    fn background_mode_parses_and_cycles() {
        assert_eq!(BackgroundMode::parse("solid"), Some(BackgroundMode::Opaque));
        assert_eq!(
            BackgroundMode::parse("terminal"),
            Some(BackgroundMode::Transparent)
        );
        assert_eq!(BackgroundMode::Opaque.next(), BackgroundMode::Transparent);
        assert_eq!(BackgroundMode::Transparent.next(), BackgroundMode::Opaque);
    }

    #[test]
    fn background_mode_updates_surface_color() {
        let mut theme = Theme::default();
        let raised = theme.raised;
        theme.set_background_mode(BackgroundMode::Transparent);
        assert_eq!(theme.surface, Color::Reset);
        assert_eq!(
            theme.raised,
            Color::Reset,
            "no opaque blocks when see-through"
        );
        theme.set_background_mode(BackgroundMode::Opaque);
        assert_eq!(theme.surface, theme.bg);
        assert_eq!(theme.raised, raised);
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        // No omarchy config in the test sandbox (or whatever's there parses) —
        // either way `load()` must not panic and must return *some* theme.
        let _ = load();
    }

    #[test]
    fn parses_a_ghostty_theme_with_inline_overrides_and_derived_shades() {
        let theme_file = "background = #171c22\nforeground = #d9d6cf\n\
            selection-background = #35464d\npalette = 6=#7fa6a2\npalette = 8=#73808c";
        let mut colors: std::collections::HashMap<String, String> =
            ghostty_pairs(theme_file).into_iter().collect();
        // A config line overrides the theme file.
        colors.extend(ghostty_pairs("palette = 5=#a092b4"));
        let t = Theme::from_ghostty(&colors).unwrap();
        assert_eq!(t.bg, Color::Rgb(0x17, 0x1c, 0x22));
        assert_eq!(t.accent, Color::Rgb(0x7f, 0xa6, 0xa2));
        assert_eq!(t.accent2, Color::Rgb(0xa0, 0x92, 0xb4));
        assert_eq!(t.selection, Color::Rgb(0x35, 0x46, 0x4d));
        // Raised sits just above the background, toward the foreground.
        let (Color::Rgb(r, ..), Color::Rgb(br, ..)) = (t.raised, t.bg) else {
            panic!("raised should be RGB");
        };
        assert!(r > br && r < 0x40, "{r:#x}");
    }

    #[test]
    fn ghostty_theme_needs_background_and_foreground() {
        let colors = ghostty_pairs("palette = 6=#7fa6a2").into_iter().collect();
        assert!(Theme::from_ghostty(&colors).is_none());
    }

    #[test]
    fn malformed_toml_falls_back_to_default() {
        let bad = "not = [valid";
        let parsed: Option<Alacritty> = toml::from_str(bad).ok();
        assert!(parsed.is_none());
    }
}
