use std::fmt::Write as _;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
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
    /// Toggle zen mode (hide header/footer for full-screen content).
    ToggleZen,
    /// Switch to the next buffer in the ring (Ctrl-n).
    BufferNext,
    /// Switch to the previous buffer in the ring (Ctrl-p).
    BufferPrev,
    /// Scroll right (horizontal panning).
    ScrollRight,
    /// Scroll left (horizontal panning).
    ScrollLeft,
    /// Show keybinding help overlay.
    ShowHelp,
    /// Toggle follow/tail mode (auto-scroll on file changes).
    ToggleFollow,
    /// Set a mark at the current scroll position (user pressed `m`).
    SetMark(char),
    /// Jump to a previously set mark (user pressed `'`).
    JumpToMark(char),
    /// Export the current document to HTML.
    ExportHtml,
    /// Open a file selected via zmd semantic search (user pressed `S`).
    ZmdOpen(std::path::PathBuf),
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
            let level = hashes.min(6) as u8;
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

            // Help overlay
            (KeyCode::Char('?'), _) => return Ok(Action::ShowHelp),

            // Table of Contents sidebar
            (KeyCode::Tab, _) => return Ok(Action::ToggleToc),

            // Link following
            (KeyCode::Char('l'), KeyModifiers::NONE) => return Ok(Action::OpenLink),

            // Clipboard copy of code blocks
            (KeyCode::Char('c'), KeyModifiers::NONE) => return Ok(Action::CopyBlock),

            // Export to HTML
            (KeyCode::Char('e'), KeyModifiers::NONE) => return Ok(Action::ExportHtml),

            // Zen mode (full-screen content, no chrome)
            (KeyCode::Char('z'), KeyModifiers::NONE | KeyModifiers::CONTROL) => {
                return Ok(Action::ToggleZen);
            }

            // Follow/tail mode (auto-scroll on file changes)
            (KeyCode::Char('F'), KeyModifiers::SHIFT) => return Ok(Action::ToggleFollow),

            // Bookmark: set mark (m + letter)
            (KeyCode::Char('m'), KeyModifiers::NONE) => {
                if let Some(ch) = read_mark_char()? {
                    return Ok(Action::SetMark(ch));
                }
            }

            // Bookmark: jump to mark (' + letter)
            (KeyCode::Char('\''), _) => {
                if let Some(ch) = read_mark_char()? {
                    return Ok(Action::JumpToMark(ch));
                }
            }

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

            // zmd semantic search (S = search notes) — only when zmd index is available
            (KeyCode::Char('S'), KeyModifiers::SHIFT) => {
                #[allow(clippy::cast_possible_truncation)]
                let scroll_pos = term.scrollbar().map(|s| s.offset as usize).unwrap_or(0);
                if let Some(zmd_root) = detect_zmd_root() {
                    match fzf_zmd_picker(&zmd_root)? {
                        Some(path) => return Ok(Action::ZmdOpen(path)),
                        None => return Ok(Action::Redraw(scroll_pos)),
                    }
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

            // Horizontal scroll
            (KeyCode::Right, _) | (KeyCode::Char('L'), KeyModifiers::SHIFT) => {
                return Ok(Action::ScrollRight);
            }
            (KeyCode::Left, _) | (KeyCode::Char('H'), KeyModifiers::SHIFT) => {
                return Ok(Action::ScrollLeft);
            }

            _ => {}
        },

        Event::Resize(new_cols, new_rows) => {
            return Ok(Action::Resize(new_cols, new_rows));
        }

        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                term.scroll_viewport(ScrollViewport::Delta(-3));
            }
            MouseEventKind::ScrollDown => {
                term.scroll_viewport(ScrollViewport::Delta(3));
            }
            _ => {}
        },

        _ => {}
    }

    Ok(Action::Continue)
}

// ── Bookmark mark reader ─────────────────────────────────────────

