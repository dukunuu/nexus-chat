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

    /// Cycle between the two background modes.
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
        }
    }

    /// Apply a UI background mode without changing the palette colors.
    pub fn set_background_mode(&mut self, mode: BackgroundMode) {
        self.surface = match mode {
            BackgroundMode::Opaque => self.bg,
            BackgroundMode::Transparent => Color::Reset,
        };
    }

    /// Style used to paint the TUI's general surface.
    #[must_use]
    pub fn background_style(&self) -> Style {
        Style::default().bg(self.surface)
    }
}

fn omarchy_current_dir() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".config/omarchy/current"))
}

/// The omarchy theme symlink's current target (e.g. `.../themes/retropc`), or
/// `None` off-omarchy. Compared across polls to detect a theme switch.
pub fn current_link_target() -> Option<PathBuf> {
    std::fs::read_link(omarchy_current_dir()?).ok()
}

/// Load the active omarchy theme's colors, or the built-in default palette if
/// omarchy isn't installed or the theme file doesn't parse.
pub fn load() -> Theme {
    omarchy_current_dir()
        .map(|d| d.join("theme/alacritty.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<Alacritty>(&s).ok())
        .map(|a| Theme::from_alacritty(&a))
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
        theme.set_background_mode(BackgroundMode::Transparent);
        assert_eq!(theme.surface, Color::Reset);
        theme.set_background_mode(BackgroundMode::Opaque);
        assert_eq!(theme.surface, theme.bg);
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        // No omarchy config in the test sandbox (or whatever's there parses) —
        // either way `load()` must not panic and must return *some* theme.
        let _ = load();
    }

    #[test]
    fn malformed_toml_falls_back_to_default() {
        let bad = "not = [valid";
        let parsed: Option<Alacritty> = toml::from_str(bad).ok();
        assert!(parsed.is_none());
    }
}
