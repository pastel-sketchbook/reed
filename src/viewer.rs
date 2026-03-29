use std::fmt::Write as FmtWrite;
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
use crate::highlight;
use crate::images::{self, ImagePlacement};
use crate::input;
use crate::mermaid;
use crate::theme::{self, MIN_TERM_HEIGHT, MIN_TERM_WIDTH, Theme};

/// Horizontal padding (spaces) on each side of header, content, and footer.
const SIDE_PAD: u16 = 2;

/// Return the ANSI escape to set the background color, or empty string for
/// `Color::Reset` (transparent / terminal default).
fn ansi_bg(color: Color) -> String {
    match color {
        Color::Rgb { r, g, b } => format!("\x1b[48;2;{r};{g};{b}m"),
        _ => String::new(),
    }
}

/// ANSI escape: clear from cursor to end of line (fills with current bg).
const ANSI_CLEAR_EOL: &str = "\x1b[K";
/// ANSI escape: reset all attributes.
const ANSI_RESET: &str = "\x1b[0m";
/// ANSI escape: bold on.
const ANSI_BOLD: &str = "\x1b[1m";
/// ANSI escape: bold off (normal intensity).
const ANSI_NORMAL: &str = "\x1b[22m";

/// Return the ANSI escape to set the foreground color, or empty string for
/// `Color::Reset` (terminal default).
fn ansi_fg(color: Color) -> String {
    match color {
        Color::Rgb { r, g, b } => format!("\x1b[38;2;{r};{g};{b}m"),
        _ => String::new(),
    }
}

/// Build the ANSI-styled header line for the `fzf` picker, showing keyboard
/// shortcuts and the current theme name. Used by `--header` / `transform-header`.
pub fn fzf_header_line(theme: &Theme) -> String {
    let fg = ansi_fg(theme.fg);
    let accent = ansi_fg(theme.accent);
    let muted = ansi_fg(theme.muted);
    let heading = ansi_fg(theme.heading);
    let sep = "\u{2502}"; // │

    format!(
        "{accent}{ANSI_BOLD}^n/^b{ANSI_NORMAL} {fg}Theme {muted}{sep}\
         {accent}{ANSI_BOLD} ^/{ANSI_NORMAL} {fg}Layout {muted}{sep}\
         {accent}{ANSI_BOLD} enter{ANSI_NORMAL} {fg}Open\
         {heading}  [{}]{ANSI_RESET}",
        theme.name,
    )
}

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
pub fn print_to_stdout(markdown: &str) {
    let skin = MadSkin::default();
    let width = terminal::size().map(|(c, _)| usize::from(c)).unwrap_or(80);
    let joined = join_paragraphs(markdown);
    let rendered = skin.text(&joined, Some(width));
    print!("{rendered}");
}

