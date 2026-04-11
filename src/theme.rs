//! Semantic color themes for the markdown viewer.
//!
//! Each field maps to a UI purpose — rendering code references `theme.heading`
//! or `theme.code_bg` rather than a raw `Color::Cyan` or `Color::DarkGrey`.
//!
//! Built-in themes are defined in [`THEMES`]. Users can add custom themes by
//! placing TOML files in `~/.config/reed/themes/`. The combined list (built-in
//! + user) is available via [`ALL_THEMES`]. See [`load_user_themes`].

use std::path::PathBuf;
use std::sync::LazyLock;

use crossterm::style::{Attribute, Color};
use serde::Deserialize;
use termimad::MadSkin;
use tracing::warn;

/// Semantic color theme for the markdown TUI.
///
/// Each field maps to a UI purpose so rendering code never references raw colors.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// Base terminal background.
    pub bg: Color,
    /// Default foreground / body text.
    pub fg: Color,
    /// Heading text color (h1–h6).
    pub heading: Color,
    /// Bold, italic, bullets, emphasis.
    pub accent: Color,
    /// De-emphasized text: horizontal rules, muted hints.
    pub muted: Color,
    /// Code block / inline code background.
    pub code_bg: Color,
    /// Code block / inline code foreground.
    pub code_fg: Color,
    /// Blockquote marker color.
    pub quote_mark: Color,
    /// Table borders, panel borders.
    pub border: Color,
    /// Header bar background.
    pub header_bg: Color,
    /// Header bar foreground.
    pub header_fg: Color,
    /// App title color (in header).
    pub title: Color,
}

/// Minimum terminal dimensions for the interactive viewer.
pub const MIN_TERM_WIDTH: u16 = 40;
pub const MIN_TERM_HEIGHT: u16 = 8;

impl Theme {
    /// Background color for rows containing a search match.
    /// A subtle tint derived from the accent color blended with the theme bg.
    pub fn search_match_bg(&self) -> Color {
        // If the theme uses a transparent bg (Color::Reset), use a dim tint.
        let (br, bg, bb) = match self.bg {
            Color::Rgb { r, g, b } => (r, g, b),
            _ => (0, 0, 0), // transparent bg — assume black
        };
        let (ar, ag, ab) = match self.accent {
            Color::Rgb { r, g, b } => (r, g, b),
            _ => (255, 220, 100),
        };
        // Blend: 85% bg + 15% accent
        let blend = |base: u8, tint: u8| -> u8 {
            // Result is always <= 255 since both inputs are u8
            #[allow(clippy::cast_possible_truncation)]
            let v = ((u16::from(base) * 85 + u16::from(tint) * 15) / 100) as u8;
            v
        };
        Color::Rgb {
            r: blend(br, ar).max(20),
            g: blend(bg, ag).max(18),
            b: blend(bb, ab).max(10),
        }
    }

    /// Background color for the *current* search match (more prominent).
    pub fn search_current_bg(&self) -> Color {
        let (br, bg, bb) = match self.bg {
            Color::Rgb { r, g, b } => (r, g, b),
            _ => (0, 0, 0),
        };
        let (ar, ag, ab) = match self.accent {
            Color::Rgb { r, g, b } => (r, g, b),
            _ => (255, 220, 100),
        };
        // Blend: 65% bg + 35% accent
        let blend = |base: u8, tint: u8| -> u8 {
            // Result is always <= 255 since both inputs are u8
            #[allow(clippy::cast_possible_truncation)]
            let v = ((u16::from(base) * 65 + u16::from(tint) * 35) / 100) as u8;
            v
        };
        Color::Rgb {
            r: blend(br, ar).max(40),
            g: blend(bg, ag).max(35),
            b: blend(bb, ab).max(15),
        }
    }
}

// ── User theme TOML schema ───────────────────────────────────────

