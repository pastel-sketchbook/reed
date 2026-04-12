//! Mermaid diagram rendering support.
//!
//! Detects ` ```mermaid ` fenced code blocks in markdown, renders them to PNG
//! via either `mmdz` or `mmdc` (mermaid-cli), and returns them as image data
//! that the graphics pipeline can display inline.
//!
//! Detection order: `~/bin/mmdz` → `mmdz` on PATH → `mmdc` on PATH.
//! If neither is installed, the code block is left as-is (graceful fallback).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crossterm::style::Color;
use tracing::{debug, info, warn};

/// Which mermaid renderer is available.
#[derive(Debug, Clone)]
enum MermaidBackend {
    /// `mmdz` — native Zig renderer producing SVG/PNG.
    Mmdz(PathBuf),
    /// `mmdc` — Node.js mermaid-cli producing PNG via Puppeteer.
    Mmdc(PathBuf),
}

/// Cached result of backend detection.
static BACKEND: OnceLock<Option<MermaidBackend>> = OnceLock::new();

/// A mermaid code block extracted from markdown.
#[derive(Debug, Clone)]
pub struct MermaidBlock {
    /// The mermaid diagram source (content between the fences).
    pub source: String,
    /// The line index of the opening ` ```mermaid ` fence in the original markdown.
    pub fence_start_line: usize,
    /// The line index of the closing ` ``` ` fence in the original markdown.
    pub fence_end_line: usize,
}

/// Check whether a mermaid backend (`mmdz` or `mmdc`) is available.
///
/// Detection order: `~/bin/mmdz` → `mmdz` on PATH → `mmdc` on PATH.
/// The result is cached for the lifetime of the process.
pub fn mmdc_available() -> bool {
    BACKEND
        .get_or_init(|| {
            if let Some(b) = detect_mmdz() {
                return Some(b);
            }
            detect_mmdc()
        })
        .is_some()
}

/// Try to find `mmdz`.  Checks `~/bin/mmdz` first, then PATH.
fn detect_mmdz() -> Option<MermaidBackend> {
    // Explicit ~/bin/mmdz path.
    if let Some(home) = std::env::var_os("HOME") {
        let explicit = PathBuf::from(home).join("bin/mmdz");
        if let Some(b) = probe_mmdz(&explicit) {
            return Some(b);
        }
    }
    // Fallback: mmdz on PATH.
    if let Some(path) = which_cmd("mmdz")
        && let Some(b) = probe_mmdz(&path)
    {
        return Some(b);
    }
    None
}

/// Verify that a given `mmdz` path works by running `<path> version`.
fn probe_mmdz(path: &Path) -> Option<MermaidBackend> {
    match Command::new(path).arg("version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            info!(version = %version.trim(), path = %path.display(), "mmdz found");
            Some(MermaidBackend::Mmdz(path.to_path_buf()))
        }
        Ok(output) => {
            debug!(
                status = %output.status,
                path = %path.display(),
                "mmdz found but returned non-zero"
            );
            None
        }
        Err(_) => {
            debug!(path = %path.display(), "mmdz not found");
            None
        }
    }
}

/// Try to find `mmdc` (Node.js mermaid-cli) on PATH.
fn detect_mmdc() -> Option<MermaidBackend> {
    match Command::new("mmdc").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            info!(version = %version.trim(), "mmdc (mermaid-cli) found");
            let path = which_cmd("mmdc").unwrap_or_else(|| PathBuf::from("mmdc"));
            Some(MermaidBackend::Mmdc(path))
        }
        Ok(output) => {
            debug!(
                status = %output.status,
                "mmdc found but returned non-zero"
            );
            None
        }
        Err(_) => {
            debug!("mmdc not found on PATH");
            None
        }
    }
}

/// Resolve the full path to a command via `which`.
fn which_cmd(name: &str) -> Option<PathBuf> {
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() {
                None
            } else {
                Some(PathBuf::from(path))
            }
        })
}

