//! Inline image support via the Kitty graphics protocol and Sixel fallback.
//!
//! Since neither termimad nor libghostty-vt expose image data, we handle
//! images outside both pipelines:
//!
//! 1. **Pre-process**: scan markdown for `![alt](path)` references, replace
//!    them with blank placeholder lines so the VT terminal reserves vertical
//!    space.
//! 2. **Draw phase**: emit Kitty graphics protocol escape sequences (preferred)
//!    or Sixel sequences (fallback) directly to stdout at the screen positions
//!    corresponding to each placeholder.

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

/// Which graphics protocol the terminal supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    /// Kitty graphics protocol (best quality, direct PNG).
    Kitty,
    /// Sixel graphics (widely supported fallback).
    Sixel,
    /// No graphics protocol detected — show alt text / raw code blocks.
    None,
}

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
    /// Alt text for fallback (reserved for non-Kitty terminals).
    #[allow(dead_code)]
    pub alt: String,
}

/// Get the terminal's cell dimensions in pixels by querying the ioctl.
/// Returns (`cell_width_px`, `cell_height_px`) or a sensible default.
pub fn cell_size_px() -> (u16, u16) {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        #[repr(C)]
        #[allow(clippy::struct_field_names)]
        struct Winsize {
            ws_row: u16,
            ws_col: u16,
            ws_xpixel: u16,
            ws_ypixel: u16,
        }
        let mut ws = MaybeUninit::<Winsize>::uninit();
        // TIOCGWINSZ = 0x5413 on Linux, 0x4008_7468 on macOS
        #[cfg(target_os = "macos")]
        #[allow(clippy::items_after_statements)]
        const TIOCGWINSZ: u64 = 0x4008_7468;
        #[cfg(target_os = "linux")]
        #[allow(clippy::items_after_statements)]
        const TIOCGWINSZ: u64 = 0x5413;
        // SAFETY: `libc_ioctl` is the POSIX `ioctl(2)` function. Passing
        // `TIOCGWINSZ` with a valid `Winsize` pointer is the standard way to
        // query terminal pixel dimensions. `ws` is an out-parameter written
        // by the kernel when the call succeeds (ret == 0).
        let ret = unsafe { libc_ioctl(libc_stdout(), TIOCGWINSZ, ws.as_mut_ptr()) };
        if ret == 0 {
            // SAFETY: The ioctl succeeded (ret == 0), so the kernel has
            // fully initialised the `Winsize` struct behind the pointer.
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
// SAFETY: Declares the POSIX `ioctl(2)` variadic C function. We only call it
// with `TIOCGWINSZ` and a `*mut Winsize` argument, which is the documented
// ABI for querying terminal window size.
unsafe extern "C" {
    #[link_name = "ioctl"]
    fn libc_ioctl(fd: i32, request: u64, ...) -> i32;
}

#[cfg(unix)]
fn libc_stdout() -> i32 {
    1 // STDOUT_FILENO
}

// ── Image file detection ──────────────────────────────────────────

/// Image extensions supported by the `image` crate features we compile with.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Returns `true` when `path` has a recognised image extension.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| IMAGE_EXTS.iter().any(|i| i.eq_ignore_ascii_case(ext)))
}

// ── Markdown scanning ─────────────────────────────────────────────

/// Extract all `![alt](path)` references from markdown source.
///
/// Only matches images that appear on their own line (possibly with leading
/// whitespace). Inline images mixed with text are ignored for now.
pub fn extract_images(markdown: &str, base_dir: &Path) -> Vec<ImageRef> {
    // OK: constant regex pattern — panics only if the literal pattern is malformed.
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
///
/// Note: Production code uses `build_processed_markdown()` in `viewer.rs`
/// instead. This function is retained for unit tests.
#[cfg(test)]
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
    let max_px_w = u32::from(max_cols) * u32::from(cell_w);

    // Scale down if wider than terminal, preserving aspect ratio.
    let (target_w, target_h) = if orig_w > max_px_w {
        let scale = f64::from(max_px_w) / f64::from(orig_w);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let h = (f64::from(orig_h) * scale).round() as u32;
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
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let display_cols = (f64::from(target_w) / f64::from(cell_w)).ceil() as u16;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let display_rows = (f64::from(target_h) / f64::from(cell_h)).ceil() as u16;

    Some((png_bytes, display_cols, display_rows.max(1)))
}

/// Load an image from in-memory bytes (e.g. a PNG rendered by mmdc),
/// resize it to fit within `max_cols` terminal columns and `max_rows`
/// terminal rows (preserving aspect ratio), and return re-encoded PNG
/// bytes along with display dimensions in terminal cells.
///
/// When `max_rows` is `None`, only the width constraint is applied.
///
/// Returns `None` if the bytes cannot be decoded as a valid image.
pub fn load_image_from_bytes(
    data: &[u8],
    max_cols: u16,
    max_rows: Option<u16>,
    cell_w: u16,
    cell_h: u16,
) -> Option<(Vec<u8>, u16, u16)> {
    let img = match image::load_from_memory(data) {
        Ok(img) => img,
        Err(e) => {
            warn!(error = %e, "failed to load image from bytes");
            return None;
        }
    };

    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return None;
    }

    // Maximum pixel dimensions.
    let max_px_w = u32::from(max_cols) * u32::from(cell_w);

    // Scale down if wider than terminal, preserving aspect ratio.
    let (mut target_w, mut target_h) = if orig_w > max_px_w {
        let scale = f64::from(max_px_w) / f64::from(orig_w);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let h = (f64::from(orig_h) * scale).round() as u32;
        (max_px_w, h.max(1))
    } else {
        (orig_w, orig_h)
    };

    // Additionally constrain by height when a max_rows limit is provided.
    if let Some(mr) = max_rows {
        let max_px_h = u32::from(mr) * u32::from(cell_h);
        if target_h > max_px_h {
            let scale = f64::from(max_px_h) / f64::from(target_h);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let w = (f64::from(target_w) * scale).round() as u32;
            target_w = w.max(1);
            target_h = max_px_h;
        }
    }

    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);

    // Re-encode as PNG.
    let mut png_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    if let Err(e) = resized.write_to(&mut cursor, image::ImageFormat::Png) {
        warn!(error = %e, "failed to re-encode image as PNG");
        return None;
    }

    // Compute display dimensions in cells.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let display_cols = (f64::from(target_w) / f64::from(cell_w)).ceil() as u16;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let display_rows = (f64::from(target_h) / f64::from(cell_h)).ceil() as u16;

    Some((png_bytes, display_cols, display_rows.max(1)))
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
        let more = i32::from(end < bytes.len());

        if first {
            // First chunk carries all metadata.
            // a=T  — transmit + display
            // f=100 — PNG format
            // q=2  — suppress terminal responses
            // C=1  — do NOT move cursor after display
            // c/r  — display size in cells
            write!(w, "\x1b_Ga=T,f=100,q=2,C=1,c={cols},r={rows},m={more};",)?;
            first = false;
        } else {
            write!(w, "\x1b_Gm={more};")?;
        }

        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;

        offset = end;
    }

    Ok(())
}

