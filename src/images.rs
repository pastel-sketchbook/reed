//! Inline image support via the Kitty graphics protocol.
//!
//! Since neither termimad nor libghostty-vt expose image data, we handle
//! images outside both pipelines:
//!
//! 1. **Pre-process**: scan markdown for `![alt](path)` references, replace
//!    them with blank placeholder lines so the VT terminal reserves vertical
//!    space.
//! 2. **Draw phase**: emit Kitty graphics protocol escape sequences directly
//!    to stdout at the screen positions corresponding to each placeholder.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::GenericImageView;
use regex::Regex;
use tracing::{debug, warn};

/// Maximum base64 bytes per Kitty protocol chunk.
const CHUNK_SIZE: usize = 4096;

/// A parsed image reference from the markdown source.
#[derive(Debug, Clone)]
pub struct ImageRef {
    /// The alt text (used as fallback display).
    pub alt: String,
    /// The resolved absolute path to the image file.
    pub path: PathBuf,
    /// The line index in the *original* markdown where `![alt](path)` appeared.
    pub source_line: usize,
}

/// A positioned image ready for rendering.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ImagePlacement {
    /// PNG-encoded bytes of the (possibly resized) image.
    pub png_data: Vec<u8>,
    /// The VT row index (0-based, relative to content start) where the
    /// placeholder begins. This corresponds to the first blank line we
    /// inserted in place of the `![alt](path)` reference.
    pub content_row: usize,
    /// Display width in terminal columns.
    pub cols: u16,
    /// Display height in terminal rows.
    pub rows: u16,
    /// Alt text for fallback.
    pub alt: String,
}

/// Get the terminal's cell dimensions in pixels by querying the ioctl.
/// Returns (cell_width_px, cell_height_px) or a sensible default.
pub fn cell_size_px() -> (u16, u16) {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        #[repr(C)]
        struct Winsize {
            ws_row: u16,
            ws_col: u16,
            ws_xpixel: u16,
            ws_ypixel: u16,
        }
        let mut ws = MaybeUninit::<Winsize>::uninit();
        // TIOCGWINSZ = 0x5413 on Linux, 0x40087468 on macOS
        #[cfg(target_os = "macos")]
        const TIOCGWINSZ: u64 = 0x40087468;
        #[cfg(target_os = "linux")]
        const TIOCGWINSZ: u64 = 0x5413;
        let ret = unsafe { libc_ioctl(libc_stdout(), TIOCGWINSZ, ws.as_mut_ptr()) };
        if ret == 0 {
            let ws = unsafe { ws.assume_init() };
            if ws.ws_xpixel > 0 && ws.ws_ypixel > 0 && ws.ws_col > 0 && ws.ws_row > 0 {
                return (ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row);
            }
        }
    }
    // Fallback: assume 8x16 cell size (common default).
    (8, 16)
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "ioctl"]
    fn libc_ioctl(fd: i32, request: u64, ...) -> i32;
}

#[cfg(unix)]
fn libc_stdout() -> i32 {
    1 // STDOUT_FILENO
}

// ── Markdown scanning ─────────────────────────────────────────────

/// Extract all `![alt](path)` references from markdown source.
///
/// Only matches images that appear on their own line (possibly with leading
/// whitespace). Inline images mixed with text are ignored for now.
pub fn extract_images(markdown: &str, base_dir: &Path) -> Vec<ImageRef> {
    let re = Regex::new(r"(?m)^\s*!\[([^\]]*)\]\(([^)]+)\)\s*$").expect("valid regex");
    let mut refs = Vec::new();

    for (line_idx, line) in markdown.lines().enumerate() {
        if let Some(caps) = re.captures(line) {
            let alt = caps[1].to_string();
            let raw_path = &caps[2];

            // Resolve relative paths against the markdown file's directory.
            let path = if Path::new(raw_path).is_absolute() {
                PathBuf::from(raw_path)
            } else {
                base_dir.join(raw_path)
            };

            refs.push(ImageRef {
                alt,
                path,
                source_line: line_idx,
            });
        }
    }

    debug!(count = refs.len(), "extracted image references");
    refs
}