/// Get the cached backend (only valid after `mmdc_available()` returns true).
fn backend() -> &'static MermaidBackend {
    BACKEND
        .get()
        .and_then(|opt| opt.as_ref())
        .expect("backend() called before mmdc_available()")
}

// ── Extraction ────────────────────────────────────────────────────

/// Extract all ` ```mermaid ` fenced code blocks from markdown source.
///
/// Returns the blocks with their source content and line ranges.
pub fn extract_mermaid_blocks(markdown: &str) -> Vec<MermaidBlock> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();

        // Match opening fence: ```mermaid or ~~~mermaid (with optional trailing text).
        if (trimmed.starts_with("```mermaid") || trimmed.starts_with("~~~mermaid"))
            && trimmed
                .strip_prefix("```mermaid")
                .or_else(|| trimmed.strip_prefix("~~~mermaid"))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        {
            let fence_char = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            let fence_start = i;
            i += 1;

            // Collect content until closing fence.
            let mut content = String::new();
            while i < lines.len() {
                let line_trimmed = lines[i].trim_start();
                if line_trimmed.starts_with(fence_char)
                    && line_trimmed
                        .strip_prefix(fence_char)
                        .is_some_and(|rest| rest.trim().is_empty())
                {
                    // Found closing fence.
                    blocks.push(MermaidBlock {
                        source: content,
                        fence_start_line: fence_start,
                        fence_end_line: i,
                    });
                    i += 1;
                    break;
                }
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i]);
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    debug!(count = blocks.len(), "extracted mermaid blocks");
    blocks
}

// ── Rendering ─────────────────────────────────────────────────────

/// Determine the **mmdc** theme to use based on background brightness.
///
/// Returns `"dark"` for dark backgrounds and `"default"` (light) for light ones.
pub fn mermaid_theme_for(bg: Color) -> &'static str {
    if is_dark_bg(bg) { "dark" } else { "default" }
}

/// Determine the **mmdz** theme to use based on background brightness.
///
/// Returns `"default"` for dark backgrounds and `"default-light"` for
/// light ones.  mmdz theme names differ from mmdc: the dark variant is
/// called "default" and the light variant is "default-light".
fn mmdz_theme_for(bg: Color) -> &'static str {
    if is_dark_bg(bg) {
        "default"
    } else {
        "default-light"
    }
}

/// Returns `true` if `bg` should be treated as a dark background.
fn is_dark_bg(bg: Color) -> bool {
    match bg {
        Color::Rgb { r, g, b } => {
            let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
            luminance < 128.0
        }
        // Color::Reset means transparent / terminal default — assume dark.
        _ => true,
    }
}

/// Convert a crossterm `Color` to a CSS hex string for mmdc's `-b` flag.
///
/// Returns a `#RRGGBB` string for RGB colors, or `"transparent"` for
/// `Color::Reset` and other non-RGB variants.
fn color_to_hex(color: Color) -> String {
    match color {
        Color::Rgb { r, g, b } => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "transparent".to_string(),
    }
}

