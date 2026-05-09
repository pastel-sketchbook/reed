//! AI-powered file type detection via Google Magika.
//!
//! When the `detect-content-type` feature is enabled, this module initialises
//! a global [`magika::Session`] at startup and exposes [`detect_file_type`] to
//! identify files by their content rather than their extension.

use std::path::Path;

#[cfg(feature = "detect-content-type")]
use std::sync::{Mutex, OnceLock};

// ── Global session (feature-gated) ──────────────────────────────

#[cfg(feature = "detect-content-type")]
static MAGIKA: OnceLock<Mutex<magika::Session>> = OnceLock::new();

/// Initialise the global Magika session.  Call once at startup.
/// Returns `true` if initialisation succeeded.
#[cfg(feature = "detect-content-type")]
pub fn init() -> bool {
    match magika::Session::new() {
        Ok(session) => {
            let _ = MAGIKA.set(Mutex::new(session));
            tracing::info!("Magika content detection enabled");
            true
        }
        Err(e) => {
            tracing::warn!("Magika init failed (content detection disabled): {e}");
            false
        }
    }
}

/// Stub when the feature is disabled.
#[cfg(not(feature = "detect-content-type"))]
pub fn init() -> bool {
    false
}

// ── Detection result ────────────────────────────────────────────

/// Result of AI-powered content type detection.
#[derive(Debug, Clone)]
pub struct DetectedType {
    /// Short machine-readable label (e.g. `"markdown"`, `"rust"`, `"png"`).
    pub label: String,
    /// MIME type (e.g. `"text/markdown"`, `"image/png"`).
    pub mime_type: String,
    /// Human-readable description (e.g. `"Markdown document"`).
    pub description: String,
    /// Content group (e.g. `"text"`, `"image"`, `"executable"`).
    pub group: String,
    /// Confidence score in `[0.0, 1.0]`.
    pub score: f32,
    /// Whether the content is textual.
    pub is_text: bool,
}

// ── Public API ──────────────────────────────────────────────────

/// Detect the content type of a file by reading its bytes.
///
/// Returns `None` when the feature is disabled, the session failed to
/// initialise, or Magika could not identify the content.
#[cfg(feature = "detect-content-type")]
pub fn detect_file_type(path: &Path) -> Option<DetectedType> {
    let bytes = std::fs::read(path).ok()?;
    detect_bytes(&bytes)
}

/// Detect the content type of raw bytes.
#[cfg(feature = "detect-content-type")]
pub fn detect_bytes(bytes: &[u8]) -> Option<DetectedType> {
    let session = MAGIKA.get()?;
    let mut guard = session.lock().ok()?;
    let ft = guard.identify_content_sync(bytes).ok()?;
    let info = ft.info();
    Some(DetectedType {
        label: info.label.to_string(),
        mime_type: info.mime_type.to_string(),
        description: info.description.to_string(),
        group: info.group.to_string(),
        score: ft.score(),
        is_text: info.is_text,
    })
}

/// Stub when the feature is disabled.
#[cfg(not(feature = "detect-content-type"))]
pub fn detect_file_type(_path: &Path) -> Option<DetectedType> {
    None
}

/// Stub when the feature is disabled.
#[cfg(not(feature = "detect-content-type"))]
pub fn detect_bytes(_bytes: &[u8]) -> Option<DetectedType> {
    None
}

/// Returns `true` when the detected type indicates markdown content,
/// with sufficient confidence.
pub fn is_detected_markdown(detected: &DetectedType) -> bool {
    detected.score >= 0.7 && detected.label == "markdown"
}

/// Map a Magika label to a syntax-highlighting language identifier.
///
/// Returns `None` for labels that don't map to a known language (e.g.
/// generic `"txt"` or `"unknown"`).
pub fn label_to_lang(label: &str) -> Option<&'static str> {
    match label {
        "rust" => Some("rs"),
        "python" => Some("py"),
        "javascript" => Some("js"),
        "typescript" => Some("ts"),
        "java" => Some("java"),
        "c" => Some("c"),
        "cpp" => Some("cpp"),
        "csharp" => Some("cs"),
        "go" => Some("go"),
        "ruby" => Some("rb"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "kotlin" => Some("kt"),
        "scala" => Some("scala"),
        "shell" => Some("sh"),
        "bash" => Some("sh"),
        "zsh" => Some("zsh"),
        "perl" => Some("pl"),
        "lua" => Some("lua"),
        "r" => Some("r"),
        "sql" => Some("sql"),
        "html" => Some("html"),
        "css" => Some("css"),
        "xml" => Some("xml"),
        "json" => Some("json"),
        "yaml" => Some("yaml"),
        "toml" => Some("toml"),
        "ini" => Some("ini"),
        "dockerfile" => Some("dockerfile"),
        "makefile" => Some("makefile"),
        "cmake" => Some("cmake"),
        "zig" => Some("zig"),
        "elixir" => Some("ex"),
        "erlang" => Some("erl"),
        "haskell" => Some("hs"),
        "ocaml" => Some("ml"),
        "latex" => Some("tex"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_to_lang_known() {
        assert_eq!(label_to_lang("rust"), Some("rs"));
        assert_eq!(label_to_lang("python"), Some("py"));
        assert_eq!(label_to_lang("zig"), Some("zig"));
    }

    #[test]
    fn label_to_lang_unknown() {
        assert_eq!(label_to_lang("txt"), None);
        assert_eq!(label_to_lang("unknown"), None);
    }

    #[test]
    fn is_detected_markdown_checks_score() {
        let high = DetectedType {
            label: "markdown".into(),
            mime_type: "text/markdown".into(),
            description: "Markdown".into(),
            group: "text".into(),
            score: 0.95,
            is_text: true,
        };
        assert!(is_detected_markdown(&high));

        let low = DetectedType {
            label: "markdown".into(),
            mime_type: "text/markdown".into(),
            description: "Markdown".into(),
            group: "text".into(),
            score: 0.5,
            is_text: true,
        };
        assert!(!is_detected_markdown(&low));

        let wrong_label = DetectedType {
            label: "txt".into(),
            mime_type: "text/plain".into(),
            description: "Text".into(),
            group: "text".into(),
            score: 0.99,
            is_text: true,
        };
        assert!(!is_detected_markdown(&wrong_label));
    }
}
