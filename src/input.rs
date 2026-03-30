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
    /// Enter search mode (user pressed `/`).
    StartSearch,
    /// Jump to the next search match.
    NextMatch,
    /// Jump to the previous search match.
    PrevMatch,
    /// Toggle the Table of Contents sidebar.
    ToggleToc,
    /// Open link picker (user pressed `l`).
    OpenLink,
    /// Open code block picker for clipboard copy (user pressed `c`).
    CopyBlock,
    /// Switch to the next buffer in the ring (Ctrl-n).
    BufferNext,
    /// Switch to the previous buffer in the ring (Ctrl-p).
    BufferPrev,
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

            // Search
            (KeyCode::Char('/'), KeyModifiers::NONE) => return Ok(Action::StartSearch),
            (KeyCode::Char('n'), KeyModifiers::NONE) => return Ok(Action::NextMatch),
            (KeyCode::Char('N'), KeyModifiers::SHIFT) => return Ok(Action::PrevMatch),

            // Table of Contents sidebar
            (KeyCode::Tab, _) => return Ok(Action::ToggleToc),

            // Link following
            (KeyCode::Char('l'), KeyModifiers::NONE) => return Ok(Action::OpenLink),

            // Clipboard copy of code blocks
            (KeyCode::Char('c'), KeyModifiers::NONE) => return Ok(Action::CopyBlock),

            // Buffer switching (Ctrl-n / Ctrl-p)
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => return Ok(Action::BufferNext),
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => return Ok(Action::BufferPrev),

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

// ── Search prompt ────────────────────────────────────────────────

/// Show a `/` search prompt on the given terminal row and collect input.
///
/// Returns `Some(query)` on Enter, `None` on Esc/Ctrl-C.
/// The caller is responsible for repainting after this returns.
pub fn search_prompt(
    stdout: &mut std::io::Stdout,
    row: u16,
    cols: u16,
    fg: crossterm::style::Color,
    bg: crossterm::style::Color,
    accent: crossterm::style::Color,
) -> Result<Option<String>> {
    use crossterm::style::{
        Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    };

    let mut query = String::new();

    loop {
        // Draw the prompt line.
        execute!(
            stdout,
            cursor::MoveTo(0, row),
            SetForegroundColor(accent),
            SetBackgroundColor(bg),
            SetAttribute(Attribute::Bold),
            Print("/"),
            SetAttribute(Attribute::NormalIntensity),
            SetForegroundColor(fg),
            Print(&query),
        )?;
        // Clear rest of line.
        let used = 1 + query.len();
        if used < usize::from(cols) {
            execute!(stdout, Print(" ".repeat(usize::from(cols) - used)))?;
        }
        execute!(
            stdout,
            ResetColor,
            cursor::MoveTo(1 + query.len() as u16, row),
            cursor::Show
        )?;
        stdout.flush()?;

        if let Ok(ev) = event::read() {
            match ev {
                Event::Key(key) => match (key.code, key.modifiers) {
                    (KeyCode::Enter, _) => {
                        execute!(stdout, cursor::Hide)?;
                        return Ok(if query.is_empty() { None } else { Some(query) });
                    }
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        execute!(stdout, cursor::Hide)?;
                        return Ok(None);
                    }
                    (KeyCode::Backspace, _) => {
                        query.pop();
                    }
                    (KeyCode::Char(c), _) => {
                        query.push(c);
                    }
                    _ => {}
                },
                Event::Resize(c, r) => {
                    // Swallow resizes during search prompt — the outer loop
                    // will pick up the new size on the next iteration.
                    let _ = (c, r);
                }
                _ => {}
            }
        }
    }
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

// ── Link extraction & picker ─────────────────────────────────────

/// A link extracted from the markdown source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The display text (label for `[text](url)`, or the URL itself for bare URLs).
    pub text: String,
    /// The URL.
    pub url: String,
}