/// Wait briefly for the next key press and return the character if it's `a-z`.
/// Returns `None` on timeout or non-letter key (cancels the mark operation).
fn read_mark_char() -> Result<Option<char>> {
    // Wait up to 2 seconds for the user to press a letter.
    if !event::poll(Duration::from_secs(2))? {
        return Ok(None);
    }
    if let Event::Key(key) = event::read()?
        && let KeyCode::Char(ch) = key.code
    {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            return Ok(Some(lower));
        }
    }
    Ok(None)
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
            // query length is bounded by terminal width which fits in u16
            #[allow(clippy::cast_possible_truncation)]
            cursor::MoveTo(1 + query.len().min(u16::MAX as usize - 1) as u16, row),
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
    execute!(stdout, DisableMouseCapture)?;
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
        terminal::Clear(terminal::ClearType::All),
        EnableMouseCapture
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

    // OK: constant regex patterns — panics only if the literal patterns are malformed.
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
        let mut url = cap[1].to_string();
        // Strip trailing punctuation that is likely sentence-ending, not part of the URL.
        // E.g. "https://example.com." → "https://example.com"
        while url.ends_with(['.', ',', ';', ':', '!', '?']) {
            url.pop();
        }
        if !url.is_empty() && seen_urls.insert(url.clone()) {
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
    execute!(stdout, DisableMouseCapture)?;
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
        let url = selected.trim().split('\t').next().map(ToString::to_string);
        Ok(url)
    })();

    execute!(
        stdout,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All),
        EnableMouseCapture
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
            let closing_len = trimmed
                .len()
                .saturating_sub(trimmed.trim_start_matches(fence_char).len());
            if closing_len >= fence_len && trimmed.trim_start_matches(fence_char).trim().is_empty()
            {
                // Remove trailing newline from content if present.
                if content.ends_with('\n') {
                    content.pop();
                }
                blocks.push(CodeBlock {
                    lang: std::mem::take(&mut lang),
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
                let tick_len = trimmed
                    .len()
                    .saturating_sub(trimmed.trim_start_matches(ch).len());
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
            "plain"
        } else {
            &block.lang
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
            "{i}\t[{lang_label}] ({line_count} lines) {preview_trunc}"
        );
    }

    let mut stdout = std::io::stdout();
    terminal::disable_raw_mode()?;
    execute!(stdout, DisableMouseCapture)?;
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
        terminal::Clear(terminal::ClearType::All),
        EnableMouseCapture
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

// ── zmd integration (optional — only active when zmd is installed) ───

/// Walk the current directory and its ancestors looking for `.qmd/data.db`.
/// Returns the directory containing `.qmd/` if found.
///
/// This is the same discovery logic zmd itself uses: the index lives in the
/// project root alongside the markdown collections.
pub fn detect_zmd_root() -> Option<std::path::PathBuf> {
    // Quick bail: is `zmd` even on PATH?
    if !Command::new("zmd")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }

    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".qmd").join("data.db").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// A zmd search result used for path resolution.
#[derive(Debug, Clone)]
struct ZmdResult {
    collection: String,
    path: String,
}

