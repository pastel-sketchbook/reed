//! Semantic color themes for the markdown viewer.
//!
//! Each field maps to a UI purpose — rendering code references `theme.heading`
//! or `theme.code_bg` rather than a raw `Color::Cyan` or `Color::DarkGrey`.

use crossterm::style::{Attribute, Color};
use termimad::MadSkin;

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

/// Available themes, indexed by position. First entry is the default.
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

/// Look up a theme index by name (case-sensitive). Returns 0 (Default)
/// if no theme matches.
#[must_use]
pub fn theme_index_by_name(name: &str) -> usize {
    THEMES.iter().position(|t| t.name == name).unwrap_or(0)
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
}