/// Render a mermaid diagram source to PNG bytes.
///
/// Dispatches to the detected backend (`mmdz` or `mmdc`).
/// Returns `None` if no backend is available or rendering fails.
/// The `bg_color` is used to select the mermaid theme (dark/light) and,
/// for mmdc, as the diagram's background color.
pub fn render_to_png(source: &str, bg_color: Color) -> Option<Vec<u8>> {
    if !mmdc_available() {
        return None;
    }

    // Shared temp directory and content-hashed filenames.
    let bg_hex = color_to_hex(bg_color);
    let tmp_dir = std::env::temp_dir().join("reed-mermaid");
    if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
        warn!(error = %e, "failed to create mermaid temp directory");
        return None;
    }
    let hash = simple_hash(&format!("{source}{bg_hex}"));
    let input_path = tmp_dir.join(format!("{hash}.mmd"));
    let output_path = tmp_dir.join(format!("{hash}.png"));

    if let Err(e) = write_temp_file(&input_path, source) {
        warn!(error = %e, "failed to write mermaid temp file");
        return None;
    }

    let result = match backend() {
        MermaidBackend::Mmdz(path) => {
            let theme = mmdz_theme_for(bg_color);
            Command::new(path)
                .arg("render")
                .arg(&input_path)
                .arg("-o")
                .arg(&output_path)
                .arg("-t")
                .arg(theme)
                .output()
        }
        MermaidBackend::Mmdc(path) => {
            let theme = mermaid_theme_for(bg_color);
            Command::new(path)
                .arg("-i")
                .arg(&input_path)
                .arg("-o")
                .arg(&output_path)
                .arg("-t")
                .arg(theme)
                .arg("-b")
                .arg(&bg_hex)
                .arg("--scale")
                .arg("2") // 2x for crisp rendering on high-DPI
                .arg("-q") // quiet
                .output()
        }
    };

    // Clean up input file (best-effort).
    let _ = std::fs::remove_file(&input_path);

    let backend_name = match backend() {
        MermaidBackend::Mmdz(_) => "mmdz",
        MermaidBackend::Mmdc(_) => "mmdc",
    };

    match result {
        Ok(output) if output.status.success() => match std::fs::read(&output_path) {
            Ok(png_data) => {
                let _ = std::fs::remove_file(&output_path);
                debug!(
                    size = png_data.len(),
                    backend = backend_name,
                    "mermaid diagram rendered"
                );
                Some(png_data)
            }
            Err(e) => {
                warn!(error = %e, "failed to read {backend_name} output PNG");
                let _ = std::fs::remove_file(&output_path);
                None
            }
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                status = %output.status,
                stderr = %stderr.trim(),
                "{backend_name} failed to render diagram"
            );
            let _ = std::fs::remove_file(&output_path);
            None
        }
        Err(e) => {
            warn!(error = %e, "failed to execute {backend_name}");
            None
        }
    }
}

/// Simple hash for generating unique temp filenames.
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(byte));
    }
    hash
}