/// Replace image lines in markdown with placeholder blank lines.
///
/// Each `![alt](path)` line is replaced with `rows_per_image` blank lines
/// so the VT terminal reserves vertical space for the image. Returns the
/// modified markdown and a mapping from each `ImageRef` to the content row
/// where its placeholder starts.
#[allow(dead_code)]
pub fn replace_images(
    markdown: &str,
    images: &[ImageRef],
    rows_per_image: u16,
) -> (String, Vec<usize>) {
    if images.is_empty() {
        return (markdown.to_string(), Vec::new());
    }

    // Build a set of source lines that contain images.
    let image_lines: std::collections::HashSet<usize> =
        images.iter().map(|img| img.source_line).collect();

    let lines: Vec<&str> = markdown.lines().collect();
    let mut output = String::with_capacity(markdown.len());
    let mut content_rows = Vec::new();

    // Track the output line count so we know where each placeholder starts.
    let mut output_line_count: usize = 0;

    for (idx, &line) in lines.iter().enumerate() {
        if image_lines.contains(&idx) {
            // Record the output line where this image's placeholder starts.
            content_rows.push(output_line_count);

            // Insert blank placeholder lines.
            for _ in 0..rows_per_image {
                output.push('\n');
                output_line_count += 1;
            }
        } else {
            output.push_str(line);
            output.push('\n');
            output_line_count += 1;
        }
    }

    // Preserve trailing newline behavior.
    if !markdown.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }

    (output, content_rows)
}

// ── Image loading ─────────────────────────────────────────────────

/// Load an image from disk, resize it to fit within `max_cols` terminal
/// columns (preserving aspect ratio), and return PNG-encoded bytes along
/// with the display dimensions in terminal cells.
///
/// Returns `None` if the image cannot be loaded.
pub fn load_image(
    path: &Path,
    max_cols: u16,
    cell_w: u16,
    cell_h: u16,
) -> Option<(Vec<u8>, u16, u16)> {
    let img = match image::open(path) {
        Ok(img) => img,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to load image");
            return None;
        }
    };

    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return None;
    }

    // Maximum pixel width = max_cols * cell_width_px.
    let max_px_w = max_cols as u32 * cell_w as u32;

    // Scale down if wider than terminal, preserving aspect ratio.
    let (target_w, target_h) = if orig_w > max_px_w {
        let scale = max_px_w as f64 / orig_w as f64;
        let h = (orig_h as f64 * scale).round() as u32;
        (max_px_w, h.max(1))
    } else {
        (orig_w, orig_h)
    };

    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);

    // Encode as PNG.
    let mut png_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    if let Err(e) = resized.write_to(&mut cursor, image::ImageFormat::Png) {
        warn!(path = %path.display(), error = %e, "failed to encode image as PNG");
        return None;
    }

    // Compute display dimensions in cells.
    let display_cols = (target_w as f64 / cell_w as f64).ceil() as u16;
    let display_rows = (target_h as f64 / cell_h as f64).ceil() as u16;

    Some((png_bytes, display_cols, display_rows.max(1)))
}

/// Build `ImagePlacement` entries for all extractable images.
pub fn prepare_placements(
    images: &[ImageRef],
    content_rows: &[usize],
    max_cols: u16,
    cell_w: u16,
    cell_h: u16,
) -> Vec<ImagePlacement> {
    let mut placements = Vec::new();

    for (i, img) in images.iter().enumerate() {
        let content_row = content_rows.get(i).copied().unwrap_or(0);

        if let Some((png_data, cols, rows)) = load_image(&img.path, max_cols, cell_w, cell_h) {
            placements.push(ImagePlacement {
                png_data,
                content_row,
                cols,
                rows,
                alt: img.alt.clone(),
            });
        } else {
            debug!(path = %img.path.display(), "skipping unloadable image");
        }
    }

    placements
}

// ── Kitty graphics protocol ──────────────────────────────────────

/// Write a Kitty graphics protocol image to `w` at the current cursor
/// position. The image is displayed spanning `cols`x`rows` terminal cells.
///
/// The caller is responsible for positioning the cursor before calling this.
pub fn emit_kitty_image<W: Write>(w: &mut W, png_data: &[u8], cols: u16, rows: u16) -> Result<()> {
    let encoded = BASE64.encode(png_data);
    let bytes = encoded.as_bytes();

    if bytes.is_empty() {
        return Ok(());
    }

    let mut offset = 0;
    let mut first = true;

    while offset < bytes.len() {
        let end = (offset + CHUNK_SIZE).min(bytes.len());
        let chunk = &bytes[offset..end];
        let more = if end < bytes.len() { 1 } else { 0 };

        if first {
            // First chunk carries all metadata.
            // a=T  — transmit + display
            // f=100 — PNG format
            // q=2  — suppress terminal responses
            // C=1  — do NOT move cursor after display
            // c/r  — display size in cells
            write!(
                w,
                "\x1b_Ga=T,f=100,q=2,C=1,c={},r={},m={};",
                cols, rows, more
            )?;
            first = false;
        } else {
            write!(w, "\x1b_Gm={};", more)?;
        }

        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;

        offset = end;
    }

    Ok(())
}

