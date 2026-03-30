//! User configuration persistence.
//!
//! Preferences (theme name) are stored as TOML in the OS config directory
//! (`~/.config/reed/preferences.toml` on Linux/macOS).

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::theme;

/// Application name used for directory paths.
const APP_NAME: &str = "reed";

/// Persistent user preferences (TOML).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preferences {
    /// Theme name (must match a name in `theme::THEMES`).
    pub theme: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: default_theme_name().to_string(),
        }
    }
}

/// Returns `true` when running inside the Ghostty terminal.
#[must_use]
pub fn is_ghostty() -> bool {
    std::env::var("TERM").is_ok_and(|t| t == "xterm-ghostty")
}

/// Pick the default theme name (first theme in `theme::THEMES`).
fn default_theme_name() -> &'static str {
    theme::THEMES[0].name
}

/// Resolve the effective theme name.
///
/// Ghostty always forces `"FFE Dark"` regardless of CLI flags or saved
/// preferences.  Otherwise: CLI flag > saved preference > default.
#[must_use]
pub fn resolve_theme_name<'a>(cli_theme: Option<&'a str>, saved_theme: &'a str) -> &'a str {
    if is_ghostty() {
        return theme::THEMES
            .iter()
            .find(|t| t.name == "FFE Dark")
            .map_or(theme::THEMES[0].name, |t| t.name);
    }
    cli_theme.unwrap_or(saved_theme)
}

/// Resolve the preferences file path.
///
/// Returns `~/.config/reed/preferences.toml` (or OS equivalent).
#[must_use]
pub fn preferences_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|dirs| dirs.config_dir().join("preferences.toml"))
}

/// Load preferences from disk, falling back to defaults on any error.
#[must_use]
pub fn load_preferences() -> Preferences {
    preferences_path()
        .and_then(|p| fs::read_to_string(&p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save preferences to disk.
///
/// Creates parent directories if they don't exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file
/// cannot be written.
pub fn save_preferences(prefs: &Preferences) -> Result<()> {
    let path = preferences_path().context("could not determine config directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("could not create config directory")?;
    }
    let content = toml::to_string_pretty(prefs).context("could not serialize preferences")?;
    fs::write(&path, content).context("could not write preferences file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_default_uses_expected_theme() {
        let prefs = Preferences::default();
        assert_eq!(prefs.theme, default_theme_name());
    }

    #[test]
    fn preferences_roundtrip_toml() {
        let prefs = Preferences {
            theme: "Gruvbox".to_string(),
        };
        let toml_str = toml::to_string_pretty(&prefs).unwrap();
        let parsed: Preferences = toml::from_str(&toml_str).unwrap();
        assert_eq!(prefs, parsed);
    }

    #[test]
    fn preferences_path_is_some() {
        assert!(preferences_path().is_some());
    }

    #[test]
    fn preferences_path_ends_with_toml() {
        let p = preferences_path().unwrap();
        assert!(
            p.to_string_lossy().ends_with("preferences.toml"),
            "unexpected path: {p:?}"
        );
    }

    #[test]
    fn load_preferences_returns_default_when_no_file() {
        let prefs = load_preferences();
        assert!(!prefs.theme.is_empty());
    }
}