/// Write content to a temp file.
fn write_temp_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_single_mermaid_block() {
        let md = "# Title\n\n```mermaid\ngraph TD\n    A --> B\n```\n\nMore text.\n";
        let blocks = extract_mermaid_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "graph TD\n    A --> B");
        assert_eq!(blocks[0].fence_start_line, 2);
        assert_eq!(blocks[0].fence_end_line, 5);
    }

    #[test]
    fn extract_multiple_mermaid_blocks() {
        let md = "```mermaid\ngraph LR\n    A --> B\n```\n\ntext\n\n```mermaid\nsequenceDiagram\n    A->>B: Hello\n```\n";
        let blocks = extract_mermaid_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].fence_start_line, 0);
        assert_eq!(blocks[1].fence_start_line, 7);
    }

    #[test]
    fn skip_non_mermaid_code_blocks() {
        let md = "```rust\nfn main() {}\n```\n\n```mermaid\ngraph TD\n    A --> B\n```\n";
        let blocks = extract_mermaid_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "graph TD\n    A --> B");
    }

    #[test]
    fn tilde_fences_supported() {
        let md = "~~~mermaid\nflowchart LR\n    X --> Y\n~~~\n";
        let blocks = extract_mermaid_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "flowchart LR\n    X --> Y");
    }

    #[test]
    fn unclosed_mermaid_block_ignored() {
        let md = "```mermaid\ngraph TD\n    A --> B\n";
        let blocks = extract_mermaid_blocks(md);
        // No closing fence → block is not captured.
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn dark_theme_detection() {
        assert_eq!(mermaid_theme_for(Color::Rgb { r: 0, g: 0, b: 0 }), "dark");
        assert_eq!(
            mermaid_theme_for(Color::Rgb {
                r: 30,
                g: 30,
                b: 30
            }),
            "dark"
        );
        assert_eq!(mermaid_theme_for(Color::Reset), "dark");
    }

    #[test]
    fn light_theme_detection() {
        assert_eq!(
            mermaid_theme_for(Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }),
            "default"
        );
        assert_eq!(
            mermaid_theme_for(Color::Rgb {
                r: 253,
                g: 246,
                b: 227
            }),
            "default"
        );
    }

    #[test]
    fn simple_hash_deterministic() {
        let h1 = simple_hash("graph TD\n    A --> B");
        let h2 = simple_hash("graph TD\n    A --> B");
        assert_eq!(h1, h2);
    }

    #[test]
    fn simple_hash_different_inputs() {
        let h1 = simple_hash("graph TD");
        let h2 = simple_hash("graph LR");
        assert_ne!(h1, h2);
    }

    #[test]
    fn render_to_png_produces_valid_image() {
        if !mmdc_available() {
            eprintln!("SKIP: no mermaid backend installed");
            return;
        }
        let source = "graph TD\n    A[Start] --> B[End]";
        let png = render_to_png(
            source,
            Color::Rgb {
                r: 30,
                g: 30,
                b: 46,
            },
        );
        assert!(png.is_some(), "render_to_png should produce PNG bytes");
        let data = png.unwrap();
        // PNG magic bytes: 0x89 P N G
        assert!(data.len() > 8, "PNG should be non-trivial size");
        assert_eq!(&data[0..4], b"\x89PNG", "output should be a valid PNG");
    }

    #[test]
    fn render_to_png_dark_and_light_themes() {
        if !mmdc_available() {
            eprintln!("SKIP: no mermaid backend installed");
            return;
        }
        let source = "graph LR\n    X --> Y";
        let dark = render_to_png(source, Color::Rgb { r: 0, g: 0, b: 0 });
        let light = render_to_png(
            source,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        );
        assert!(dark.is_some(), "dark theme render should succeed");
        assert!(light.is_some(), "light theme render should succeed");
        // The two renders should produce different images (different themes + bg).
        assert_ne!(
            dark.as_ref().unwrap(),
            light.as_ref().unwrap(),
            "dark and light renders should differ (different themes)"
        );
    }

    #[test]
    fn color_to_hex_rgb() {
        assert_eq!(
            color_to_hex(Color::Rgb {
                r: 30,
                g: 30,
                b: 46
            }),
            "#1e1e2e"
        );
        assert_eq!(
            color_to_hex(Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }),
            "#ffffff"
        );
    }

    #[test]
    fn color_to_hex_reset() {
        assert_eq!(color_to_hex(Color::Reset), "transparent");
    }

    #[test]
    fn mmdz_theme_dark_bg() {
        assert_eq!(mmdz_theme_for(Color::Rgb { r: 0, g: 0, b: 0 }), "default");
        assert_eq!(
            mmdz_theme_for(Color::Rgb {
                r: 30,
                g: 30,
                b: 30
            }),
            "default"
        );
        assert_eq!(mmdz_theme_for(Color::Reset), "default");
    }

    #[test]
    fn mmdz_theme_light_bg() {
        assert_eq!(
            mmdz_theme_for(Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }),
            "default-light"
        );
    }

    #[test]
    fn is_dark_bg_consistent_with_themes() {
        // Dark backgrounds: is_dark_bg=true → mmdc "dark", mmdz "default"
        let dark = Color::Rgb { r: 0, g: 0, b: 0 };
        assert!(is_dark_bg(dark));
        assert_eq!(mermaid_theme_for(dark), "dark");
        assert_eq!(mmdz_theme_for(dark), "default");

        // Light backgrounds: is_dark_bg=false → mmdc "default", mmdz "default-light"
        let light = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        assert!(!is_dark_bg(light));
        assert_eq!(mermaid_theme_for(light), "default");
        assert_eq!(mmdz_theme_for(light), "default-light");
    }
}
