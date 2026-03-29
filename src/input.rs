use std::fmt::Write as _;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{RenderState, Terminal};
use tracing::debug;

/// Result of processing one input cycle.
pub enum Action {
    Continue,
    Quit,
    Resize(u16, u16),
    NextTheme,
    PrevTheme,
    /// Jump to a specific line (e.g. from fzf heading navigation).
    GotoLine(usize),
    /// Force a full redraw (e.g. after an overlay like fzf dirtied the screen).
    /// Carries the scroll offset to restore after repaint.
    Redraw(usize),
}

/// A heading extracted from the markdown source.
#[derive(Debug, Clone)]
pub struct Heading {
    /// The raw heading text (without `#` prefix).
    pub text: String,
    /// Heading level (1–6).
    pub level: u8,
    /// 1-indexed line number in the original markdown.
    pub line: usize,
}

/// Extract all headings from markdown source (ATX-style only: `# ...`).
pub fn extract_headings(markdown: &str) -> Vec<Heading> {
    let mut headings = Vec::new();

    for (idx, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('#') {
            // Count the heading level.
            let hashes = 1 + rest.len() - rest.trim_start_matches('#').len();
            #[allow(clippy::cast_possible_truncation)]
            let level = (hashes as u8).min(6);
            let text_part = rest.trim_start_matches('#');

            // Must have a space after the #'s (or be empty for bare `#`).
            if text_part.is_empty() || text_part.starts_with(' ') {
                let text = text_part.trim().to_string();
                if !text.is_empty() {
                    headings.push(Heading {
                        text,
                        level,
                        line: idx + 1, // 1-indexed
                    });
                }
            }
        }
    }

    headings
}

/// Poll for one input event and update terminal state accordingly.
///
/// `content_rows` is the number of visible content rows (excluding header/footer)
/// used for page-up/down calculations.
///
/// `headings` are pre-extracted from the markdown for fzf heading navigation.
#[allow(clippy::cast_possible_wrap)]
pub fn poll<'a>(
    term: &mut Terminal<'a, 'a>,
    _render: &mut RenderState<'a>,
    content_rows: u16,
    headings: &[Heading],
) -> Result<Action> {
    if !event::poll(Duration::from_millis(16))? {
        return Ok(Action::Continue);
    }

    match event::read()? {
        Event::Key(key) => match (key.code, key.modifiers) {
            // Quit
            (KeyCode::Char('q') | KeyCode::Esc, _)
            | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(Action::Quit),

            // Theme cycling
            (KeyCode::Char('t'), KeyModifiers::NONE) => return Ok(Action::NextTheme),
            (KeyCode::Char('T'), KeyModifiers::SHIFT) => return Ok(Action::PrevTheme),

            // Fuzzy heading navigation (s = sections)
            (KeyCode::Char('s'), KeyModifiers::NONE) => {
                #[allow(clippy::cast_possible_truncation)]
                let scroll_pos = term.scrollbar().map(|s| s.offset as usize).unwrap_or(0);
                match fzf_heading_picker(headings)? {
                    Some(line) => return Ok(Action::GotoLine(line)),
                    None => return Ok(Action::Redraw(scroll_pos)),
                }
            }

            // Scroll down
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                term.scroll_viewport(ScrollViewport::Delta(1));
            }

            // Scroll up
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                term.scroll_viewport(ScrollViewport::Delta(-1));
            }

            // Page down (Space also pages down, like less/man)
            (KeyCode::PageDown | KeyCode::Char(' '), _)
            | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(content_rows as isize));
            }

            // Page up
            (KeyCode::PageUp, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(-(content_rows as isize)));
            }

            // Half-page down / up
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta((content_rows / 2) as isize));
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(-((content_rows / 2) as isize)));
            }

            // Top / bottom
            (KeyCode::Char('g') | KeyCode::Home, _) => {
                term.scroll_viewport(ScrollViewport::Top);
            }
            (KeyCode::Char('G') | KeyCode::End, _) => {
                term.scroll_viewport(ScrollViewport::Bottom);
            }

            _ => {}
        },

        Event::Resize(new_cols, new_rows) => {
            return Ok(Action::Resize(new_cols, new_rows));
        }

        _ => {}
    }

    Ok(Action::Continue)
}

