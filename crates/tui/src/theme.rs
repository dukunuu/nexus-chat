//! Semantic color roles, sourced from the user's active omarchy theme
//! (`~/.config/omarchy/current/theme/alacritty.toml`) when present, falling
//! back to the app's original hardcoded ANSI palette otherwise. `events.rs`
//! polls `source_stamp()` and calls `load()` again when the omarchy
//! theme symlink changes, so switching themes in omarchy updates the running
//! app without a restart.

use nexus_core::theme_colors::{Palette, ThemeColors};
use ratatui::style::{Color, Style};

pub use nexus_core::theme_colors::source_stamp;
#[cfg(test)]
use nexus_core::theme_colors::{Alacritty, ghostty_pairs};

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
    /// Expanded diff row backgrounds.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
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
    /// Contrasting, opaque surface for compact controls and number chips.
    pub control: Color,
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
            diff_add_bg: Color::Indexed(22),
            diff_del_bg: Color::Indexed(52),
            user_msg: Color::Cyan,
            assistant_msg: Color::White,
            tool_msg: Color::Yellow,
            research_msg: Color::Magenta,
            // 256-color grays: the named ANSI palette has no in-between shade.
            raised: Color::Indexed(236),
            raised_opaque: Color::Indexed(236),
            control: Color::Indexed(238),
            selection: Color::Indexed(239),
        }
    }
}

impl Theme {
    fn from_palette(palette: &Palette) -> Self {
        let mut theme = Self::default();
        for (role, field) in [
            ("bg", &mut theme.bg),
            ("fg", &mut theme.fg),
            ("fg_dim", &mut theme.fg_dim),
            ("accent", &mut theme.accent),
            ("accent2", &mut theme.accent2),
            ("border", &mut theme.border),
            ("border_dim", &mut theme.border_dim),
            ("success", &mut theme.success),
            ("warning", &mut theme.warning),
            ("error", &mut theme.error),
            ("diff_add_bg", &mut theme.diff_add_bg),
            ("diff_del_bg", &mut theme.diff_del_bg),
            ("user_msg", &mut theme.user_msg),
            ("assistant_msg", &mut theme.assistant_msg),
            ("tool_msg", &mut theme.tool_msg),
            ("research_msg", &mut theme.research_msg),
            ("raised", &mut theme.raised),
            ("control", &mut theme.control),
            ("selection", &mut theme.selection),
        ] {
            if let Some([r, g, b]) = palette.rgb(role) {
                *field = Color::Rgb(r, g, b);
            }
        }
        theme.surface = theme.bg;
        theme.raised_opaque = theme.raised;
        theme
    }

    #[cfg(test)]
    fn from_alacritty(a: &Alacritty) -> Self {
        Self::from_palette(&Palette::from_alacritty(a))
    }

    #[cfg(test)]
    fn from_ghostty(colors: &std::collections::HashMap<String, String>) -> Option<Self> {
        Palette::from_ghostty(colors).map(|palette| Self::from_palette(&palette))
    }

    /// The opaque palette for studio; terminal transparency is not a deck color.
    #[must_use]
    pub fn colors(&self) -> ThemeColors {
        [
            ("bg", self.bg),
            ("fg", self.fg),
            ("fg_dim", self.fg_dim),
            ("accent", self.accent),
            ("accent2", self.accent2),
            ("border", self.border),
            ("border_dim", self.border_dim),
            ("success", self.success),
            ("warning", self.warning),
            ("error", self.error),
            ("raised", self.raised_opaque),
            ("control", self.control),
            ("selection", self.selection),
        ]
        .map(|(role, color)| (role.into(), color_hex(color)))
        .into()
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

fn color_hex(color: Color) -> String {
    use nexus_core::theme_colors::indexed_hex;
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(index) => indexed_hex(index),
        Color::Black | Color::Reset => indexed_hex(0),
        Color::Red => indexed_hex(1),
        Color::Green => indexed_hex(2),
        Color::Yellow => indexed_hex(3),
        Color::Blue => indexed_hex(4),
        Color::Magenta => indexed_hex(5),
        Color::Cyan => indexed_hex(6),
        Color::Gray => indexed_hex(7),
        Color::White => indexed_hex(15),
        Color::DarkGray => indexed_hex(8),
        Color::LightRed => indexed_hex(9),
        Color::LightGreen => indexed_hex(10),
        Color::LightYellow => indexed_hex(11),
        Color::LightBlue => indexed_hex(12),
        Color::LightMagenta => indexed_hex(13),
        Color::LightCyan => indexed_hex(14),
    }
}

/// The shared terminal palette, converted without losing ANSI fallbacks.
pub fn load() -> Theme {
    Theme::from_palette(&nexus_core::theme_colors::load())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_backgrounds_follow_rgb_palettes_with_ansi_fallbacks() {
        let theme = Theme::default();
        assert_eq!(theme.diff_add_bg, Color::Indexed(22));
        assert_eq!(theme.diff_del_bg, Color::Indexed(52));
        let colors = ghostty_pairs(
            "background = #101820\nforeground = #eeeeee\npalette = 2=#20d040\npalette = 1=#e02030",
        )
        .into_iter()
        .collect();
        let theme = Theme::from_ghostty(&colors).unwrap();
        assert_eq!(theme.diff_add_bg, Color::Rgb(19, 57, 38));
        assert_eq!(theme.diff_del_bg, Color::Rgb(53, 25, 35));
    }

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
    fn control_surface_has_ansi_fallback_and_rgb_contrast_in_both_theme_sources() {
        let fallback = Theme::default();
        assert_eq!(fallback.control, Color::Indexed(238));
        assert_ne!(fallback.control, fallback.bg);
        let a: Alacritty = toml::from_str(
            r##"
[colors.primary]
background = "#000000"
foreground = "#ffffff"
[colors.normal]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
magenta = "#ff00ff"
cyan = "#00ffff"
[colors.bright]
black = "#808080"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
magenta = "#ff00ff"
cyan = "#00ffff"
"##,
        )
        .unwrap();
        let colors = ghostty_pairs("background = #ffffff\nforeground = #000000")
            .into_iter()
            .collect();
        for (mut theme, control) in [
            (Theme::from_alacritty(&a), Color::Rgb(41, 41, 41)),
            (
                Theme::from_ghostty(&colors).unwrap(),
                Color::Rgb(214, 214, 214),
            ),
        ] {
            assert_eq!(theme.control, control);
            theme.set_background_mode(BackgroundMode::Transparent);
            assert_eq!(theme.control, control);
            theme.set_background_mode(BackgroundMode::Opaque);
            assert_eq!(theme.control, control);
        }
    }

    #[test]
    fn malformed_toml_falls_back_to_default() {
        let bad = "not = [valid";
        let parsed: Option<Alacritty> = toml::from_str(bad).ok();
        assert!(parsed.is_none());
    }
}