/// The Kitty Unicode placeholder character.
pub const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics table from the Kitty graphics protocol spec.
/// Each entry maps a number (0–255) to a Unicode combining character.
pub const ROW_COL_DIACRITICS: [char; 256] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A674}', '\u{A675}',
    '\u{A676}', '\u{A677}', '\u{A678}', '\u{A679}', '\u{A67A}', '\u{A67B}', '\u{A67C}', '\u{A67D}',
];

/// Transmit a PNG image via Kitty protocol and generate Unicode placeholder
/// text lines that will display the image inline with the text stream.
///
/// This approach makes images scroll with text naturally, solving the
/// "floating image" problem in fzf preview panes.
///
/// Returns the placeholder text lines (including ANSI color escapes) that
/// should be written to stdout in place of the image. Each line corresponds
/// to one row of the image.
pub fn emit_kitty_unicode_placeholder<W: Write>(
    w: &mut W,
    png_data: &[u8],
    image_id: u32,
    cols: u16,
    rows: u16,
) -> Result<Vec<String>> {
    // Step 1: Transmit image data (a=t, transmit only, do not display)
    let encoded = BASE64.encode(png_data);
    let bytes = encoded.as_bytes();

    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    let mut offset = 0;
    let mut first = true;

    while offset < bytes.len() {
        let end = (offset + CHUNK_SIZE).min(bytes.len());
        let chunk = &bytes[offset..end];
        let more = i32::from(end < bytes.len());

        if first {
            write!(w, "\x1b_Ga=t,f=100,q=2,i={image_id},m={more};")?;
            first = false;
        } else {
            write!(w, "\x1b_Gm={more};")?;
        }
        w.write_all(chunk)?;
        w.write_all(b"\x1b\\")?;
        offset = end;
    }

    // Step 2: Create virtual placement (U=1)
    write!(w, "\x1b_Ga=p,U=1,i={image_id},c={cols},r={rows},q=2;\x1b\\")?;

    // Step 3: Build placeholder text lines
    // Image ID is encoded as RGB: R = (id >> 16) & 0xFF, G = (id >> 8) & 0xFF, B = id & 0xFF
    let r = (image_id >> 16) & 0xFF;
    let g = (image_id >> 8) & 0xFF;
    let b = image_id & 0xFF;
    let fg_on = format!("\x1b[38;2;{r};{g};{b}m");
    let fg_off = "\x1b[39m";

    let mut lines = Vec::with_capacity(usize::from(rows));

    for row in 0..rows {
        let row_idx = usize::from(row).min(ROW_COL_DIACRITICS.len() - 1);
        let row_diacritic = ROW_COL_DIACRITICS[row_idx];

        let mut line = String::new();
        line.push_str(&fg_on);

        for col in 0..cols {
            line.push(KITTY_PLACEHOLDER);
            if col == 0 {
                // First cell: specify row diacritic (column auto-starts at 0)
                line.push(row_diacritic);
            }
            // Subsequent cells: auto-inherit row, auto-increment column
        }

        line.push_str(fg_off);
        lines.push(line);
    }

    Ok(lines)
}

