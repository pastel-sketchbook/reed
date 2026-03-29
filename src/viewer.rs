use std::io::{self, IsTerminal, Write};

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
use tracing::warn;
use unicode_width::UnicodeWidthStr;

use crate::config;
use crate::input;
use crate::theme::{self, Theme, MIN_TERM_HEIGHT, MIN_TERM_WIDTH};

/// Horizontal padding (spaces) on each side of header, content, and footer.
const SIDE_PAD: u16 = 2;

/// Print rendered markdown to stdout (non-interactive, no TTY required).
pub fn print_to_stdout(markdown: &str) -> Result<()> {
    let skin = MadSkin::default();
    let width = terminal::size().map(|(c, _)| c as usize).unwrap_or(80);
    let joined = join_paragraphs(markdown);
    let rendered = skin.text(&joined, Some(width));
    print!("{rendered}");
    Ok(())
}

/// What caused the inner render loop to exit.
enum LoopExit {
    Quit,
    NextTheme,
    PrevTheme,
    /// Terminal was resized — must re-create VT with new dimensions.
    Resize(u16, u16),
}

/// Run the interactive markdown viewer loop.
/// Falls back to print mode if no TTY is available.
pub fn run(
    markdown: &str,
    max_scrollback: usize,
    initial_theme: Option<&str>,
    filename: &str,
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

            // Build themed skin and render markdown → ANSI.
            let skin = theme::build_skin(theme);
            let joined = join_paragraphs(markdown);
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
            }
        }

        Ok(())
    })();

    // Always restore terminal, even on error.
    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
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
) -> Result<LoopExit> {
    loop {
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

        // ── Draw footer (last row) ──────────────────────────────
        draw_footer(stdout, content_rows + 1, cols, theme)?;

        stdout.flush()?;

        // ── Handle input ─────────────────────────────────────────
        match input::poll(term, render_state, content_rows)? {
            input::Action::Continue => {}
            input::Action::Quit => return Ok(LoopExit::Quit),
            input::Action::NextTheme => return Ok(LoopExit::NextTheme),
            input::Action::PrevTheme => return Ok(LoopExit::PrevTheme),
            input::Action::Resize(new_cols, new_rows) => {
                return Ok(LoopExit::Resize(new_cols, new_rows));
            }
        }
    }
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
}