/// TOML-friendly representation of a custom theme.
///
/// Colors are specified as `"#RRGGBB"` hex strings. The special value
/// `"reset"` maps to [`Color::Reset`] (transparent terminal background).
///
/// Example `~/.config/reed/themes/my-theme.toml`:
///
/// ```toml
/// name = "My Theme"
/// bg = "#1e1e2e"
/// fg = "#cdd6f4"
/// heading = "#89b4fa"
/// accent = "#f5c2e7"
/// muted = "#6c7086"
/// code_bg = "#181825"
/// code_fg = "#a6e3a1"
/// quote_mark = "#a6e3a1"
/// border = "#313244"
/// header_bg = "#181825"
/// header_fg = "#cdd6f4"
/// title = "#89b4fa"
/// ```
#[derive(Debug, Clone, Deserialize)]
struct UserTheme {
    name: String,
    bg: String,
    fg: String,
    heading: String,
    accent: String,
    muted: String,
    code_bg: String,
    code_fg: String,
    quote_mark: String,
    border: String,
    header_bg: String,
    header_fg: String,
    title: String,
}

/// Parse a hex color string like `"#RRGGBB"` or `"#RGB"` into a crossterm
/// [`Color`]. The special value `"reset"` (case-insensitive) yields
/// [`Color::Reset`].
fn parse_hex_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("reset") {
        return Some(Color::Reset);
    }
    let hex = s.strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb { r, g, b })
        }
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            Some(Color::Rgb { r, g, b })
        }
        _ => None,
    }
}

impl UserTheme {
    /// Convert to a [`Theme`]. The name string is leaked to produce a
    /// `&'static str` — acceptable because themes live for the entire
    /// process lifetime.
    fn into_theme(self) -> Option<Theme> {
        Some(Theme {
            name: Box::leak(self.name.into_boxed_str()),
            bg: parse_hex_color(&self.bg)?,
            fg: parse_hex_color(&self.fg)?,
            heading: parse_hex_color(&self.heading)?,
            accent: parse_hex_color(&self.accent)?,
            muted: parse_hex_color(&self.muted)?,
            code_bg: parse_hex_color(&self.code_bg)?,
            code_fg: parse_hex_color(&self.code_fg)?,
            quote_mark: parse_hex_color(&self.quote_mark)?,
            border: parse_hex_color(&self.border)?,
            header_bg: parse_hex_color(&self.header_bg)?,
            header_fg: parse_hex_color(&self.header_fg)?,
            title: parse_hex_color(&self.title)?,
        })
    }
}

/// Return the user themes directory (`~/.config/reed/themes/`).
fn user_themes_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "reed").map(|dirs| dirs.config_dir().join("themes"))
}

/// Load custom themes from TOML files in `~/.config/reed/themes/*.toml`.
///
/// Invalid files are silently skipped (with a `tracing::warn`).
fn load_user_themes() -> Vec<Theme> {
    let Some(dir) = user_themes_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut themes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<UserTheme>(&content) {
                    Ok(ut) => {
                        let name = ut.name.clone();
                        if let Some(theme) = ut.into_theme() {
                            themes.push(theme);
                        } else {
                            warn!(
                                file = %path.display(),
                                name,
                                "user theme has invalid hex color(s), skipping"
                            );
                        }
                    }
                    Err(e) => warn!(
                        file = %path.display(),
                        error = %e,
                        "failed to parse user theme TOML, skipping"
                    ),
                },
                Err(e) => warn!(
                    file = %path.display(),
                    error = %e,
                    "failed to read user theme file, skipping"
                ),
            }
        }
    }
    // Sort by name for deterministic ordering.
    themes.sort_by(|a, b| a.name.cmp(b.name));
    themes
}

/// All available themes: built-in [`THEMES`] followed by user-defined themes
/// loaded from `~/.config/reed/themes/*.toml`.
///
/// Computed once on first access.
pub static ALL_THEMES: LazyLock<Vec<Theme>> = LazyLock::new(|| {
    let mut all = THEMES.to_vec();
    let user = load_user_themes();
    // Skip user themes whose names collide with built-in themes.
    for theme in user {
        if all.iter().any(|t| t.name == theme.name) {
            warn!(
                name = theme.name,
                "user theme name collides with built-in theme, skipping"
            );
        } else {
            all.push(theme);
        }
    }
    all
});