/// Delete a Kitty image by ID (virtual placements + image data).
pub fn delete_kitty_image_by_id<W: Write>(w: &mut W, image_id: u32) -> Result<()> {
    write!(w, "\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\")?;
    Ok(())
}

// ── Sixel graphics protocol ──────────────────────────────────────

/// Maximum number of colors in the Sixel palette.
const SIXEL_MAX_COLORS: usize = 256;

/// Quantize an RGBA image to a palette of up to 256 colors using a simple
/// median-cut approximation. Returns the palette (RGB triples) and an
/// index buffer mapping each pixel to a palette entry.
fn quantize_to_palette(rgba: &[u8], width: u32, height: u32) -> (Vec<[u8; 3]>, Vec<u8>) {
    // Use u64 intermediate to avoid u32 overflow on large images, then
    // truncate to usize (safe: images exceeding usize on 32-bit targets
    // would fail allocation anyway).
    #[allow(clippy::cast_possible_truncation)]
    let pixel_count = (u64::from(width) * u64::from(height)) as usize;
    debug_assert_eq!(rgba.len(), pixel_count * 4);
    if rgba.len() != pixel_count * 4 {
        return (Vec::new(), Vec::new());
    }

    // Collect unique-ish colors by bucketing into a 5-5-5 color space.
    let mut histogram: std::collections::HashMap<u16, (u64, u64, u64, u64)> =
        std::collections::HashMap::new();
    for chunk in rgba.chunks_exact(4) {
        let (r, g, b, a) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        if a < 128 {
            continue; // skip transparent pixels
        }
        let key = ((u16::from(r) >> 3) << 10) | ((u16::from(g) >> 3) << 5) | (u16::from(b) >> 3);
        let entry = histogram.entry(key).or_insert((0, 0, 0, 0));
        entry.0 += u64::from(r);
        entry.1 += u64::from(g);
        entry.2 += u64::from(b);
        entry.3 += 1;
    }

    // Build palette from the most common buckets.
    let mut buckets: Vec<_> = histogram.into_iter().collect();
    buckets.sort_by(|a, b| b.1.3.cmp(&a.1.3)); // most frequent first
    buckets.truncate(SIXEL_MAX_COLORS);

    #[allow(clippy::cast_possible_truncation)] // r/g/b sums divided by count always fit in u8
    let palette: Vec<[u8; 3]> = buckets
        .iter()
        .map(|(_, (r, g, b, count))| {
            [
                (*r / *count) as u8,
                (*g / *count) as u8,
                (*b / *count) as u8,
            ]
        })
        .collect();

    // Build a lookup from bucket key to palette index.
    let mut key_to_idx: std::collections::HashMap<u16, u8> =
        std::collections::HashMap::with_capacity(buckets.len());
    for (i, (key, _)) in buckets.iter().enumerate() {
        // palette is truncated to SIXEL_MAX_COLORS (256), so index always fits in u8
        #[allow(clippy::cast_possible_truncation)]
        let idx = i as u8;
        key_to_idx.insert(*key, idx);
    }

    // Map each pixel to the nearest palette entry (by bucket key).
    let mut indices = vec![0u8; pixel_count];
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let (r, g, b, a) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        if a < 128 {
            // Transparent → index 0 (background)
            indices[i] = 0;
            continue;
        }
        let key = ((u16::from(r) >> 3) << 10) | ((u16::from(g) >> 3) << 5) | (u16::from(b) >> 3);
        indices[i] = key_to_idx.get(&key).copied().unwrap_or(0);
    }

    (palette, indices)
}