/// Resolve a zmd search result to a physical file path.
///
/// Runs `zmd collection list` from `zmd_root` to map collection names to
/// their local paths, then joins with the document path.
fn resolve_zmd_path(zmd_root: &std::path::Path, result: &ZmdResult) -> Option<std::path::PathBuf> {
    let output = Command::new("zmd")
        .arg("collection")
        .arg("list")
        .current_dir(zmd_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let list = String::from_utf8_lossy(&output.stdout);
    // Each line: "  name: path" (with leading whitespace).
    for line in list.lines() {
        let line = line.trim();
        if let Some((name, coll_path)) = line.split_once(':') {
            let name = name.trim();
            let coll_path = coll_path.trim();
            if name == result.collection {
                let full = zmd_root.join(coll_path).join(&result.path);
                if full.exists() {
                    return Some(full);
                }
            }
        }
    }
    None
}

/// Fetch document content via `zmd get` for a `zmd://` URI.
///
/// Returns the markdown content string, or `None` if the command fails.
pub fn zmd_get_content(zmd_root: &std::path::Path, uri: &str) -> Option<String> {
    let output = Command::new("zmd")
        .arg("get")
        .arg(uri)
        .current_dir(zmd_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let content = String::from_utf8_lossy(&output.stdout).into_owned();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Launch an fzf picker over zmd search results.
///
/// Prompts the user for a search query, runs `zmd query --json`, presents
/// results in fzf with `zmd context` as a preview, and returns the resolved
/// file path of the selected document.
///
/// Returns `None` if the user cancelled or no results were found.
fn fzf_zmd_picker(zmd_root: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
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

    let mut stdout = std::io::stdout();
    terminal::disable_raw_mode()?;
    execute!(stdout, DisableMouseCapture)?;
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    let center_row = term_rows * 30 / 100;
    execute!(stdout, cursor::MoveTo(0, center_row), cursor::Show)?;

    let result = (|| -> Result<Option<std::path::PathBuf>> {
        // Resolve our own binary for the reload command (same pattern as
        // the main fzf picker's preview/header/label commands).
        let reed_bin = std::env::current_exe().context("cannot determine reed binary path")?;
        let reed_escaped = shell_escape_str(&reed_bin.display().to_string());
        let zmd_root_str = zmd_root.display().to_string();

        // The reload command: reed itself parses zmd query JSON and
        // outputs tab-separated lines (collection/path \t title).
        let reload_cmd = format!("{reed_escaped} --zmd-reload {{q}}");

        // Preview command: use zmd get for the highlighted item.
        let context_cmd = format!(
            r#"cd {zmd_root_escaped} && zmd get "zmd://{{1}}" 2>/dev/null || echo "No preview available""#,
            zmd_root_escaped = shell_escape_str(&zmd_root_str)
        );

        let mut child = Command::new("fzf")
            .arg("--ansi")
            .arg("--no-multi")
            .arg("--disabled") // disable fzf's built-in filter; we do our own via reload
            .arg("--prompt")
            .arg("zmd> ")
            .arg("--delimiter")
            .arg("\t")
            .arg("--with-nth")
            .arg("2..") // display title only
            .arg("--preview")
            .arg(&context_cmd)
            .arg("--preview-window")
            .arg("right:50%:wrap")
            .arg("--height")
            .arg("100%")
            .arg("--layout")
            .arg("reverse")
            .arg("--border")
            .arg("rounded")
            .arg("--border-label")
            .arg(" zmd Search ")
            .arg("--color")
            .arg("bg:-1")
            .arg("--bind")
            .arg(format!("change:reload:{reload_cmd}"))
            .arg("--bind")
            .arg("start:reload:true") // start empty, type to search
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to launch fzf for zmd search")?;

        // Start with empty stdin (results come from reload).
        if let Some(stdin) = child.stdin.take() {
            drop(stdin); // EOF immediately
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Ok(None); // user cancelled
        }

        // Parse selected line: "collection/path\ttitle"
        let selected = String::from_utf8_lossy(&output.stdout);
        let selected = selected.trim();
        if selected.is_empty() {
            return Ok(None);
        }

        let zmd_ref = selected.split('\t').next().unwrap_or("");
        if zmd_ref.is_empty() {
            return Ok(None);
        }

        // Split "collection/path" into collection name and document path.
        if let Some((collection, doc_path)) = zmd_ref.split_once('/') {
            let result = ZmdResult {
                collection: collection.to_string(),
                path: doc_path.to_string(),
            };
            // Try resolving to a physical file first.
            if let Some(physical) = resolve_zmd_path(zmd_root, &result) {
                return Ok(Some(physical));
            }
            // Fallback: fetch content via `zmd get` and write to a temp file.
            let uri = format!("zmd://{zmd_ref}");
            if let Some(content) = zmd_get_content(zmd_root, &uri) {
                let tmp_dir = std::env::temp_dir();
                let safe_name = doc_path.replace('/', "_");
                let tmp_path = tmp_dir.join(format!("reed-zmd-{safe_name}"));
                std::fs::write(&tmp_path, &content)
                    .context("failed to write zmd content to temp file")?;
                return Ok(Some(tmp_path));
            }
        }

        Ok(None)
    })();

    // Restore terminal state.
    execute!(
        stdout,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All),
        EnableMouseCapture
    )?;
    terminal::enable_raw_mode()?;

    result
}

/// Escape a string for safe embedding in a shell command.
fn shell_escape_str(s: &str) -> String {
    if s.contains(' ') || s.contains('\'') || s.contains('"') || s.contains('\\') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

/// Launch a zmd search picker (public entry point for the fzf picker mode).
///
/// Wraps `fzf_zmd_picker` — the terminal state dance (disable raw mode,
/// re-enable after) is handled inside.
pub fn zmd_search_pick(zmd_root: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    fzf_zmd_picker(zmd_root)
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
        // Trailing period should be stripped (it's sentence punctuation, not part of the URL).
        assert_eq!(links[1].url, "http://foo.bar/baz");
    }

    #[test]
    fn extract_bare_urls_trailing_punctuation() {
        let md = "See https://a.com, and https://b.com; also https://c.com!\n";
        let links = extract_links(md);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].url, "https://a.com");
        assert_eq!(links[1].url, "https://b.com");
        assert_eq!(links[2].url, "https://c.com");
    }

    #[test]
    fn extract_bare_urls_preserves_path_dots() {
        // Dots that are part of the URL path should NOT be stripped.
        let md = "Download from https://example.com/v1.2.3/file.tar.gz here.\n";
        let links = extract_links(md);
        assert_eq!(links.len(), 1);
        // The trailing period after "gz" is sentence punctuation, stripped.
        // But ".gz" itself is preserved because the period after "gz" is
        // a trailing sentence period, not the one inside the filename.
        assert_eq!(links[0].url, "https://example.com/v1.2.3/file.tar.gz");
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