// ── fzf heading picker ───────────────────────────────────────────

/// Launch fzf with the heading list and return the selected heading's line number.
///
/// Stays on the alternate screen so the markdown content remains visible
/// behind fzf.  Uses `--height` + `--border` so fzf appears as a compact
/// overlay at the bottom of the terminal.
/// Returns `None` if the user cancelled (Esc / Ctrl-C) or fzf is not installed.
fn fzf_heading_picker(headings: &[Heading]) -> Result<Option<usize>> {
    if headings.is_empty() {
        return Ok(None);
    }

    // Check if fzf is available.
    if Command::new("fzf")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        debug!("fzf not found on PATH");
        return Ok(None);
    }

    // Build the input: each line is "line_number:indent heading_text".
    let mut input = String::new();
    for h in headings {
        let indent = "  ".repeat((h.level as usize).saturating_sub(1));
        let _ = writeln!(input, "{}:{indent}{}", h.line, h.text);
    }

    // Stay on alternate screen — only disable raw mode so fzf can handle
    // its own terminal input.  Position cursor near the vertical center
    // so fzf's --height overlay appears centered over the markdown content.
    let mut stdout = std::io::stdout();
    terminal::disable_raw_mode()?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    let center_row = term_rows * 30 / 100;
    execute!(stdout, cursor::MoveTo(0, center_row), cursor::Show)?;

    // Run fzf as a centered overlay.
    let result = (|| -> Result<Option<usize>> {
        let mut child = Command::new("fzf")
            .arg("--ansi")
            .arg("--no-multi")
            .arg("--prompt")
            .arg("Heading> ")
            .arg("--delimiter")
            .arg(":")
            .arg("--with-nth")
            .arg("2..") // display only the heading text (not the line number)
            .arg("--preview-window")
            .arg("hidden") // no preview pane
            .arg("--height")
            .arg("~40%") // compact overlay — shrinks to fit
            .arg("--layout")
            .arg("reverse") // prompt at top, items below
            .arg("--border")
            .arg("rounded")
            .arg("--border-label")
            .arg(" Headings ")
            .arg("--color")
            .arg("bg:-1") // transparent background — terminal default shows through
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // fzf draws its UI on stderr
            .spawn()
            .context("failed to launch fzf for heading picker")?;

        // Write headings to fzf's stdin.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
            // stdin drops here, signalling EOF to fzf.
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Ok(None); // user cancelled
        }

        // Parse the selected line: "line_number:heading_text\n"
        let selected = String::from_utf8_lossy(&output.stdout);
        let line_num = selected
            .trim()
            .split(':')
            .next()
            .and_then(|s| s.parse::<usize>().ok());

        Ok(line_num)
    })();

    // Restore raw mode + hide cursor.  Clear screen so the outer loop
    // repaints cleanly over any fzf residue.
    execute!(
        stdout,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All)
    )?;
    terminal::enable_raw_mode()?;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_atx_headings() {
        let md = "# Title\n\nSome text.\n\n## Section One\n\nBody.\n\n### Sub-section\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0].text, "Title");
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].line, 1);
        assert_eq!(headings[1].text, "Section One");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].line, 5);
        assert_eq!(headings[2].text, "Sub-section");
        assert_eq!(headings[2].level, 3);
        assert_eq!(headings[2].line, 9);
    }

    #[test]
    fn skip_non_headings() {
        let md = "No headings here.\nJust text.\n#hashtag is not a heading\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 0);
    }

    #[test]
    fn heading_with_extra_hashes() {
        let md = "###### Deep heading\n";
        let headings = extract_headings(md);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].level, 6);
    }
}