/// Emit a Sixel graphics sequence for the given image data.
///
/// The image is decoded, quantized to a 256-color palette, and emitted as
/// a Sixel escape sequence. Sixel encodes 6 rows of pixels per "sixel line".
pub fn emit_sixel_image<W: Write>(
    w: &mut W,
    png_data: &[u8],
    _cols: u16,
    _rows: u16,
) -> Result<()> {
    let img = image::load_from_memory(png_data)
        .map_err(|e| anyhow::anyhow!("sixel: failed to decode image: {e}"))?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    let raw = rgba.into_raw();

    let (palette, indices) = quantize_to_palette(&raw, width, height);
    if palette.is_empty() {
        return Ok(()); // nothing to draw
    }

    // Sixel header: DCS P1 ; P2 ; P3 q
    //   P1=0 (pixel aspect 2:1 default)
    //   P2=0 (background: device default)
    //   P3=0 (horizontal grid size default)
    write!(w, "\x1bP0;0;0q")?;

    // Emit color palette registers.
    // Format: #<index>;2;<r%>;<g%>;<b%>
    for (i, rgb) in palette.iter().enumerate() {
        let r_pct = u16::from(rgb[0]) * 100 / 255;
        let g_pct = u16::from(rgb[1]) * 100 / 255;
        let b_pct = u16::from(rgb[2]) * 100 / 255;
        write!(w, "#{i};2;{r_pct};{g_pct};{b_pct}")?;
    }

    // Emit sixel data, 6 rows at a time.
    #[allow(clippy::cast_possible_truncation)] // u32 width always fits in usize
    let w_usize = width as usize;
    for band_y in (0..height).step_by(6) {
        // For each color in the palette, emit a row of sixel characters.
        let mut any_color_emitted = false;
        for (color_idx, _) in palette.iter().enumerate() {
            // palette has at most SIXEL_MAX_COLORS (256) entries, so index fits in u8
            #[allow(clippy::cast_possible_truncation)]
            let color_idx_u8 = color_idx as u8;

            // Build sixel characters for this color across the width.
            let mut sixels = Vec::with_capacity(w_usize);
            let mut has_pixel = false;
            for x in 0..w_usize {
                let mut sixel_bits: u8 = 0;
                for bit in 0..6u32 {
                    let y = band_y + bit;
                    if y < height {
                        #[allow(clippy::cast_possible_truncation)] // u32 fits in usize
                        let idx = (y as usize) * w_usize + x;
                        if indices[idx] == color_idx_u8 {
                            sixel_bits |= 1 << bit;
                            has_pixel = true;
                        }
                    }
                }
                sixels.push(sixel_bits + 0x3f); // offset to ASCII '?'
            }

            if !has_pixel {
                continue; // skip colors not present in this band
            }

            // Select color.
            if any_color_emitted {
                // '$' — carriage return (go back to start of this sixel line)
                write!(w, "$")?;
            }
            write!(w, "#{color_idx}")?;

            // Write sixel characters, using run-length encoding.
            let mut i = 0;
            while i < sixels.len() {
                let ch = sixels[i];
                let mut run = 1usize;
                while i + run < sixels.len() && sixels[i + run] == ch {
                    run += 1;
                }
                if run >= 3 {
                    write!(w, "!{run}{}", ch as char)?;
                } else {
                    for _ in 0..run {
                        w.write_all(&[ch])?;
                    }
                }
                i += run;
            }

            any_color_emitted = true;
        }

        // '-' — graphics new line (advance to next sixel band)
        write!(w, "-")?;
    }

    // Sixel terminator: ST (String Terminator)
    write!(w, "\x1b\\")?;

    Ok(())
}