/// Extract all links from markdown source.
///
/// Supports:
/// - `[text](url)` inline links
/// - Bare URLs: `https://...` or `http://...`
pub fn extract_links(markdown: &str) -> Vec<Link> {
    use regex::Regex;
    use std::sync::LazyLock;

    static MD_LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());
    static BARE_URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|[\s(<])((https?://[^\s)>]+))").unwrap());

    let mut links = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    // Markdown-style links [text](url).
    for cap in MD_LINK.captures_iter(markdown) {
        let text = cap[1].to_string();
        let url = cap[2].to_string();
        if seen_urls.insert(url.clone()) {
            links.push(Link { text, url });
        }
    }

    // Bare URLs.
    for cap in BARE_URL.captures_iter(markdown) {
        let url = cap[1].to_string();
        if seen_urls.insert(url.clone()) {
            links.push(Link {
                text: url.clone(),
                url,
            });
        }
    }

    links
}

/// Launch fzf with the link list and open the selected URL.
///
/// Returns `true` if a URL was opened, `false` if cancelled.
pub fn fzf_link_picker(links: &[Link]) -> Result<bool> {
    if links.is_empty() {
        return Ok(false);
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
        return Ok(false);
    }

    // Build the input: each line is "url\ttext".
    let mut input = String::new();
    for link in links {
        let _ = writeln!(input, "{}\t{}", link.url, link.text);
    }

    let mut stdout = std::io::stdout();
    terminal::disable_raw_mode()?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    let center_row = term_rows * 30 / 100;
    execute!(stdout, cursor::MoveTo(0, center_row), cursor::Show)?;

    let result = (|| -> Result<Option<String>> {
        let mut child = Command::new("fzf")
            .arg("--ansi")
            .arg("--no-multi")
            .arg("--prompt")
            .arg("Link> ")
            .arg("--delimiter")
            .arg("\t")
            .arg("--with-nth")
            .arg("2..") // display text, not URL
            .arg("--preview-window")
            .arg("hidden")
            .arg("--height")
            .arg("~40%")
            .arg("--layout")
            .arg("reverse")
            .arg("--border")
            .arg("rounded")
            .arg("--border-label")
            .arg(" Links ")
            .arg("--color")
            .arg("bg:-1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to launch fzf for link picker")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Ok(None);
        }

        let selected = String::from_utf8_lossy(&output.stdout);
        let url = selected.trim().split('\t').next().map(|s| s.to_string());
        Ok(url)
    })();

    execute!(
        stdout,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All)
    )?;
    terminal::enable_raw_mode()?;

    match result? {
        Some(url) => {
            open_url(&url)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Open a URL with the platform-appropriate command.
fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";

    debug!(url = %url, cmd = %cmd, "opening URL");
    Command::new(cmd)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to open URL")?;
    Ok(())
}

// ── Code block extraction & clipboard copy ──────────────────────

/// A fenced code block extracted from the markdown source.
#[derive(Debug, Clone)]
pub struct CodeBlock {
    /// Language tag (may be empty).
    pub lang: String,
    /// The raw content between the fences (no fence lines themselves).
    pub content: String,
}

/// Extract all fenced code blocks from the raw markdown.
pub fn extract_code_blocks(markdown: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut in_fence = false;
    let mut fence_char = ' ';
    let mut fence_len = 0usize;
    let mut lang = String::new();
    let mut content = String::new();

    for line in markdown.lines() {
        if in_fence {
            // Check for closing fence: same char, at least same length, no other content.
            let trimmed = line.trim_start();
            let closing_len = trimmed.len() - trimmed.trim_start_matches(fence_char).len();
            if closing_len >= fence_len && trimmed.trim_start_matches(fence_char).trim().is_empty()
            {
                // Remove trailing newline from content if present.
                if content.ends_with('\n') {
                    content.pop();
                }
                blocks.push(CodeBlock {
                    lang: lang.clone(),
                    content: std::mem::take(&mut content),
                });
                in_fence = false;
            } else {
                content.push_str(line);
                content.push('\n');
            }
        } else {
            // Check for opening fence: ``` or ~~~
            let trimmed = line.trim_start();
            for ch in ['`', '~'] {
                let tick_len = trimmed.len() - trimmed.trim_start_matches(ch).len();
                if tick_len >= 3 {
                    in_fence = true;
                    fence_char = ch;
                    fence_len = tick_len;
                    lang = trimmed[tick_len..].trim().to_string();
                    content.clear();
                    break;
                }
            }
        }
    }

    blocks
}

/// Launch fzf with the code block list and copy the selected block to the clipboard.
///
/// Returns `true` if a block was copied, `false` if cancelled.
pub fn fzf_code_block_picker(blocks: &[CodeBlock]) -> Result<bool> {
    if blocks.is_empty() {
        return Ok(false);
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
        return Ok(false);
    }

    // Build the input: each line is "index\tlang: first_line_preview".
    let mut input = String::new();
    for (i, block) in blocks.iter().enumerate() {
        let lang_label = if block.lang.is_empty() {
            "plain".to_string()
        } else {
            block.lang.clone()
        };
        // First non-empty line as preview, truncated.
        let preview = block
            .content
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("(empty)");
        let preview_trunc = if preview.len() > 60 {
            format!("{}...", &preview[..57])
        } else {
            preview.to_string()
        };
        let line_count = block.content.lines().count();
        let _ = writeln!(
            input,
            "{}\t[{}] ({} lines) {}",
            i, lang_label, line_count, preview_trunc
        );
    }

    let mut stdout = std::io::stdout();
    terminal::disable_raw_mode()?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    let center_row = term_rows * 30 / 100;
    execute!(stdout, cursor::MoveTo(0, center_row), cursor::Show)?;

    let result = (|| -> Result<Option<usize>> {
        let mut child = Command::new("fzf")
            .arg("--ansi")
            .arg("--no-multi")
            .arg("--prompt")
            .arg("Copy block> ")
            .arg("--delimiter")
            .arg("\t")
            .arg("--with-nth")
            .arg("2..") // display label, not index
            .arg("--preview-window")
            .arg("hidden")
            .arg("--height")
            .arg("~40%")
            .arg("--layout")
            .arg("reverse")
            .arg("--border")
            .arg("rounded")
            .arg("--border-label")
            .arg(" Code Blocks ")
            .arg("--color")
            .arg("bg:-1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to launch fzf for code block picker")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Ok(None);
        }

        let selected = String::from_utf8_lossy(&output.stdout);
        let idx = selected
            .trim()
            .split('\t')
            .next()
            .and_then(|s| s.parse::<usize>().ok());
        Ok(idx)
    })();

    execute!(
        stdout,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All)
    )?;
    terminal::enable_raw_mode()?;

    match result? {
        Some(idx) if idx < blocks.len() => {
            copy_to_clipboard(&blocks[idx].content)?;
            debug!(block = idx, "copied code block to clipboard");
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Copy text to the system clipboard.
fn copy_to_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let (cmd, args): (&str, &[&str]) = ("pbcopy", &[]);
    #[cfg(not(target_os = "macos"))]
    let (cmd, args): (&str, &[&str]) = ("xclip", &["-selection", "clipboard"]);

    debug!(cmd = %cmd, "copying to clipboard");
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch clipboard command")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }

    let status = child.wait().context("clipboard command failed")?;
    if !status.success() {
        anyhow::bail!("clipboard command exited with status {status}");
    }

    Ok(())
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

    #[test]
    fn extract_markdown_links() {
        let md = "Check out [Rust](https://www.rust-lang.org) and [Go](https://go.dev).\n";
        let links = extract_links(md);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Rust");
        assert_eq!(links[0].url, "https://www.rust-lang.org");
        assert_eq!(links[1].text, "Go");
        assert_eq!(links[1].url, "https://go.dev");
    }

    #[test]
    fn extract_bare_urls() {
        let md = "Visit https://example.com for info.\nAlso http://foo.bar/baz.\n";
        let links = extract_links(md);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[1].url, "http://foo.bar/baz.");
    }

    #[test]
    fn extract_links_deduplicates() {
        let md = "[a](https://x.com) and [b](https://x.com) and https://x.com\n";
        let links = extract_links(md);
        // Only one entry for https://x.com
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "a");
    }

    #[test]
    fn extract_links_mixed() {
        let md = "# Title\n\n[docs](https://docs.rs)\n\nSee https://crates.io for crates.\n";
        let links = extract_links(md);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://docs.rs");
        assert_eq!(links[1].url, "https://crates.io");
    }

    // ── Code block extraction tests ─────────────────────────────

    #[test]
    fn extract_code_blocks_backtick_fence() {
        let md = "# Hello\n\n```rust\nfn main() {}\n```\n\nDone.\n";
        let blocks = extract_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "rust");
        assert_eq!(blocks[0].content, "fn main() {}");
    }

    #[test]
    fn extract_code_blocks_tilde_fence() {
        let md = "~~~python\nprint('hi')\n~~~\n";
        let blocks = extract_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "python");
        assert_eq!(blocks[0].content, "print('hi')");
    }

    #[test]
    fn extract_code_blocks_no_lang() {
        let md = "```\nsome code\nmore code\n```\n";
        let blocks = extract_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "");
        assert_eq!(blocks[0].content, "some code\nmore code");
    }

    #[test]
    fn extract_code_blocks_multiple() {
        let md = "```js\nconsole.log(1);\n```\n\nText\n\n```go\nfmt.Println()\n```\n";
        let blocks = extract_code_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lang, "js");
        assert_eq!(blocks[1].lang, "go");
    }

    #[test]
    fn extract_code_blocks_no_blocks() {
        let md = "# Title\n\nJust some text.\n";
        let blocks = extract_code_blocks(md);
        assert!(blocks.is_empty());
    }
}