/// Built-in themes, indexed by position. First entry is the default.
///
/// For the full list of themes (built-in + user), use [`ALL_THEMES`].
pub const THEMES: &[Theme] = &[
    // ── Dark themes ──────────────────────────────────────────────
    //
    // Default — transparent bg, cyan accent
    Theme {
        name: "Default",
        bg: Color::Reset,
        fg: Color::White,
        heading: Color::Rgb {
            r: 0,
            g: 217,
            b: 255,
        },
        accent: Color::Rgb {
            r: 255,
            g: 220,
            b: 100,
        },
        muted: Color::DarkGrey,
        code_bg: Color::Rgb {
            r: 24,
            g: 24,
            b: 30,
        },
        code_fg: Color::Rgb {
            r: 180,
            g: 210,
            b: 115,
        },
        quote_mark: Color::Rgb {
            r: 0,
            g: 200,
            b: 80,
        },
        border: Color::DarkGrey,
        header_bg: Color::Rgb {
            r: 24,
            g: 24,
            b: 30,
        },
        header_fg: Color::Rgb {
            r: 0,
            g: 217,
            b: 255,
        },

        title: Color::Rgb {
            r: 0,
            g: 217,
            b: 255,
        },
    },
    // Gruvbox Dark — warm earthy tones
    Theme {
        name: "Gruvbox",
        bg: Color::Rgb {
            r: 29,
            g: 32,
            b: 33,
        },
        fg: Color::Rgb {
            r: 235,
            g: 219,
            b: 178,
        },
        heading: Color::Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
        accent: Color::Rgb {
            r: 215,
            g: 153,
            b: 33,
        },
        muted: Color::Rgb {
            r: 146,
            g: 131,
            b: 116,
        },
        code_bg: Color::Rgb {
            r: 37,
            g: 36,
            b: 36,
        },
        code_fg: Color::Rgb {
            r: 184,
            g: 187,
            b: 38,
        },
        quote_mark: Color::Rgb {
            r: 215,
            g: 153,
            b: 33,
        },
        border: Color::Rgb {
            r: 62,
            g: 57,
            b: 54,
        },
        header_bg: Color::Rgb {
            r: 37,
            g: 36,
            b: 36,
        },
        header_fg: Color::Rgb {
            r: 235,
            g: 219,
            b: 178,
        },

        title: Color::Rgb {
            r: 250,
            g: 189,
            b: 47,
        },
    },
    // Solarized Dark — blue-cyan palette
    Theme {
        name: "Solarized",
        bg: Color::Rgb { r: 0, g: 43, b: 54 },
        fg: Color::Rgb {
            r: 253,
            g: 246,
            b: 227,
        },
        heading: Color::Rgb {
            r: 181,
            g: 137,
            b: 0,
        },
        accent: Color::Rgb {
            r: 42,
            g: 161,
            b: 152,
        },
        muted: Color::Rgb {
            r: 131,
            g: 148,
            b: 150,
        },
        code_bg: Color::Rgb { r: 7, g: 54, b: 66 },
        code_fg: Color::Rgb {
            r: 133,
            g: 153,
            b: 0,
        },
        quote_mark: Color::Rgb {
            r: 42,
            g: 161,
            b: 152,
        },
        border: Color::Rgb {
            r: 16,
            g: 58,
            b: 68,
        },
        header_bg: Color::Rgb { r: 7, g: 54, b: 66 },
        header_fg: Color::Rgb {
            r: 253,
            g: 246,
            b: 227,
        },

        title: Color::Rgb {
            r: 181,
            g: 137,
            b: 0,
        },
    },
    // Ayu Dark — deep blue with orange accents
    Theme {
        name: "Ayu",
        bg: Color::Rgb {
            r: 10,
            g: 14,
            b: 20,
        },
        fg: Color::Rgb {
            r: 191,
            g: 191,
            b: 191,
        },
        heading: Color::Rgb {
            r: 255,
            g: 180,
            b: 84,
        },
        accent: Color::Rgb {
            r: 255,
            g: 153,
            b: 64,
        },
        muted: Color::Rgb {
            r: 92,
            g: 103,
            b: 115,
        },
        code_bg: Color::Rgb {
            r: 18,
            g: 22,
            b: 30,
        },
        code_fg: Color::Rgb {
            r: 125,
            g: 210,
            b: 80,
        },
        quote_mark: Color::Rgb {
            r: 255,
            g: 153,
            b: 64,
        },
        border: Color::Rgb {
            r: 40,
            g: 44,
            b: 52,
        },
        header_bg: Color::Rgb {
            r: 18,
            g: 22,
            b: 30,
        },
        header_fg: Color::Rgb {
            r: 191,
            g: 191,
            b: 191,
        },

        title: Color::Rgb {
            r: 255,
            g: 180,
            b: 84,
        },
    },
    // Flexoki Dark — ink-and-paper warmth
    Theme {
        name: "Flexoki",
        bg: Color::Rgb {
            r: 16,
            g: 15,
            b: 15,
        },
        fg: Color::Rgb {
            r: 206,
            g: 205,
            b: 195,
        },
        heading: Color::Rgb {
            r: 208,
            g: 162,
            b: 21,
        },
        accent: Color::Rgb {
            r: 36,
            g: 131,
            b: 123,
        },
        muted: Color::Rgb {
            r: 135,
            g: 133,
            b: 128,
        },
        code_bg: Color::Rgb {
            r: 24,
            g: 23,
            b: 22,
        },
        code_fg: Color::Rgb {
            r: 206,
            g: 205,
            b: 195,
        },
        quote_mark: Color::Rgb {
            r: 36,
            g: 131,
            b: 123,
        },
        border: Color::Rgb {
            r: 40,
            g: 39,
            b: 38,
        },
        header_bg: Color::Rgb {
            r: 24,
            g: 23,
            b: 22,
        },
        header_fg: Color::Rgb {
            r: 206,
            g: 205,
            b: 195,
        },

        title: Color::Rgb {
            r: 208,
            g: 162,
            b: 21,
        },
    },
    // Zoegi Dark — muted monochrome with green accent
    Theme {
        name: "Zoegi",
        bg: Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        },
        fg: Color::Rgb {
            r: 204,
            g: 204,
            b: 204,
        },
        heading: Color::Rgb {
            r: 128,
            g: 200,
            b: 160,
        },
        accent: Color::Rgb {
            r: 64,
            g: 128,
            b: 104,
        },
        muted: Color::Rgb {
            r: 89,
            g: 89,
            b: 89,
        },
        code_bg: Color::Rgb {
            r: 28,
            g: 28,
            b: 28,
        },
        code_fg: Color::Rgb {
            r: 92,
            g: 168,
            b: 112,
        },
        quote_mark: Color::Rgb {
            r: 64,
            g: 128,
            b: 104,
        },
        border: Color::Rgb {
            r: 48,
            g: 48,
            b: 48,
        },
        header_bg: Color::Rgb {
            r: 28,
            g: 28,
            b: 28,
        },
        header_fg: Color::Rgb {
            r: 204,
            g: 204,
            b: 204,
        },

        title: Color::Rgb {
            r: 128,
            g: 200,
            b: 160,
        },
    },
    // FFE Dark — Nordic-inspired cool blues
    Theme {
        name: "FFE Dark",
        bg: Color::Rgb {
            r: 30,
            g: 35,
            b: 43,
        },
        fg: Color::Rgb {
            r: 216,
            g: 222,
            b: 233,
        },
        heading: Color::Rgb {
            r: 240,
            g: 169,
            b: 136,
        },
        accent: Color::Rgb {
            r: 79,
            g: 214,
            b: 190,
        },
        muted: Color::Rgb {
            r: 155,
            g: 162,
            b: 175,
        },
        code_bg: Color::Rgb {
            r: 26,
            g: 31,
            b: 39,
        },
        code_fg: Color::Rgb {
            r: 161,
            g: 239,
            b: 211,
        },
        quote_mark: Color::Rgb {
            r: 79,
            g: 214,
            b: 190,
        },
        border: Color::Rgb {
            r: 59,
            g: 66,
            b: 82,
        },
        header_bg: Color::Rgb {
            r: 26,
            g: 31,
            b: 39,
        },
        header_fg: Color::Rgb {
            r: 216,
            g: 222,
            b: 233,
        },

        title: Color::Rgb {
            r: 240,
            g: 169,
            b: 136,
        },
    },
    // ── Light themes ─────────────────────────────────────────────
    //
    // Default Light — transparent bg, readable on light terminals
    Theme {
        name: "Default Light",
        bg: Color::Reset,
        fg: Color::Rgb {
            r: 40,
            g: 40,
            b: 50,
        },
        heading: Color::Rgb {
            r: 0,
            g: 140,
            b: 180,
        },
        accent: Color::Rgb {
            r: 200,
            g: 120,
            b: 0,
        },
        muted: Color::Rgb {
            r: 120,
            g: 120,
            b: 130,
        },
        code_bg: Color::Rgb {
            r: 235,
            g: 235,
            b: 240,
        },
        code_fg: Color::Rgb {
            r: 40,
            g: 40,
            b: 50,
        },
        quote_mark: Color::Rgb {
            r: 0,
            g: 140,
            b: 50,
        },
        border: Color::Rgb {
            r: 180,
            g: 180,
            b: 190,
        },
        header_bg: Color::Rgb {
            r: 235,
            g: 235,
            b: 240,
        },
        header_fg: Color::Rgb {
            r: 40,
            g: 40,
            b: 50,
        },

        title: Color::Rgb {
            r: 0,
            g: 140,
            b: 180,
        },
    },
    // Gruvbox Light — warm parchment tones
    Theme {
        name: "Gruvbox Light",
        bg: Color::Rgb {
            r: 251,
            g: 241,
            b: 199,
        },
        fg: Color::Rgb {
            r: 60,
            g: 56,
            b: 54,
        },
        heading: Color::Rgb {
            r: 215,
            g: 153,
            b: 33,
        },
        accent: Color::Rgb {
            r: 152,
            g: 103,
            b: 0,
        },
        muted: Color::Rgb {
            r: 146,
            g: 131,
            b: 116,
        },
        code_bg: Color::Rgb {
            r: 242,
            g: 233,
            b: 185,
        },
        code_fg: Color::Rgb {
            r: 121,
            g: 116,
            b: 14,
        },
        quote_mark: Color::Rgb {
            r: 152,
            g: 103,
            b: 0,
        },
        border: Color::Rgb {
            r: 213,
            g: 196,
            b: 161,
        },
        header_bg: Color::Rgb {
            r: 242,
            g: 233,
            b: 185,
        },
        header_fg: Color::Rgb {
            r: 60,
            g: 56,
            b: 54,
        },

        title: Color::Rgb {
            r: 215,
            g: 153,
            b: 33,
        },
    },
    // Solarized Light — bright blue-cyan
    Theme {
        name: "Solarized Light",
        bg: Color::Rgb {
            r: 253,
            g: 246,
            b: 227,
        },
        fg: Color::Rgb {
            r: 88,
            g: 110,
            b: 117,
        },
        heading: Color::Rgb {
            r: 181,
            g: 137,
            b: 0,
        },
        accent: Color::Rgb {
            r: 42,
            g: 161,
            b: 152,
        },
        muted: Color::Rgb {
            r: 147,
            g: 161,
            b: 161,
        },
        code_bg: Color::Rgb {
            r: 238,
            g: 232,
            b: 213,
        },
        code_fg: Color::Rgb {
            r: 133,
            g: 153,
            b: 0,
        },
        quote_mark: Color::Rgb {
            r: 42,
            g: 161,
            b: 152,
        },
        border: Color::Rgb {
            r: 220,
            g: 212,
            b: 188,
        },
        header_bg: Color::Rgb {
            r: 238,
            g: 232,
            b: 213,
        },
        header_fg: Color::Rgb {
            r: 88,
            g: 110,
            b: 117,
        },

        title: Color::Rgb {
            r: 181,
            g: 137,
            b: 0,
        },
    },
    // Flexoki Light — soft warm paper
    Theme {
        name: "Flexoki Light",
        bg: Color::Rgb {
            r: 255,
            g: 252,
            b: 240,
        },
        fg: Color::Rgb {
            r: 16,
            g: 15,
            b: 15,
        },
        heading: Color::Rgb {
            r: 36,
            g: 131,
            b: 123,
        },
        accent: Color::Rgb {
            r: 102,
            g: 128,
            b: 11,
        },
        muted: Color::Rgb {
            r: 111,
            g: 110,
            b: 105,
        },
        code_bg: Color::Rgb {
            r: 244,
            g: 241,
            b: 230,
        },
        code_fg: Color::Rgb {
            r: 16,
            g: 15,
            b: 15,
        },
        quote_mark: Color::Rgb {
            r: 102,
            g: 128,
            b: 11,
        },
        border: Color::Rgb {
            r: 230,
            g: 228,
            b: 217,
        },
        header_bg: Color::Rgb {
            r: 244,
            g: 241,
            b: 230,
        },
        header_fg: Color::Rgb {
            r: 16,
            g: 15,
            b: 15,
        },

        title: Color::Rgb {
            r: 36,
            g: 131,
            b: 123,
        },
    },
    // Ayu Light — bright with orange warmth
    Theme {
        name: "Ayu Light",
        bg: Color::Rgb {
            r: 252,
            g: 252,
            b: 252,
        },
        fg: Color::Rgb {
            r: 92,
            g: 97,
            b: 102,
        },
        heading: Color::Rgb {
            r: 255,
            g: 153,
            b: 64,
        },
        accent: Color::Rgb {
            r: 133,
            g: 179,
            b: 4,
        },
        muted: Color::Rgb {
            r: 153,
            g: 160,
            b: 166,
        },
        code_bg: Color::Rgb {
            r: 242,
            g: 242,
            b: 242,
        },
        code_fg: Color::Rgb {
            r: 92,
            g: 97,
            b: 102,
        },
        quote_mark: Color::Rgb {
            r: 133,
            g: 179,
            b: 4,
        },
        border: Color::Rgb {
            r: 207,
            g: 209,
            b: 210,
        },
        header_bg: Color::Rgb {
            r: 242,
            g: 242,
            b: 242,
        },
        header_fg: Color::Rgb {
            r: 92,
            g: 97,
            b: 102,
        },

        title: Color::Rgb {
            r: 255,
            g: 153,
            b: 64,
        },
    },
    // Zoegi Light — clean minimal green
    Theme {
        name: "Zoegi Light",
        bg: Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        fg: Color::Rgb {
            r: 51,
            g: 51,
            b: 51,
        },
        heading: Color::Rgb {
            r: 55,
            g: 121,
            b: 97,
        },
        accent: Color::Rgb {
            r: 55,
            g: 121,
            b: 97,
        },
        muted: Color::Rgb {
            r: 89,
            g: 89,
            b: 89,
        },
        code_bg: Color::Rgb {
            r: 245,
            g: 245,
            b: 245,
        },
        code_fg: Color::Rgb {
            r: 55,
            g: 121,
            b: 97,
        },
        quote_mark: Color::Rgb {
            r: 55,
            g: 121,
            b: 97,
        },
        border: Color::Rgb {
            r: 230,
            g: 230,
            b: 230,
        },
        header_bg: Color::Rgb {
            r: 245,
            g: 245,
            b: 245,
        },
        header_fg: Color::Rgb {
            r: 51,
            g: 51,
            b: 51,
        },

        title: Color::Rgb {
            r: 55,
            g: 121,
            b: 97,
        },
    },
    // FFE Light — soft Nordic daylight
    Theme {
        name: "FFE Light",
        bg: Color::Rgb {
            r: 232,
            g: 236,
            b: 240,
        },
        fg: Color::Rgb {
            r: 30,
            g: 35,
            b: 43,
        },
        heading: Color::Rgb {
            r: 192,
            g: 121,
            b: 32,
        },
        accent: Color::Rgb {
            r: 42,
            g: 157,
            b: 132,
        },
        muted: Color::Rgb {
            r: 74,
            g: 80,
            b: 96,
        },
        code_bg: Color::Rgb {
            r: 245,
            g: 247,
            b: 250,
        },
        code_fg: Color::Rgb {
            r: 26,
            g: 138,
            b: 110,
        },
        quote_mark: Color::Rgb {
            r: 42,
            g: 157,
            b: 132,
        },
        border: Color::Rgb {
            r: 201,
            g: 205,
            b: 214,
        },
        header_bg: Color::Rgb {
            r: 245,
            g: 247,
            b: 250,
        },
        header_fg: Color::Rgb {
            r: 30,
            g: 35,
            b: 43,
        },

        title: Color::Rgb {
            r: 192,
            g: 121,
            b: 32,
        },
    },
];