/// Preview mode: themed ANSI output to stdout for `fzf` `--preview` and piping.
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
        .or_else(|| terminal::size().ok().map(|(c, _)| usize::from(c)))
        .unwrap_or(80);

    // Determine max output lines: FZF_PREVIEW_LINES > unlimited.
    let max_lines = std::env::var("FZF_PREVIEW_LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    let skin = theme::build_skin(theme);
    let highlighted = highlight::highlight_code_blocks(markdown, theme.bg);
    let joined = join_paragraphs(&highlighted);
    let rendered = skin.text(&joined, Some(width)).to_string();

    // Split into lines, optionally skip to start_line, optionally limit count.
    let lines: Vec<&str> = rendered.lines().collect();
    let start_offset = start_line.unwrap_or(1).saturating_sub(1);
    let iter = lines.iter().skip(start_offset);

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

/// Preview mode for non-markdown code files: syntax-highlight with `syntect`
/// and write directly to stdout, bypassing `termimad` entirely.
///
/// `lang` is the `syntect` language token (e.g. "rs", "py"). If `None` or
/// unrecognized, the raw source is printed as-is.
pub fn preview_code(
    source: &str,
    lang: Option<&str>,
    theme_name: Option<&str>,
    start_line: Option<usize>,
) -> Result<()> {
    let prefs = config::load_preferences();
    let name = theme_name.unwrap_or(&prefs.theme);
    let theme = &theme::THEMES[theme::theme_index_by_name(name)];

    // Determine max output lines: FZF_PREVIEW_LINES > unlimited.
    let max_lines = std::env::var("FZF_PREVIEW_LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    // Attempt syntax highlighting; fall back to raw source.
    let highlighted = lang
        .and_then(|l| highlight::highlight_code(source, l, theme.bg))
        .unwrap_or_else(|| source.to_string());

    // Apply theme background: set bg color, print text, clear to EOL.
    let bg = ansi_bg(theme.bg);

    let lines: Vec<&str> = highlighted.lines().collect();
    let start_offset = start_line.unwrap_or(1).saturating_sub(1);
    let iter = lines.iter().skip(start_offset);

    let mut stdout = io::stdout().lock();
    let write_line = |stdout: &mut io::StdoutLock<'_>, line: &str| -> Result<()> {
        writeln!(stdout, "{bg}{line}{ANSI_CLEAR_EOL}{ANSI_RESET}")?;
        Ok(())
    };
    match max_lines {
        Some(limit) => {
            for line in iter.take(limit) {
                write_line(&mut stdout, line)?;
            }
        }
        None => {
            for line in iter {
                write_line(&mut stdout, line)?;
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
    /// Jump to a specific line (from `fzf` heading navigation).
    GotoLine(usize),
    /// Force a full redraw (screen was dirtied by an external overlay).
    /// Carries the scroll offset to restore.
    Redraw(usize),
}

/// Run the interactive markdown viewer loop.
/// Falls back to print mode if no TTY is available.
///
/// When `code_lang` is `Some`, the content is treated as a code file: it is
/// syntax-highlighted with `syntect` and fed directly to the VT terminal,
/// bypassing `termimad`. When `None`, the standard markdown pipeline is used.
#[allow(clippy::too_many_lines)]
pub fn run(
    markdown: &str,
    max_scrollback: usize,
    initial_theme: Option<&str>,
    filename: &str,
    base_dir: &Path,
    initial_line: Option<usize>,
    code_lang: Option<&str>,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        print_to_stdout(markdown);
        return Ok(());
    }

    let (mut cols, mut rows) = terminal::size().context("no terminal available")?;
    if cols == 0 || rows == 0 {
        print_to_stdout(markdown);
        return Ok(());
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

            // --- Render content to ANSI text ---
            let (ansi_text, all_placements) = if let Some(lang) = code_lang {
                // Code file path: syntect → VT terminal (no termimad).
                let highlighted = highlight::highlight_code(markdown, lang, theme.bg)
                    .unwrap_or_else(|| markdown.to_string());
                // Apply theme bg to every line so the VT cells pick it up.
                let bg = ansi_bg(theme.bg);
                let ansi = highlighted.lines().fold(String::new(), |mut acc, line| {
                    let _ = write!(acc, "{bg}{line}{ANSI_CLEAR_EOL}{ANSI_RESET}\r\n");
                    acc
                });
                (ansi, Vec::new())
            } else {
                // Markdown path: syntect (inside fences) → termimad → VT.

                // --- Syntax-highlight fenced code blocks ---
                let highlighted_md = highlight::highlight_code_blocks(markdown, theme.bg);

                // --- Build unified replacement map ---
                let (processed_md, placements) = build_processed_markdown(
                    &highlighted_md,
                    &image_refs,
                    &mermaid_blocks,
                    &rendered_mermaids,
                    inner_cols,
                    cell_w,
                    cell_h,
                );

                let skin = theme::build_skin(theme);
                let joined = join_paragraphs(&processed_md);
                let rendered = skin
                    .text(&joined, Some(usize::from(inner_cols)))
                    .to_string();
                let ansi = rendered.replace('\n', "\r\n");
                let ansi = strip_leading_blank_lines(&ansi).to_string();
                (ansi, placements)
            };

            // Create the virtual terminal and feed rendered content.
            let mut term = Terminal::new(TerminalOptions {
                cols: inner_cols,
                rows: content_rows,
                max_scrollback,
            })
            .context("failed to create libghostty-vt terminal")?;
            term.vt_write(ansi_text.as_bytes());

            // Map headings from markdown line numbers to VT row numbers
            // so the `s` key navigates to the correct rendered position.
            let mapped_headings = map_headings_to_vt_rows(&headings, &ansi_text);

            // Apply initial scroll position (--line flag or heading jump).
            // After vt_write the viewport sits at the bottom of the content,
            // so we must first scroll to Top before applying a forward delta.
            if let Some(line) = goto_line.take() {
                use libghostty_vt::terminal::ScrollViewport;
                term.scroll_viewport(ScrollViewport::Top);
                #[allow(clippy::cast_possible_wrap)]
                let delta = line.saturating_sub(1) as isize;
                if delta > 0 {
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
                &mapped_headings,
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
                LoopExit::Redraw(scroll_pos) => {
                    // Restore scroll position after overlay dirtied the screen.
                    goto_line = Some(scroll_pos + 1); // convert 0-indexed to 1-indexed
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
            let snapshot = render_state
                .update(term)
                .context("VT render state update")?;
            let mut row_iter = row_it.update(&snapshot).context("VT row iterator update")?;
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
                    Print(" ".repeat(usize::from(SIDE_PAD))),
                )?;

                let mut col_pos: u16 = 0;
                let mut cell_iter = cell_it.update(row)?;

                while let Some(cell) = cell_iter.next() {
                    let graphemes: Vec<char> = cell.graphemes()?;
                    let style = cell.style()?;

                    let fg_rgb = cell.fg_color()?;
                    let bg_rgb = cell.bg_color()?;

                    let fg = fg_rgb.map_or(theme.fg, rgb_to_color);
                    let bg = bg_rgb.map_or(theme.bg, rgb_to_color);

                    let (foreground, background) = if style.inverse { (bg, fg) } else { (fg, bg) };

                    // Reset attributes before each cell to prevent leakage,
                    // then apply only the attributes this cell actually needs.
                    queue!(
                        stdout,
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(foreground),
                        SetBackgroundColor(background),
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
                        #[allow(clippy::cast_possible_truncation)]
                        let w = UnicodeWidthStr::width(text.as_str()) as u16;
                        queue!(stdout, Print(&text))?;
                        col_pos += w;
                    }
                }

                // Fill remaining inner area + right padding to terminal edge.
                let filled = usize::from(SIDE_PAD) + usize::from(col_pos);
                if filled < usize::from(cols) {
                    queue!(
                        stdout,
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(theme.fg),
                        SetBackgroundColor(theme.bg),
                        Print(" ".repeat(usize::from(cols) - filled)),
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
                    Print(" ".repeat(usize::from(cols))),
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
            input::Action::Redraw(pos) => return Ok(LoopExit::Redraw(pos)),
        }
    }
}

// ── Image rendering ──────────────────────────────────────────────

/// Emit Kitty graphics protocol images for all placements visible in the
/// current viewport.
///
/// Uses `Terminal::scrollbar()` to determine the scroll offset, then maps
/// each `ImagePlacement::content_row` to a screen row. Images that are
/// partially or fully off-screen are skipped.
fn emit_visible_images(
    stdout: &mut io::Stdout,
    term: &Terminal<'_, '_>,
    placements: &[ImagePlacement],
    content_rows: u16,
) -> Result<()> {
    // Determine the scroll offset: which document row is at the top of the viewport.
    let scrollbar = term.scrollbar().context("VT scrollbar query")?;
    #[allow(clippy::cast_possible_truncation)]
    let viewport_top = scrollbar.offset as usize;

    for placement in placements {
        let img_start = placement.content_row;
        let img_end = img_start + usize::from(placement.rows);

        // Check if any part of the image is visible in the viewport.
        let viewport_end = viewport_top + usize::from(content_rows);
        if img_end <= viewport_top || img_start >= viewport_end {
            continue; // entirely off-screen
        }

        // Screen row where the image starts (relative to content area).
        // Content area starts at terminal row 1 (after header).
        #[allow(clippy::cast_possible_truncation)]
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
    let pad = " ".repeat(usize::from(SIDE_PAD));
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
    let used = usize::from(SIDE_PAD)
        + UnicodeWidthStr::width(title)
        + UnicodeWidthStr::width(version)
        + UnicodeWidthStr::width(separator);
    let remaining = usize::from(cols).saturating_sub(used + usize::from(SIDE_PAD));
    let display_name = if filename.len() > remaining {
        &filename[filename.len() - remaining..]
    } else {
        filename
    };
    queue!(stdout, Print(display_name))?;

    // Fill rest of header row with background.
    let total_used = used + UnicodeWidthStr::width(display_name);
    if total_used < usize::from(cols) {
        queue!(stdout, Print(" ".repeat(usize::from(cols) - total_used)))?;
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
    let pad = " ".repeat(usize::from(SIDE_PAD));
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
    let left_len: usize = usize::from(SIDE_PAD)
        + key_hints
            .iter()
            .map(|(_, t)| UnicodeWidthStr::width(*t))
            .sum::<usize>();

    // Right side: theme name + right padding.
    let right = format!("{}{pad}", theme.name);
    let right_len = right.len();

    // Fill middle with background.
    let middle = usize::from(cols).saturating_sub(left_len + right_len);
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
        "Terminal too small: {cols}x{rows} (need {MIN_TERM_WIDTH}x{MIN_TERM_HEIGHT}). Please resize.",
    );

    // Center the message vertically and horizontally.
    let y = rows / 2;
    #[allow(clippy::cast_possible_truncation)]
    let x = (usize::from(cols).saturating_sub(msg.len()) / 2) as u16;

    queue!(
        stdout,
        cursor::MoveTo(x, y),
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

// ── ANSI / heading helpers ───────────────────────────────────────

/// Strip ANSI escape sequences (CSI, OSC, etc.) from a string, returning
/// only the visible text.  Used to match heading text in rendered output.
fn strip_ansi_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the escape sequence until its terminating character.
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() || c2 == '\\' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Map each heading to a 1-indexed VT row by searching the rendered ANSI
/// text for the heading's text content.  Headings are matched in order,
/// scanning forward from the last match to handle duplicate names correctly.
fn map_headings_to_vt_rows(headings: &[input::Heading], ansi_text: &str) -> Vec<input::Heading> {
    let lines: Vec<String> = ansi_text.split("\r\n").map(strip_ansi_codes).collect();
    let mut mapped = Vec::with_capacity(headings.len());
    let mut search_from = 0;

    for h in headings {
        let mut vt_row = search_from; // default: keep last position
        for (i, line) in lines.iter().enumerate().skip(search_from) {
            if line.contains(&h.text) {
                vt_row = i;
                search_from = i + 1;
                break;
            }
        }
        mapped.push(input::Heading {
            text: h.text.clone(),
            level: h.level,
            line: vt_row + 1, // 1-indexed for goto_line
        });
    }

    mapped
}

/// Strip leading blank lines from ANSI text.
///
/// `termimad` may emit lines that contain only ANSI escape sequences (SGR codes)
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
                }
                return &s[start..]; // first line has content, stop
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
/// `minimad` (`termimad`'s parser) splits on every `\n`, treating each source line
/// as its own paragraph. `CommonMark` instead joins consecutive non-blank lines.
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
    // The length guard above guarantees at least one character.
    let first = chars_only.chars().next().expect("guarded by len >= 3");
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

    #[test]
    fn strip_ansi_codes_removes_sgr() {
        let input = "\x1b[1;31mHello\x1b[0m world";
        assert_eq!(strip_ansi_codes(input), "Hello world");
    }

    #[test]
    fn strip_ansi_codes_plain_text() {
        assert_eq!(strip_ansi_codes("no escapes here"), "no escapes here");
    }

    #[test]
    fn map_headings_finds_correct_vt_rows() {
        // Simulate ANSI text with headings at specific rows.
        let ansi = "intro line\r\n\x1b[1mTitle\x1b[0m\r\nbody\r\n\x1b[1mSection\x1b[0m\r\nmore";
        let headings = vec![
            input::Heading {
                text: "Title".to_string(),
                level: 1,
                line: 1,
            },
            input::Heading {
                text: "Section".to_string(),
                level: 2,
                line: 5,
            },
        ];
        let mapped = map_headings_to_vt_rows(&headings, ansi);
        assert_eq!(mapped[0].line, 2); // "Title" is on row 1 (0-indexed), reported as 2 (1-indexed)
        assert_eq!(mapped[1].line, 4); // "Section" is on row 3 (0-indexed), reported as 4
    }

    #[test]
    fn map_headings_duplicate_names_scans_forward() {
        let ansi = "\x1b[1mFoo\x1b[0m\r\nbody\r\n\x1b[1mFoo\x1b[0m\r\nend";
        let headings = vec![
            input::Heading {
                text: "Foo".to_string(),
                level: 1,
                line: 1,
            },
            input::Heading {
                text: "Foo".to_string(),
                level: 1,
                line: 3,
            },
        ];
        let mapped = map_headings_to_vt_rows(&headings, ansi);
        assert_eq!(mapped[0].line, 1); // first "Foo" at row 0 → 1-indexed
        assert_eq!(mapped[1].line, 3); // second "Foo" at row 2 → 1-indexed
    }
}
