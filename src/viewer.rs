use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result};
use crossterm::{
    cursor, execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, ClearType},
};
use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use termimad::MadSkin;
use tracing::{debug, warn};
use unicode_width::UnicodeWidthStr;

use crate::config;
use crate::images::{self, ImagePlacement};
use crate::input;
use crate::mermaid;
use crate::theme::{self, MIN_TERM_HEIGHT, MIN_TERM_WIDTH, Theme};

/// Horizontal padding (spaces) on each side of header, content, and footer.
const SIDE_PAD: u16 = 2;

/// Check whether the terminal likely supports the Kitty graphics protocol.
///
/// Returns `false` for terminals known not to support it (tmux, screen, etc.)
/// so we can fall back to showing raw code blocks / alt text instead of
/// emitting Kitty escape sequences that would produce garbage.
fn kitty_graphics_supported() -> bool {
    // TERM_PROGRAM is set by many terminal emulators.
    if let Ok(prog) = std::env::var("TERM_PROGRAM") {
        let lc = prog.to_ascii_lowercase();
        if lc.contains("kitty")
            || lc.contains("wezterm")
            || lc.contains("ghostty")
            || lc.contains("konsole")
        {
            return true;
        }
    }

    // Inside tmux / screen the Kitty protocol is not forwarded.
    if let Ok(term) = std::env::var("TERM") {
        let lc = term.to_ascii_lowercase();
        if lc.starts_with("tmux") || lc.starts_with("screen") {
            debug!(TERM = %term, "Kitty graphics disabled (multiplexer detected)");
            return false;
        }
    }

    // TMUX env var is set when running inside tmux, even if TERM was overridden.
    if std::env::var_os("TMUX").is_some() {
        debug!("Kitty graphics disabled (TMUX env var present)");
        return false;
    }

    // Fallback: assume support unless we detected a known blocker above.
    // This is optimistic but avoids false negatives for lesser-known
    // Kitty-capable terminals.
    true
}

/// Print rendered markdown to stdout (non-interactive, no TTY required).
pub fn print_to_stdout(markdown: &str) -> Result<()> {
    let skin = MadSkin::default();
    let width = terminal::size().map(|(c, _)| c as usize).unwrap_or(80);
    let joined = join_paragraphs(markdown);
    let rendered = skin.text(&joined, Some(width));
    print!("{rendered}");
    Ok(())
}