/// Look up a theme index by name (case-sensitive) in [`ALL_THEMES`].
/// Returns 0 (Default) if no theme matches.
#[must_use]
pub fn theme_index_by_name(name: &str) -> usize {
    ALL_THEMES.iter().position(|t| t.name == name).unwrap_or(0)
}

/// Build a [`MadSkin`] configured with the given theme colors.
///
/// Body text is left unstyled so the viewer's render loop can apply the
/// theme's `fg`/`bg` as the fallback for cells without explicit ANSI colors.
pub fn build_skin(theme: &Theme) -> MadSkin {
    let mut skin = MadSkin::default();

    // Headers — prominent, bold, no underline.
    // termimad's default adds Underlined to all headers and centers h1;
    // we override to just bold + theme color with left alignment.
    for h in &mut skin.headers {
        h.set_fg(theme.heading);
        h.add_attr(Attribute::Bold);
        h.compound_style
            .object_style
            .attributes
            .unset(Attribute::Underlined);
        h.align = termimad::Alignment::Left;
    }

    // Bold / italic
    skin.bold.set_fg(theme.accent);
    skin.italic.set_fg(theme.accent);
    skin.strikeout.set_fg(theme.muted);

    // Code
    skin.inline_code.set_fg(theme.code_fg);
    skin.inline_code.set_bg(theme.code_bg);
    skin.code_block.set_fg(theme.code_fg);
    skin.code_block.set_bg(theme.code_bg);
    skin.code_block.left_margin = 2;

    // Blockquote marker
    skin.quote_mark.set_fg(theme.quote_mark);

    // Horizontal rule
    skin.horizontal_rule.set_fg(theme.muted);

    // Bullet — use a proper bullet character instead of dashes
    skin.bullet.set_char('\u{2022}'); // •
    skin.bullet.set_fg(theme.accent);

    skin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_count_matches_expected() {
        assert_eq!(THEMES.len(), 14);
    }

    #[test]
    fn default_theme_is_first() {
        assert_eq!(THEMES[0].name, "Default");
    }

    #[test]
    fn theme_index_by_name_found() {
        assert_eq!(theme_index_by_name("Gruvbox"), 1);
        assert_eq!(theme_index_by_name("Ayu"), 3);
    }

    #[test]
    fn theme_index_by_name_not_found_returns_zero() {
        assert_eq!(theme_index_by_name("Nonexistent"), 0);
    }

    #[test]
    fn all_themes_have_unique_names() {
        let names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "Duplicate theme name: {name}");
        }
    }

    #[test]
    fn all_themes_have_non_empty_names() {
        for theme in THEMES {
            assert!(!theme.name.is_empty(), "Theme name must not be empty");
        }
    }

    #[test]
    fn theme_index_by_name_is_case_sensitive() {
        // "solarized" (lowercase) should not match "Solarized"
        assert_eq!(theme_index_by_name("solarized"), 0);
        assert_ne!(theme_index_by_name("Solarized"), 0);
    }

    #[test]
    fn build_skin_does_not_panic() {
        for theme in THEMES {
            let _ = build_skin(theme);
        }
    }

    // ── User theme / hex parsing tests ───────────────────────────

    #[test]
    fn parse_hex_color_6digit() {
        assert_eq!(
            parse_hex_color("#1e1e2e"),
            Some(Color::Rgb {
                r: 0x1e,
                g: 0x1e,
                b: 0x2e,
            })
        );
    }

    #[test]
    fn parse_hex_color_3digit() {
        // #abc → #aabbcc
        assert_eq!(
            parse_hex_color("#abc"),
            Some(Color::Rgb {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            })
        );
    }

    #[test]
    fn parse_hex_color_reset() {
        assert_eq!(parse_hex_color("reset"), Some(Color::Reset));
        assert_eq!(parse_hex_color("RESET"), Some(Color::Reset));
    }

    #[test]
    fn parse_hex_color_invalid() {
        assert_eq!(parse_hex_color("notacolor"), None);
        assert_eq!(parse_hex_color("#zzzzzz"), None);
        assert_eq!(parse_hex_color("#12"), None);
    }

    #[test]
    fn user_theme_toml_roundtrip() {
        let toml_str = r##"
name = "Test Theme"
bg = "#1e1e2e"
fg = "#cdd6f4"
heading = "#89b4fa"
accent = "#f5c2e7"
muted = "#6c7086"
code_bg = "#181825"
code_fg = "#a6e3a1"
quote_mark = "#a6e3a1"
border = "#313244"
header_bg = "#181825"
header_fg = "#cdd6f4"
title = "#89b4fa"
"##;
        let ut: UserTheme = toml::from_str(toml_str).unwrap();
        let theme = ut.into_theme().expect("should parse all colors");
        assert_eq!(theme.name, "Test Theme");
        assert_eq!(
            theme.bg,
            Color::Rgb {
                r: 0x1e,
                g: 0x1e,
                b: 0x2e
            }
        );
    }

    #[test]
    fn user_theme_bad_hex_returns_none() {
        let toml_str = r##"
name = "Bad"
bg = "nope"
fg = "#cdd6f4"
heading = "#89b4fa"
accent = "#f5c2e7"
muted = "#6c7086"
code_bg = "#181825"
code_fg = "#a6e3a1"
quote_mark = "#a6e3a1"
border = "#313244"
header_bg = "#181825"
header_fg = "#cdd6f4"
title = "#89b4fa"
"##;
        let ut: UserTheme = toml::from_str(toml_str).unwrap();
        assert!(ut.into_theme().is_none(), "bad hex should fail into_theme");
    }

    #[test]
    fn user_theme_reset_bg() {
        let toml_str = r##"
name = "Transparent"
bg = "reset"
fg = "#ffffff"
heading = "#ffffff"
accent = "#ffffff"
muted = "#ffffff"
code_bg = "#000000"
code_fg = "#ffffff"
quote_mark = "#ffffff"
border = "#ffffff"
header_bg = "#000000"
header_fg = "#ffffff"
title = "#ffffff"
"##;
        let ut: UserTheme = toml::from_str(toml_str).unwrap();
        let theme = ut.into_theme().unwrap();
        assert_eq!(theme.bg, Color::Reset);
    }

    #[test]
    fn all_themes_includes_builtins() {
        // ALL_THEMES should have at least the built-in themes.
        assert!(ALL_THEMES.len() >= THEMES.len());
        // First theme should be Default.
        assert_eq!(ALL_THEMES[0].name, "Default");
    }

    #[test]
    fn user_themes_dir_is_some() {
        assert!(user_themes_dir().is_some());
    }
}