// ── Image cropping for viewport clipping ──────────────────────────

/// Crop a PNG image vertically so that only the rows visible within the
/// viewport are retained.  Returns the cropped PNG bytes, or `None` if
/// the image cannot be decoded.
///
/// * `skip_rows` — how many cell rows to remove from the top (image
///   starts above the viewport).
/// * `keep_rows` — how many cell rows to keep (the visible portion).
/// * `cell_h` — height of a single terminal cell in pixels.
///
/// Both parameters are in **terminal cell rows**, not pixels.
pub fn crop_image_vertically(
    png_data: &[u8],
    total_rows: u16,
    skip_rows: u16,
    keep_rows: u16,
    cell_h: u16,
) -> Option<Vec<u8>> {
    // Nothing to crop — fast path.
    if skip_rows == 0 && keep_rows >= total_rows {
        return Some(png_data.to_vec());
    }

    let img = image::load_from_memory(png_data).ok()?;
    let (img_w, img_h) = img.dimensions();
    if img_w == 0 || img_h == 0 {
        return None;
    }

    // Convert cell rows to pixel offsets.
    let px_skip = u32::from(skip_rows) * u32::from(cell_h);
    let px_keep = u32::from(keep_rows) * u32::from(cell_h);

    let y_start = px_skip.min(img_h);
    let y_end = (px_skip + px_keep).min(img_h);
    if y_end <= y_start {
        return None;
    }

    let cropped = img.crop_imm(0, y_start, img_w, y_end - y_start);

    // Re-encode as PNG.
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    cropped
        .write_to(&mut cursor, image::ImageFormat::Png)
        .ok()?;

    Some(buf)
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
            let max_px_w = u32::from(max_cols) * u32::from(cell_w);
            let target_h = if orig_w > max_px_w {
                let scale = f64::from(max_px_w) / f64::from(orig_w);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let h = (f64::from(orig_h) * scale).round() as u32;
                h
            } else {
                orig_h
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let rows = (f64::from(target_h) / f64::from(cell_h)).ceil() as u16;
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

    // ── Sixel tests ──────────────────────────────────────────────

    #[test]
    fn quantize_produces_palette_and_indices() {
        // 2x2 image: red, green, blue, white
        let rgba = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let (palette, indices) = quantize_to_palette(&rgba, 2, 2);
        assert!(!palette.is_empty());
        assert!(palette.len() <= SIXEL_MAX_COLORS);
        assert_eq!(indices.len(), 4);
    }

    #[test]
    fn quantize_transparent_pixels() {
        // 2x1: one opaque, one transparent
        let rgba = vec![
            255, 0, 0, 255, // opaque red
            0, 0, 0, 0, // fully transparent
        ];
        let (palette, indices) = quantize_to_palette(&rgba, 2, 1);
        assert!(!palette.is_empty());
        assert_eq!(indices.len(), 2);
        // Transparent pixel maps to index 0.
        assert_eq!(indices[1], 0);
    }

    #[test]
    fn sixel_output_has_header_and_terminator() {
        // Create a tiny 2x2 PNG in memory.
        let img = image::RgbaImage::from_fn(2, 2, |x, y| {
            if (x + y) % 2 == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let mut png_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();

        let mut buf = Vec::new();
        emit_sixel_image(&mut buf, &png_bytes, 2, 1).unwrap();

        let output = String::from_utf8(buf).unwrap();
        // Should start with DCS (Sixel header).
        assert!(
            output.starts_with("\x1bP0;0;0q"),
            "missing Sixel DCS header"
        );
        // Should end with ST (String Terminator).
        assert!(output.ends_with("\x1b\\"), "missing Sixel ST terminator");
        // Should contain at least one color definition.
        assert!(output.contains("#0;2;"), "missing palette definition");
    }

    #[test]
    fn graphics_protocol_enum_equality() {
        assert_eq!(GraphicsProtocol::Kitty, GraphicsProtocol::Kitty);
        assert_ne!(GraphicsProtocol::Kitty, GraphicsProtocol::Sixel);
        assert_ne!(GraphicsProtocol::Sixel, GraphicsProtocol::None);
    }
}