// ── Convenience: compute rows_per_image ───────────────────────────

/// Estimate how many terminal rows an image will occupy, given a maximum
/// column width and pixel cell dimensions. This is used to insert the
/// correct number of placeholder lines before the VT sees the markdown.
pub fn estimate_image_rows(path: &Path, max_cols: u16, cell_w: u16, cell_h: u16) -> u16 {
    // Quick dimension read without loading full pixel data.
    match image::image_dimensions(path) {
        Ok((orig_w, orig_h)) => {
            if orig_w == 0 || orig_h == 0 {
                return 1;
            }
            let max_px_w = max_cols as u32 * cell_w as u32;
            let target_h = if orig_w > max_px_w {
                let scale = max_px_w as f64 / orig_w as f64;
                (orig_h as f64 * scale).round() as u32
            } else {
                orig_h
            };
            let rows = (target_h as f64 / cell_h as f64).ceil() as u16;
            rows.max(1)
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot read image dimensions");
            1 // fallback: single row placeholder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_standalone_images() {
        let md = "# Hello\n\n![photo](images/photo.png)\n\nSome text.\n";
        let refs = extract_images(md, Path::new("/docs"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].alt, "photo");
        assert_eq!(refs[0].path, PathBuf::from("/docs/images/photo.png"));
        assert_eq!(refs[0].source_line, 2);
    }

    #[test]
    fn extract_multiple_images() {
        let md = "![a](a.png)\ntext\n![b](b.jpg)\n";
        let refs = extract_images(md, Path::new("/"));
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].source_line, 0);
        assert_eq!(refs[1].source_line, 2);
    }

    #[test]
    fn skip_inline_images() {
        let md = "Check out ![this](img.png) in the text.\n";
        let refs = extract_images(md, Path::new("/"));
        // The image is inline (not on its own line), should NOT match.
        // Actually our regex requires the image to be the only content on the line.
        // "Check out ![this](img.png) in the text." has other text, so no match.
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn replace_single_image() {
        let md = "# Title\n![photo](photo.png)\nMore text.\n";
        let refs = extract_images(md, Path::new("/"));
        let (replaced, rows) = replace_images(md, &refs, 3);

        // Line 1 (![photo](photo.png)) should be replaced with 3 blank lines.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], 1); // starts at output line 1

        let lines: Vec<&str> = replaced.lines().collect();
        // Line 0: "# Title"
        assert_eq!(lines[0], "# Title");
        // Lines 1-3: blank placeholders
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "");
        // Line 4: "More text."
        assert_eq!(lines[4], "More text.");
    }

    #[test]
    fn absolute_path_preserved() {
        let md = "![abs](/absolute/path/img.png)\n";
        let refs = extract_images(md, Path::new("/other"));
        assert_eq!(refs[0].path, PathBuf::from("/absolute/path/img.png"));
    }

    #[test]
    fn kitty_encoding_single_chunk() {
        // Small payload that fits in one chunk.
        let data = vec![0u8; 10]; // 10 bytes → ~16 base64 chars
        let mut buf = Vec::new();
        emit_kitty_image(&mut buf, &data, 5, 3).unwrap();

        let output = String::from_utf8(buf).unwrap();
        // Should contain header with m=0 (single chunk).
        assert!(output.contains("a=T,f=100,q=2,C=1,c=5,r=3,m=0;"));
        // Should end with ST.
        assert!(output.ends_with("\x1b\\"));
    }

    #[test]
    fn kitty_encoding_multi_chunk() {
        // Large payload that requires chunking.
        let data = vec![0u8; 5000]; // > 4096 base64 chars after encoding
        let mut buf = Vec::new();
        emit_kitty_image(&mut buf, &data, 40, 20).unwrap();

        let output = String::from_utf8(buf).unwrap();
        // First chunk should have m=1.
        assert!(output.contains("m=1;"));
        // Last chunk should have m=0.
        assert!(output.contains("\x1b_Gm=0;"));
    }
}