/// Preview mode: themed ANSI output to stdout for fzf --preview and piping.
///
/// Respects `FZF_PREVIEW_COLUMNS` / `FZF_PREVIEW_LINES` for width/height.
/// When `start_line` is set, output begins at that 1-indexed line.
pub fn preview(markdown: &str, theme_name: Option<&str>, start_line: Option<usize>) -> Result<()> {
    // Resolve theme.
    let prefs = config::load_preferences();
    let name = theme_name.unwrap_or(&prefs.theme);
    let theme = &theme::THEMES[theme::theme_index_by_name(name)];

    // Determine output width: FZF_PREVIEW_COLUMNS > terminal width > 80.
    let width = std::env::var("FZF_PREVIEW_COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .or_else(|| terminal::size().ok().map(|(c, _)| c as usize))
        .unwrap_or(80);

    // Determine max output lines: FZF_PREVIEW_LINES > unlimited.
    let max_lines = std::env::var("FZF_PREVIEW_LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    let skin = theme::build_skin(theme);
    let joined = join_paragraphs(markdown);
    let rendered = skin.text(&joined, Some(width)).to_string();

    // Split into lines, optionally skip to start_line, optionally limit count.
    let lines: Vec<&str> = rendered.lines().collect();
    let skip = start_line.unwrap_or(1).saturating_sub(1);
    let iter = lines.iter().skip(skip);

    let mut stdout = io::stdout().lock();
    match max_lines {
        Some(limit) => {
            for line in iter.take(limit) {
                writeln!(stdout, "{line}")?;
            }
        }
        None => {
            for line in iter {
                writeln!(stdout, "{line}")?;
            }
        }
    }

    Ok(())
}

/// What caused the inner render loop to exit.
enum LoopExit {
    Quit,
    NextTheme,
    PrevTheme,
    /// Terminal was resized — must re-create VT with new dimensions.
    Resize(u16, u16),
    /// Jump to a specific line (from fzf heading navigation).
    GotoLine(usize),
}

/// Run the interactive markdown viewer loop.
/// Falls back to print mode if no TTY is available.
pub fn run(
    markdown: &str,
    max_scrollback: usize,
    initial_theme: Option<&str>,
    filename: &str,
    base_dir: &Path,
    initial_line: Option<usize>,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        return print_to_stdout(markdown);
    }

    let (mut cols, mut rows) = terminal::size().context("no terminal available")?;
    if cols == 0 || rows == 0 {
        return print_to_stdout(markdown);
    }

    // Resolve initial theme: CLI flag > saved preference > default.
    let prefs = config::load_preferences();
    let theme_name = initial_theme.unwrap_or(&prefs.theme);
    let mut theme_index = theme::theme_index_by_name(theme_name);

    // Enter raw mode / alternate screen.
    terminal::enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();

    let result = (|| -> Result<()> {
        execute!(
            stdout,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::Clear(ClearType::All)
        )?;

        // Extract image references once from the original markdown.
        // Only process images/mermaid if the terminal supports Kitty graphics.
        let has_kitty = kitty_graphics_supported();
        let image_refs = if has_kitty {
            images::extract_images(markdown, base_dir)
        } else {
            Vec::new()
        };
        let (cell_w, cell_h) = images::cell_size_px();

        // Extract mermaid blocks once from the original markdown.
        // When Kitty is unsupported, leave mermaid_blocks empty so the
        // fenced code blocks pass through to termimad as-is (fallback).
        let mermaid_blocks = if has_kitty {
            mermaid::extract_mermaid_blocks(markdown)
        } else {
            Vec::new()
        };

        // Extract headings once for fzf navigation (the `s` key).
        let headings = input::extract_headings(markdown);

        // Mutable scroll target — set by --line flag or fzf heading jump.
        // Consumed on first use, then reset to None.
        let mut goto_line = initial_line;

        loop {
            let theme = &theme::THEMES[theme_index];

            // Terminal size guard — show a helpful message if too small.
            if cols < MIN_TERM_WIDTH || rows < MIN_TERM_HEIGHT {
                render_size_warning(&mut stdout, cols, rows, theme)?;
                // Wait for resize or quit.
                match wait_for_resize_or_quit()? {
                    Some((new_cols, new_rows)) => {
                        cols = new_cols;
                        rows = new_rows;
                        continue;
                    }
                    None => break, // quit
                }
            }

            // Layout: 1 row header + content + 1 row footer.
            let content_rows = rows.saturating_sub(2).max(1);
            let inner_cols = cols.saturating_sub(2 * SIDE_PAD).max(1);

            // --- Render mermaid diagrams to PNG (theme-aware) ---
            // Each entry: (block_index, png_data). Blocks that fail to render
            // are omitted and will be shown as regular code blocks.
            let rendered_mermaids: Vec<(usize, Vec<u8>)> = mermaid_blocks
                .iter()
                .enumerate()
                .filter_map(|(i, block)| {
                    mermaid::render_to_png(&block.source, theme.bg).map(|png| (i, png))
                })
                .collect();

            // --- Build unified replacement map ---
            // A "replacement" is a line range in the original markdown that
            // should be replaced with blank placeholder rows for an image.
            // Both `![alt](path)` images and successfully-rendered mermaid
            // blocks produce replacements.
            let (processed_md, all_placements) = build_processed_markdown(
                markdown,
                &image_refs,
                &mermaid_blocks,
                &rendered_mermaids,
                inner_cols,
                cell_w,
                cell_h,
            );

            // Build themed skin and render markdown → ANSI.
            let skin = theme::build_skin(theme);
            let joined = join_paragraphs(&processed_md);
            let ansi_text = skin.text(&joined, Some(inner_cols as usize)).to_string();

            // termimad uses bare \n for line breaks, but the VT terminal
            // follows strict VT semantics where LF only moves down — it
            // does NOT return to column 0. Convert to \r\n so each line
            // starts at the left edge.
            let ansi_text = ansi_text.replace('\n', "\r\n");

            // termimad may emit a leading blank line before the first heading
            // (ANSI escape codes followed by \r\n with no printable text).
            // Strip it so content starts immediately below the header bar.
            let ansi_text = strip_leading_blank_lines(&ansi_text);

            // Create the virtual terminal and feed rendered content.
            let mut term = Terminal::new(TerminalOptions {
                cols: inner_cols,
                rows: content_rows,
                max_scrollback,
            })
            .context("failed to create libghostty-vt terminal")?;
            term.vt_write(ansi_text.as_bytes());

            // Apply initial scroll position (--line flag or heading jump).
            if let Some(line) = goto_line.take() {
                // line is 1-indexed; scroll to put that line at the top.
                let delta = line.saturating_sub(1) as isize;
                if delta > 0 {
                    use libghostty_vt::terminal::ScrollViewport;
                    term.scroll_viewport(ScrollViewport::Delta(delta));
                }
            }

            // Allocate render iterators (reused every frame).
            let mut render_state = RenderState::new().context("failed to create render state")?;
            let mut row_it = RowIterator::new().context("failed to create row iterator")?;
            let mut cell_it = CellIterator::new().context("failed to create cell iterator")?;

            execute!(stdout, terminal::Clear(ClearType::All))?;

            match run_inner_loop(
                &mut term,
                &mut render_state,
                &mut row_it,
                &mut cell_it,
                &mut stdout,
                content_rows,
                cols,
                theme,
                filename,
                &all_placements,
                &headings,
            )? {
                LoopExit::Quit => break,
                LoopExit::NextTheme => {
                    theme_index = (theme_index + 1) % theme::THEMES.len();
                    persist_theme(theme_index);
                }
                LoopExit::PrevTheme => {
                    let len = theme::THEMES.len();
                    theme_index = (theme_index + len - 1) % len;
                    persist_theme(theme_index);
                }
                LoopExit::Resize(new_cols, new_rows) => {
                    cols = new_cols;
                    rows = new_rows;
                }
                LoopExit::GotoLine(line) => {
                    goto_line = Some(line);
                }
            }
        }

        Ok(())
    })();

    // Always restore terminal, even on error.
    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
}

// ── Unified markdown pre-processing ──────────────────────────────

/// A line range in the original markdown to replace with placeholder rows.
struct Replacement {
    /// First line of the range (inclusive).
    start_line: usize,
    /// Last line of the range (inclusive).
    end_line: usize,
    /// Number of blank placeholder rows to insert.
    placeholder_rows: u16,
    /// Pre-loaded PNG data for this replacement (if available).
    png_data: Option<Vec<u8>>,
    /// Display dimensions in terminal cells.
    display_cols: u16,
    display_rows: u16,
    /// Alt text / label.
    alt: String,
}

/// Build the processed markdown with image and mermaid placeholders, and
/// return the resulting `ImagePlacement` entries for Kitty rendering.
fn build_processed_markdown(
    markdown: &str,
    image_refs: &[images::ImageRef],
    mermaid_blocks: &[mermaid::MermaidBlock],
    rendered_mermaids: &[(usize, Vec<u8>)],
    inner_cols: u16,
    cell_w: u16,
    cell_h: u16,
) -> (String, Vec<ImagePlacement>) {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut replacements = Vec::new();

    // --- Image replacements (single-line each) ---
    for img in image_refs {
        let row_count = images::estimate_image_rows(&img.path, inner_cols, cell_w, cell_h);

        // Try to load the image now.
        let loaded = images::load_image(&img.path, inner_cols, cell_w, cell_h);
        let (png_data, display_cols, display_rows) = match loaded {
            Some((data, c, r)) => (Some(data), c, r),
            None => (None, 0, row_count),
        };

        replacements.push(Replacement {
            start_line: img.source_line,
            end_line: img.source_line,
            placeholder_rows: row_count,
            png_data,
            display_cols,
            display_rows,
            alt: img.alt.clone(),
        });
    }

    // --- Mermaid replacements (multi-line each) ---
    for &(block_idx, ref png_data) in rendered_mermaids {
        let block = &mermaid_blocks[block_idx];

        // Determine display size from the rendered PNG.
        let (display_cols, display_rows, placeholder_rows) =
            match images::load_image_from_bytes(png_data, inner_cols, cell_w, cell_h) {
                Some((_, c, r)) => (c, r, r),
                None => continue, // skip if we can't determine dimensions
            };

        replacements.push(Replacement {
            start_line: block.fence_start_line,
            end_line: block.fence_end_line,
            placeholder_rows,
            png_data: Some(png_data.clone()),
            display_cols,
            display_rows,
            alt: String::from("mermaid diagram"),
        });
    }

    // Sort replacements by start_line so we process them in order.
    replacements.sort_by_key(|r| r.start_line);

    // If no replacements, return markdown unchanged.
    if replacements.is_empty() {
        return (markdown.to_string(), Vec::new());
    }

    // Build processed markdown and placements.
    let mut output = String::with_capacity(markdown.len());
    let mut placements = Vec::new();
    let mut output_line_count: usize = 0;
    let mut repl_idx = 0;
    let mut skip_until: Option<usize> = None;

    for (idx, &line) in lines.iter().enumerate() {
        // Skip lines that are part of a multi-line replacement.
        if let Some(end) = skip_until {
            if idx <= end {
                continue;
            }
            skip_until = None;
        }

        if repl_idx < replacements.len() && idx == replacements[repl_idx].start_line {
            let repl = &replacements[repl_idx];

            // Record the output line where this placeholder starts.
            let content_row = output_line_count;

            // Insert blank placeholder lines.
            for _ in 0..repl.placeholder_rows {
                output.push('\n');
                output_line_count += 1;
            }

            // Create placement if we have PNG data.
            if let Some(ref png_data) = repl.png_data {
                placements.push(ImagePlacement {
                    png_data: png_data.clone(),
                    content_row,
                    cols: repl.display_cols,
                    rows: repl.display_rows,
                    alt: repl.alt.clone(),
                });
            }

            // Skip remaining lines of multi-line replacements.
            if repl.end_line > repl.start_line {
                skip_until = Some(repl.end_line);
            }

            repl_idx += 1;
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

    (output, placements)
}

/// Persist the current theme choice to disk (best-effort).
fn persist_theme(theme_index: usize) {
    if let Err(e) = config::save_preferences(&config::Preferences {
        theme: theme::THEMES[theme_index].name.to_string(),
    }) {
        warn!(error = %e, "failed to save theme preference");
    }
}

#[expect(clippy::too_many_arguments)]
fn run_inner_loop<'a>(
    term: &mut Terminal<'a, 'a>,
    render_state: &mut RenderState<'a>,
    row_it: &mut RowIterator<'a>,
    cell_it: &mut CellIterator<'a>,
    stdout: &mut io::Stdout,
    content_rows: u16,
    cols: u16,
    theme: &Theme,
    filename: &str,
    placements: &[ImagePlacement],
    headings: &[input::Heading],
) -> Result<LoopExit> {
    loop {
        // Begin synchronized update — the terminal buffers everything until
        // the matching end marker, then renders the frame atomically.
        // This prevents flicker/blink when deleting + re-emitting images.
        queue!(stdout, terminal::BeginSynchronizedUpdate)?;

        // ── Draw header (row 0) ──────────────────────────────────
        draw_header(stdout, cols, theme, filename)?;

        // ── Draw content (rows 1 .. content_rows) ────────────────
        {
            let snapshot = render_state.update(term)?;
            let mut row_iter = row_it.update(&snapshot)?;
            let mut screen_row: u16 = 0;

            while let Some(row) = row_iter.next() {
                if screen_row >= content_rows {
                    break;
                }

                // Content starts at terminal row 1 (after header).
                queue!(stdout, cursor::MoveTo(0, screen_row + 1))?;

                // Left padding.
                queue!(
                    stdout,
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(theme.fg),
                    SetBackgroundColor(theme.bg),
                    Print(" ".repeat(SIDE_PAD as usize)),
                )?;

                let mut col_pos: u16 = 0;
                let mut cell_iter = cell_it.update(row)?;

                while let Some(cell) = cell_iter.next() {
                    let graphemes: Vec<char> = cell.graphemes()?;
                    let style = cell.style()?;

                    let fg_rgb = cell.fg_color()?;
                    let bg_rgb = cell.bg_color()?;

                    let fg = fg_rgb.map(rgb_to_color).unwrap_or(theme.fg);
                    let bg = bg_rgb.map(rgb_to_color).unwrap_or(theme.bg);

                    let (draw_fg, draw_bg) = if style.inverse { (bg, fg) } else { (fg, bg) };

                    // Reset attributes before each cell to prevent leakage,
                    // then apply only the attributes this cell actually needs.
                    queue!(
                        stdout,
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(draw_fg),
                        SetBackgroundColor(draw_bg),
                    )?;

                    if style.bold {
                        queue!(stdout, SetAttribute(Attribute::Bold))?;
                    }
                    if style.underline != libghostty_vt::style::Underline::None {
                        queue!(stdout, SetAttribute(Attribute::Underlined))?;
                    }
                    if style.italic {
                        queue!(stdout, SetAttribute(Attribute::Italic))?;
                    }

                    if graphemes.is_empty() {
                        queue!(stdout, Print(' '))?;
                        col_pos += 1;
                    } else {
                        let text: String = graphemes.into_iter().collect();
                        let w = UnicodeWidthStr::width(text.as_str()) as u16;
                        queue!(stdout, Print(&text))?;
                        col_pos += w;
                    }
                }

                // Fill remaining inner area + right padding to terminal edge.
                let filled = SIDE_PAD as usize + col_pos as usize;
                if filled < cols as usize {
                    queue!(
                        stdout,
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(theme.fg),
                        SetBackgroundColor(theme.bg),
                        Print(" ".repeat(cols as usize - filled)),
                    )?;
                }

                queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
                screen_row += 1;
            }

            // Fill any remaining content rows with theme background.
            while screen_row < content_rows {
                queue!(stdout, cursor::MoveTo(0, screen_row + 1))?;
                queue!(
                    stdout,
                    SetForegroundColor(theme.fg),
                    SetBackgroundColor(theme.bg),
                    Print(" ".repeat(cols as usize)),
                    ResetColor,
                )?;
                screen_row += 1;
            }
        }
        // snapshot dropped here — render_state is free for input::poll

        // ── Emit Kitty graphics for visible images ───────────────
        if !placements.is_empty() {
            // Delete all previously placed Kitty images to prevent ghost
            // artifacts when scrolling.  q=2 suppresses terminal responses.
            write!(stdout, "\x1b_Ga=d,q=2;\x1b\\")?;
            emit_visible_images(stdout, term, placements, content_rows)?;
        }

        // ── Draw footer (last row) ──────────────────────────────
        draw_footer(stdout, content_rows + 1, cols, theme)?;

        // End synchronized update — terminal renders the complete frame now.
        queue!(stdout, terminal::EndSynchronizedUpdate)?;
        stdout.flush()?;

        // ── Handle input ─────────────────────────────────────────
        match input::poll(term, render_state, content_rows, headings)? {
            input::Action::Continue => {}
            input::Action::Quit => return Ok(LoopExit::Quit),
            input::Action::NextTheme => return Ok(LoopExit::NextTheme),
            input::Action::PrevTheme => return Ok(LoopExit::PrevTheme),
            input::Action::Resize(new_cols, new_rows) => {
                return Ok(LoopExit::Resize(new_cols, new_rows));
            }
            input::Action::GotoLine(line) => return Ok(LoopExit::GotoLine(line)),
        }
    }
}

// ── Image rendering ──────────────────────────────────────────────

/// Emit Kitty graphics protocol images for all placements visible in the
/// current viewport.
///
/// Uses `Terminal::scrollbar()` to determine the scroll offset, then maps
/// each `ImagePlacement.content_row` to a screen row. Images that are
/// partially or fully off-screen are skipped.
fn emit_visible_images(
    stdout: &mut io::Stdout,
    term: &Terminal<'_, '_>,
    placements: &[ImagePlacement],
    content_rows: u16,
) -> Result<()> {
    // Determine the scroll offset: which document row is at the top of the viewport.
    let scrollbar = term.scrollbar()?;
    let viewport_top = scrollbar.offset as usize;

    for placement in placements {
        let img_start = placement.content_row;
        let img_end = img_start + placement.rows as usize;

        // Check if any part of the image is visible in the viewport.
        let viewport_end = viewport_top + content_rows as usize;
        if img_end <= viewport_top || img_start >= viewport_end {
            continue; // entirely off-screen
        }

        // Screen row where the image starts (relative to content area).
        // Content area starts at terminal row 1 (after header).
        let screen_row = if img_start >= viewport_top {
            (img_start - viewport_top) as u16
        } else {
            0 // image starts above viewport, show from top
        };

        // Position cursor at the image location (left padding + content area).
        // NOTE: no flush here — everything stays buffered until the single
        // frame-end flush so delete + re-emit is atomic (no blink).
        queue!(stdout, cursor::MoveTo(SIDE_PAD, screen_row + 1))?;

        // Emit the Kitty graphics protocol escape sequence.
        images::emit_kitty_image(stdout, &placement.png_data, placement.cols, placement.rows)?;
    }

    Ok(())
}

// ── Header ────────────────────────────────────────────────────────

fn draw_header(stdout: &mut io::Stdout, cols: u16, theme: &Theme, filename: &str) -> Result<()> {
    queue!(
        stdout,
        cursor::MoveTo(0, 0),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(theme.header_fg),
        SetBackgroundColor(theme.header_bg),
    )?;

    // Left: padding + "REED" title + version + separator + filename.
    let pad = " ".repeat(SIDE_PAD as usize);
    let title = "REED";
    queue!(
        stdout,
        Print(&pad),
        SetForegroundColor(theme.title),
        SetAttribute(Attribute::Bold),
        Print(title),
        SetAttribute(Attribute::NormalIntensity),
        SetForegroundColor(theme.header_fg),
    )?;

    let version = concat!(" v", env!("CARGO_PKG_VERSION"));
    queue!(
        stdout,
        SetForegroundColor(theme.muted),
        Print(version),
        SetForegroundColor(theme.header_fg),
    )?;

    let separator = "  \u{2502}  "; // │
    queue!(
        stdout,
        SetForegroundColor(theme.border),
        Print(separator),
        SetForegroundColor(theme.header_fg),
    )?;

    // Truncate filename if needed, reserving SIDE_PAD on the right.
    // Use visual width (not byte len) because separator contains multi-byte │.
    let used = SIDE_PAD as usize
        + UnicodeWidthStr::width(title)
        + UnicodeWidthStr::width(version)
        + UnicodeWidthStr::width(separator);
    let remaining = (cols as usize).saturating_sub(used + SIDE_PAD as usize);
    let display_name = if filename.len() > remaining {
        &filename[filename.len() - remaining..]
    } else {
        filename
    };
    queue!(stdout, Print(display_name))?;

    // Fill rest of header row with background.
    let total_used = used + UnicodeWidthStr::width(display_name);
    if total_used < cols as usize {
        queue!(stdout, Print(" ".repeat(cols as usize - total_used)))?;
    }

    queue!(stdout, ResetColor)?;

    Ok(())
}

// ── Footer / Status bar ───────────────────────────────────────────

fn draw_footer(stdout: &mut io::Stdout, row: u16, cols: u16, theme: &Theme) -> Result<()> {
    queue!(
        stdout,
        cursor::MoveTo(0, row),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(theme.fg),
        SetBackgroundColor(theme.bg),
    )?;

    // Left padding.
    let pad = " ".repeat(SIDE_PAD as usize);
    queue!(stdout, Print(&pad))?;

    // Left side: key hints — colorful on transparent background.
    let key_hints = build_key_hints();
    for (style, text) in &key_hints {
        match style {
            HintStyle::Key => {
                queue!(
                    stdout,
                    SetForegroundColor(theme.accent),
                    SetAttribute(Attribute::Bold),
                    Print(text),
                    SetAttribute(Attribute::NormalIntensity),
                )?;
            }
            HintStyle::Desc => {
                queue!(stdout, SetForegroundColor(theme.fg), Print(text),)?;
            }
            HintStyle::Sep => {
                queue!(stdout, SetForegroundColor(theme.muted), Print(text),)?;
            }
        }
    }

    // Use visual width (not byte len) because separators contain multi-byte │.
    let left_len: usize = SIDE_PAD as usize
        + key_hints
            .iter()
            .map(|(_, t)| UnicodeWidthStr::width(*t))
            .sum::<usize>();

    // Right side: theme name + right padding.
    let right = format!("{}{}", theme.name, &pad);
    let right_len = right.len();

    // Fill middle with background.
    let middle = (cols as usize).saturating_sub(left_len + right_len);
    queue!(
        stdout,
        SetForegroundColor(theme.fg),
        Print(" ".repeat(middle)),
    )?;

    // Theme name (right-aligned).
    queue!(
        stdout,
        SetForegroundColor(theme.heading),
        Print(&right),
        ResetColor,
    )?;

    Ok(())
}

enum HintStyle {
    Key,
    Desc,
    Sep,
}

fn build_key_hints() -> Vec<(HintStyle, &'static str)> {
    vec![
        (HintStyle::Key, "j/k "),
        (HintStyle::Desc, "Scroll "),
        (HintStyle::Sep, "\u{2502}"),
        (HintStyle::Key, " g/G "),
        (HintStyle::Desc, "Top/Bot "),
        (HintStyle::Sep, "\u{2502}"),
        (HintStyle::Key, " s "),
        (HintStyle::Desc, "Sections "),
        (HintStyle::Sep, "\u{2502}"),
        (HintStyle::Key, " t/T "),
        (HintStyle::Desc, "Theme "),
        (HintStyle::Sep, "\u{2502}"),
        (HintStyle::Key, " q "),
        (HintStyle::Desc, "Quit"),
    ]
}

// ── Size warning ──────────────────────────────────────────────────

fn render_size_warning(stdout: &mut io::Stdout, cols: u16, rows: u16, theme: &Theme) -> Result<()> {
    execute!(stdout, terminal::Clear(ClearType::All))?;

    let msg = format!(
        "Terminal too small: {}x{} (need {}x{}). Please resize.",
        cols, rows, MIN_TERM_WIDTH, MIN_TERM_HEIGHT,
    );

    // Center the message vertically and horizontally.
    let y = rows / 2;
    let x = (cols as usize).saturating_sub(msg.len()) / 2;

    queue!(
        stdout,
        cursor::MoveTo(x as u16, y),
        SetForegroundColor(theme.accent),
        Print(&msg),
        ResetColor,
    )?;

    stdout.flush()?;

    Ok(())
}

/// Block until the user resizes the terminal or presses quit.
/// Returns `Some((cols, rows))` on resize, `None` on quit.
fn wait_for_resize_or_quit() -> Result<Option<(u16, u16)>> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};

    loop {
        if let Ok(event) = event::read() {
            match event {
                Event::Resize(c, r) => return Ok(Some((c, r))),
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

fn rgb_to_color(rgb: libghostty_vt::style::RgbColor) -> Color {
    Color::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

// ── ANSI-aware blank line stripping ───────────────────────────────

/// Strip leading blank lines from ANSI text.
///
/// termimad may emit lines that contain only ANSI escape sequences (SGR codes)
/// and whitespace before `\r\n`. These show up as blank rows in the VT terminal.
/// This function strips all such leading lines so content starts immediately.
fn strip_leading_blank_lines(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;

    loop {
        let mut i = start;
        let mut found_printable = false;

        while i < bytes.len() {
            if bytes[i] == 0x1B {
                // Skip ANSI escape: ESC [ <params> <final byte>
                i += 1;
                if i < bytes.len() && bytes[i] == b'[' {
                    i += 1;
                    while i < bytes.len() && !(bytes[i] >= b'@' && bytes[i] <= b'~') {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1; // skip final byte (e.g. 'm')
                    }
                }
            } else if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                // Hit \r\n — if no printable text was found, this is a blank line.
                if !found_printable {
                    start = i + 2; // skip past this blank line, try next
                    break;
                } else {
                    return &s[start..]; // first line has content, stop
                }
            } else if bytes[i] == b' ' || bytes[i] == b'\t' {
                i += 1; // whitespace — not printable content
            } else {
                found_printable = true;
                i += 1;
            }
        }

        // Reached end of string without finding another \r\n to strip.
        if found_printable || i >= bytes.len() {
            return &s[start..];
        }
    }
}

// ── Paragraph joining ─────────────────────────────────────────────

/// Pre-process markdown to join consecutive plain-text lines into single lines.
///
/// minimad (termimad's parser) splits on every `\n`, treating each source line
/// as its own paragraph. CommonMark instead joins consecutive non-blank lines.
/// This function merges those "continuation" lines so termimad can reflow them
/// to the terminal width.
///
/// Structural lines are never joined:
/// - blank lines
/// - headings (`#`)
/// - list items (`- `, `* `, `+ `, `1. `)
/// - blockquotes (`> `)
/// - code fences (``` ` ``` or `~`)
/// - tables (`|`)
/// - horizontal rules (`---`, `***`, `___` with optional spaces)
/// - YAML frontmatter (`---` delimited block at start of file)
/// - HTML blocks (`<`)
fn join_paragraphs(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut output = String::with_capacity(markdown.len());
    let mut i = 0;
    let total = lines.len();

    // Strip optional YAML frontmatter at the very start.
    // Most markdown viewers hide frontmatter entirely.
    if total > 0 && lines[0].trim() == "---" {
        i = 1;
        while i < total {
            if lines[i].trim() == "---" || lines[i].trim() == "..." {
                i += 1;
                break;
            }
            i += 1;
        }
    }

    // Track whether we're inside a fenced code block.
    let mut in_code_fence = false;

    while i < total {
        let line = lines[i];

        // Toggle code fence state.
        if is_code_fence(line) {
            in_code_fence = !in_code_fence;
            output.push_str(line);
            output.push('\n');
            i += 1;
            continue;
        }

        // Inside code fences, pass through verbatim.
        if in_code_fence {
            output.push_str(line);
            output.push('\n');
            i += 1;
            continue;
        }

        // Structural / blank lines are never joined.
        if is_structural(line) || line.trim().is_empty() {
            output.push_str(line);
            output.push('\n');
            i += 1;
            continue;
        }

        // Plain text line — collect continuation lines and join with spaces.
        output.push_str(line);
        i += 1;

        while i < total {
            let next = lines[i];
            if next.trim().is_empty() || is_structural(next) || is_code_fence(next) {
                break;
            }
            output.push(' ');
            output.push_str(next.trim());
            i += 1;
        }

        output.push('\n');
    }

    // Preserve trailing newline if original had one.
    if markdown.ends_with('\n') && !output.ends_with('\n') {
        output.push('\n');
    }

    output
}

/// Returns `true` if a line opens or closes a fenced code block.
fn is_code_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Returns `true` if a line is a structural markdown element that should never
/// be joined with adjacent lines.
fn is_structural(line: &str) -> bool {
    let trimmed = line.trim_start();

    // Headings
    if trimmed.starts_with('#') {
        return true;
    }
    // Unordered list items
    if trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || trimmed == "-"
        || trimmed == "*"
        || trimmed == "+"
    {
        return true;
    }
    // Ordered list items (digit(s) followed by `. ` or `) `)
    if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return true;
        }
    }
    // Blockquotes
    if trimmed.starts_with('>') {
        return true;
    }
    // Tables
    if trimmed.starts_with('|') {
        return true;
    }
    // HTML blocks
    if trimmed.starts_with('<') {
        return true;
    }
    // Horizontal rules: three or more `-`, `*`, or `_` with optional spaces.
    if is_horizontal_rule(trimmed) {
        return true;
    }

    false
}

/// Check for horizontal rules: lines consisting of 3+ of the same char
/// (`-`, `*`, or `_`), optionally separated by spaces.
fn is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return false;
    }
    let chars_only: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if chars_only.len() < 3 {
        return false;
    }
    let first = chars_only.chars().next().unwrap();
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    chars_only.chars().all(|c| c == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_simple_paragraph() {
        let input = "Hello\nworld\nfoo.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "Hello world foo.\n");
    }

    #[test]
    fn preserve_blank_line_separation() {
        let input = "Para one\nline two.\n\nPara two\nline two.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "Para one line two.\n\nPara two line two.\n");
    }

    #[test]
    fn preserve_headings() {
        let input = "# Heading\nSome text\ncontinued.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "# Heading\nSome text continued.\n");
    }

    #[test]
    fn preserve_list_items() {
        let input = "- item one\n- item two\nSome text\ncontinued.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "- item one\n- item two\nSome text continued.\n");
    }

    #[test]
    fn preserve_code_fences() {
        let input = "Before.\n```\nfn main() {\n    println!(\"hi\");\n}\n```\nAfter\ntext.\n";
        let result = join_paragraphs(input);
        assert_eq!(
            result,
            "Before.\n```\nfn main() {\n    println!(\"hi\");\n}\n```\nAfter text.\n"
        );
    }

    #[test]
    fn strip_frontmatter() {
        let input = "---\ntitle: Test\n---\nHello\nworld.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "Hello world.\n");
    }

    #[test]
    fn strip_frontmatter_with_dots() {
        let input = "---\ntitle: Test\nauthor: Me\n...\nContent here.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "Content here.\n");
    }

    #[test]
    fn no_frontmatter_unchanged() {
        let input = "# Heading\nSome text.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "# Heading\nSome text.\n");
    }

    #[test]
    fn preserve_blockquotes() {
        let input = "> quote line\n> continued\nPlain text\ncontinued.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "> quote line\n> continued\nPlain text continued.\n");
    }

    #[test]
    fn preserve_tables() {
        let input = "| A | B |\n|---|---|\n| 1 | 2 |\nParagraph\ncontinued.\n";
        let result = join_paragraphs(input);
        assert_eq!(
            result,
            "| A | B |\n|---|---|\n| 1 | 2 |\nParagraph continued.\n"
        );
    }

    #[test]
    fn preserve_horizontal_rule() {
        let input = "Above.\n---\nBelow\ncontinued.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "Above.\n---\nBelow continued.\n");
    }

    #[test]
    fn ordered_list() {
        let input = "1. first\n2. second\nPlain.\n";
        let result = join_paragraphs(input);
        assert_eq!(result, "1. first\n2. second\nPlain.\n");
    }

    // ── build_processed_markdown tests ───────────────────────────

    /// Create a tiny valid 1x1 red PNG for testing.
    fn tiny_png() -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img = ImageBuffer::from_pixel(1, 1, Rgba([255u8, 0, 0, 255]));
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        buf
    }

    #[test]
    fn build_processed_md_no_replacements() {
        let md = "# Hello\n\nSome text.\n";
        let (result, placements) = build_processed_markdown(
            md,
            &[], // no images
            &[], // no mermaid blocks
            &[], // no rendered mermaids
            80,
            8,
            16,
        );
        assert_eq!(result, md);
        assert!(placements.is_empty());
    }

    #[test]
    fn build_processed_md_image_replacement() {
        // Markdown with one image line. Since the image file doesn't exist,
        // load_image returns None, so we get a placeholder with no PNG data
        // and thus no placement entry.
        let md = "# Title\n\n![photo](nonexistent.png)\n\nMore text.\n";
        let image_refs = images::extract_images(md, std::path::Path::new("/tmp"));
        assert_eq!(image_refs.len(), 1);

        let (result, placements) = build_processed_markdown(md, &image_refs, &[], &[], 80, 8, 16);

        // The image line should have been replaced with placeholder blank line(s).
        assert!(!result.contains("![photo]"));
        // No placement because the image file doesn't exist.
        assert!(placements.is_empty());
    }

    #[test]
    fn build_processed_md_mermaid_replacement() {
        let md = "# Title\n\n```mermaid\ngraph TD\n    A --> B\n```\n\nMore text.\n";
        let mermaid_blocks = mermaid::extract_mermaid_blocks(md);
        assert_eq!(mermaid_blocks.len(), 1);

        // Provide a pre-rendered PNG for block index 0.
        let png = tiny_png();
        let rendered = vec![(0usize, png)];

        let (result, placements) =
            build_processed_markdown(md, &[], &mermaid_blocks, &rendered, 80, 8, 16);

        // The mermaid fenced block should be replaced with placeholder lines.
        assert!(
            !result.contains("```mermaid"),
            "mermaid fence should be removed"
        );
        assert!(
            !result.contains("graph TD"),
            "mermaid source should be removed"
        );
        // We should have one placement for the diagram.
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].alt, "mermaid diagram");
        // The text before and after should be preserved.
        assert!(result.contains("# Title"));
        assert!(result.contains("More text."));
    }

    #[test]
    fn build_processed_md_mermaid_fallback_no_render() {
        // When no rendered mermaids are provided, the mermaid block stays as-is.
        let md = "```mermaid\ngraph TD\n    A --> B\n```\n";
        let mermaid_blocks = mermaid::extract_mermaid_blocks(md);
        assert_eq!(mermaid_blocks.len(), 1);

        let (result, placements) = build_processed_markdown(
            md,
            &[],
            &mermaid_blocks,
            &[], // no renders — fallback to code block
            80,
            8,
            16,
        );

        // Should be unchanged — mermaid source preserved as code block.
        assert_eq!(result, md);
        assert!(placements.is_empty());
    }

    #[test]
    fn build_processed_md_mixed_image_and_mermaid() {
        let md = "\
![photo](fake.png)\n\
\n\
```mermaid\n\
graph LR\n\
    X --> Y\n\
```\n\
\n\
End.\n";

        let image_refs = images::extract_images(md, std::path::Path::new("/tmp"));
        let mermaid_blocks = mermaid::extract_mermaid_blocks(md);

        let png = tiny_png();
        let rendered = vec![(0usize, png)];

        let (result, placements) =
            build_processed_markdown(md, &image_refs, &mermaid_blocks, &rendered, 80, 8, 16);

        // Image line and mermaid block should both be replaced.
        assert!(!result.contains("![photo]"));
        assert!(!result.contains("```mermaid"));
        assert!(!result.contains("graph LR"));
        // One placement for mermaid (image file doesn't exist, so no image placement).
        assert_eq!(placements.len(), 1);
        assert!(result.contains("End."));
    }
}
