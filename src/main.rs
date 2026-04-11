mod config;
mod highlight;
mod images;
mod input;
mod mermaid;
mod theme;
mod viewer;

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Parser;

/// Terminal file viewer with syntax highlighting, powered by libghostty-vt.
///
/// When invoked with no file argument, launches fzf as an interactive file
/// picker with reed providing the preview. Select a file to open it in the
/// full interactive viewer. Pipe a file list into stdin to narrow the
/// candidates (e.g. `find . -name '*.rs' | reed`).
///
/// Use `-` as the file argument to read from stdin
/// (e.g. `curl https://example.com/README.md | reed -`).
#[derive(Parser)]
#[command(name = "reed", version, about)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// File to display. Use `-` to read from stdin.
    /// If omitted, launches fzf for interactive file picking.
    file: Option<PathBuf>,

    /// Maximum scrollback lines (default: 100 000).
    #[arg(long, default_value_t = 100_000)]
    max_scrollback: usize,

    /// Print rendered output to stdout instead of launching the interactive viewer.
    #[arg(long)]
    print: bool,

    /// Preview mode: themed ANSI output to stdout for use with fzf --preview.
    /// Respects `FZF_PREVIEW_COLUMNS` and `FZF_PREVIEW_LINES` if set.
    #[arg(long)]
    preview: bool,

    /// Initial theme (overrides saved preference). Use `t`/`T` to cycle at runtime.
    #[arg(long)]
    theme: Option<String>,

    /// Scroll to this line number on startup (1-indexed).
    #[arg(long)]
    line: Option<usize>,

    /// Cycle to the next theme, save preference, and exit.
    /// Used internally by the fzf picker for theme switching.
    #[arg(long)]
    next_theme: bool,

    /// Cycle to the previous theme, save preference, and exit.
    /// Used internally by the fzf picker for theme switching.
    #[arg(long)]
    prev_theme: bool,

    /// Open the file in an external editor (`emacs`, `nvim`, or `$EDITOR`)
    /// instead of the built-in viewer.
    #[arg(long)]
    editor: bool,

    /// Print the ANSI-styled fzf header line (shortcuts + theme name) and exit.
    /// Used internally by fzf transform-header to update the header on theme change.
    #[arg(long)]
    print_header: bool,
}

#[expect(clippy::too_many_lines)]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Theme cycling mode: update preference and exit immediately.
    if cli.next_theme || cli.prev_theme {
        return cycle_theme(cli.next_theme);
    }

    // Print fzf header line and exit (used by transform-header).
    if cli.print_header {
        let prefs = config::load_preferences();
        let theme = &theme::ALL_THEMES[theme::theme_index_by_name(config::active_theme(&prefs))];
        print!("{}", viewer::fzf_header_line(theme));
        return Ok(());
    }

    // No file argument → launch fzf picker mode.
    let Some(file) = cli.file else {
        if cli.preview || cli.print {
            bail!("--preview and --print require a file argument");
        }
        return fzf_pick_and_view(cli.theme.as_deref(), cli.max_scrollback);
    };

    // `-` as the file argument: read from stdin.
    if file.as_os_str() == "-" {
        return view_stdin(
            cli.print,
            cli.preview,
            cli.theme.as_deref(),
            cli.line,
            cli.max_scrollback,
        );
    }

    // --editor: open directly in an external editor and exit.
    if cli.editor {
        let editor =
            detect_editor().context("no editor found (install emacs/nvim or set $EDITOR)")?;
        return open_in_editor(&editor, &file);
    }

    // Document files (docx, pptx, xlsx, …): convert to markdown via pandoc
    // and render with the built-in viewer.
    if is_document_path(&file) && has_command("pandoc") {
        let markdown = pandoc_to_markdown(&file)?;
        let filename = file.display().to_string();
        let base_dir = file
            .canonicalize()
            .unwrap_or_else(|_| file.clone())
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

        if cli.print {
            viewer::print_to_stdout(&markdown);
            return Ok(());
        } else if cli.preview {
            return viewer::preview(&markdown, cli.theme.as_deref(), cli.line);
        }
        return viewer::run(
            &markdown,
            cli.max_scrollback,
            cli.theme.as_deref(),
            &filename,
            &base_dir,
            cli.line,
            None, // Rendered as markdown.
            Some(file.as_path()),
            None,
        )
        .map(|_| ());
    }

    // Binary files: images are displayed natively when the terminal
    // supports a graphics protocol; other binaries open in hexyl.
    if is_binary(&file) {
        let gfx = viewer::detect_graphics_protocol();

        // Image files → display with Kitty / Sixel when possible.
        if images::is_image_path(&file) && gfx != images::GraphicsProtocol::None {
            return if cli.print || cli.preview {
                print_image(&file, gfx)
            } else {
                display_image(&file, gfx)
            };
        }

        // Non-image binary → hexyl.
        if !has_command("hexyl") {
            bail!(
                "{} is a binary file (install hexyl for hex viewing)",
                file.display()
            );
        }
        if cli.print || cli.preview {
            let status = Command::new("hexyl")
                .arg(&file)
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .context("failed to launch hexyl")?;
            if !status.success() {
                tracing::warn!("hexyl exited with status {status}");
            }
            return Ok(());
        }
        return open_in_hexyl(&file);
    }

    let raw_content = std::fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;

    // When running as an fzf preview, clear any Kitty images left over
    // from a previously previewed image file.
    if cli.preview {
        let gfx = viewer::detect_graphics_protocol();
        if gfx == images::GraphicsProtocol::Kitty {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            write!(stdout, "\x1b_Ga=d,d=A,q=2;\x1b\\")?;
            stdout.flush()?;
        }
    }

    let is_markdown = highlight::is_markdown_path(&file);
    let code_lang = if is_markdown {
        None
    } else {
        highlight::lang_for_path(&file)
    };

    let filename = file.display().to_string();

    // Resolve the directory containing the file (for relative image paths).
    let base_dir = file
        .canonicalize()
        .unwrap_or_else(|_| file.clone())
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

    if cli.print {
        if is_markdown {
            viewer::print_to_stdout(&raw_content);
            Ok(())
        } else {
            viewer::preview_code(
                &raw_content,
                code_lang.as_deref(),
                cli.theme.as_deref(),
                None,
            )
        }
    } else if cli.preview {
        if is_markdown {
            viewer::preview(&raw_content, cli.theme.as_deref(), cli.line)
        } else {
            viewer::preview_code(
                &raw_content,
                code_lang.as_deref(),
                cli.theme.as_deref(),
                cli.line,
            )
        }
    } else {
        viewer::run(
            &raw_content,
            cli.max_scrollback,
            cli.theme.as_deref(),
            &filename,
            &base_dir,
            cli.line,
            code_lang.as_deref(),
            Some(file.as_path()),
            None, // No buffer ring in single-file mode.
        )
        .map(|_| ()) // Single file — ignore BufferNext/BufferPrev.
    }
}

// ── Theme cycling (for fzf integration) ─────────────────────────

/// Cycle to the next or previous theme, save the preference, and exit.
fn cycle_theme(forward: bool) -> Result<()> {
    let mut prefs = config::load_preferences();
    let current = theme::theme_index_by_name(config::active_theme(&prefs));
    let len = theme::ALL_THEMES.len();
    let next = if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    };
    config::set_active_theme(&mut prefs, theme::ALL_THEMES[next].name);
    config::save_preferences(&prefs).context("failed to save theme preference")?;
    Ok(())
}

// ── Stdin viewing ───────────────────────────────────────────────

/// Read all of stdin and view the content as markdown or plain text.
///
/// For the interactive viewer, `/dev/tty` must be available since stdin
/// is consumed by the piped input.
fn view_stdin(
    print: bool,
    preview: bool,
    theme: Option<&str>,
    line: Option<usize>,
    max_scrollback: usize,
) -> Result<()> {
    let mut content = String::new();
    std::io::stdin()
        .read_to_string(&mut content)
        .context("failed to read from stdin")?;

    if content.is_empty() {
        bail!("no input received on stdin");
    }

    if print {
        viewer::print_to_stdout(&content);
        return Ok(());
    }

    if preview {
        return viewer::preview(&content, theme, line);
    }

    // Interactive mode: we need to reopen the TTY for crossterm since
    // stdin is consumed by the pipe. Write the content to a temp file
    // and view that.
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join("reed-stdin.md");
    std::fs::write(&tmp_path, &content).context("failed to write temp file for stdin content")?;

    let result = viewer::run(
        &content,
        max_scrollback,
        theme,
        "<stdin>",
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        line,
        None,
        Some(tmp_path.as_path()),
        None,
    )
    .map(|_| ());

    // Clean up temp file (best effort).
    let _ = std::fs::remove_file(&tmp_path);

    result
}

// ── fzf picker mode ─────────────────────────────────────────────

/// Launch fzf with reed as the preview command. When the user selects a
/// file, open it in the interactive viewer.  Quitting the viewer returns
/// to the fzf picker; quitting fzf itself exits reed.
///
/// If stdin is not a TTY (i.e. something is piped in), candidates are
/// buffered and re-fed to fzf on each iteration so the picker can be
/// re-launched after the viewer exits.
#[expect(clippy::too_many_lines)]
fn fzf_pick_and_view(theme: Option<&str>, max_scrollback: usize) -> Result<()> {
    // Vendor directories to exclude when the filter is active.
    const VENDOR_DIRS: &[&str] = &[
        "node_modules",
        "vendor",
        ".git",
        "target",
        "build",
        "dist",
        ".next",
        "__pycache__",
        ".venv",
        "venv",
    ];

    // If the user passed --theme, save it as the current preference so the
    // preview command (which reads from prefs) picks it up.
    if let Some(t) = theme {
        let mut prefs = config::load_preferences();
        config::set_active_theme(&mut prefs, t);
        config::save_preferences(&prefs).context("failed to save theme preference")?;
    }

    // If stdin is piped, buffer the candidates so we can re-feed them to fzf
    // on each loop iteration (the pipe is consumed on first read).
    let piped_candidates = if std::io::stdin().is_terminal() {
        None
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read piped candidates")?;
        Some(buf)
    };

    // Resolve our own binary path so the preview command works regardless
    // of how reed was invoked (cargo run, PATH, relative, etc.).
    let reed_bin = std::env::current_exe().context("cannot determine reed binary path")?;

    // Build the preview command that fzf will invoke for each candidate.
    // No --theme flag: the preview reads from saved preferences so that
    // t/T theme cycling takes effect on reload.
    let preview_cmd = format!("{} --preview {{}}", shell_escape(&reed_bin));

    // Theme cycling commands.
    let next_theme_cmd = format!("{} --next-theme", shell_escape(&reed_bin));
    let prev_theme_cmd = format!("{} --prev-theme", shell_escape(&reed_bin));

    // Header command: prints the styled shortcuts + theme name line.
    let header_cmd = format!("{} --print-header", shell_escape(&reed_bin));

    // Detect fd/fdfind once for the vendor-filter toggle.
    let fd_bin = if Command::new("fd")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        Some("fd")
    } else if Command::new("fdfind")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        Some("fdfind")
    } else {
        None
    };

    // Build the two fd commands: filtered (excludes vendor) and unfiltered.
    let (fd_filtered, fd_unfiltered) = if let Some(bin) = fd_bin {
        let excludes = VENDOR_DIRS.iter().fold(String::new(), |mut acc, d| {
            use std::fmt::Write;
            let _ = write!(acc, " --exclude {d}");
            acc
        });
        (
            format!("{bin} --type f --hidden{excludes}"),
            format!("{bin} --type f --hidden"),
        )
    } else {
        // Fallback: use find (no vendor filtering without fd).
        let cmd = "find . -type f".to_string();
        (cmd.clone(), cmd)
    };

    // Buffer ring: tracks recently opened files for Ctrl-n / Ctrl-p cycling.
    let mut buffer_ring: Vec<PathBuf> = Vec::new();
    // Index into buffer_ring — set before first use inside the loop.
    #[allow(unused_assignments)]
    let mut buffer_index: usize = 0;

    loop {
        // Build the initial header from current preferences (may have changed
        // via theme cycling in the previous iteration).
        let prefs = config::load_preferences();
        let initial_theme =
            &theme::ALL_THEMES[theme::theme_index_by_name(config::active_theme(&prefs))];
        let initial_header = viewer::fzf_header_line(initial_theme);

        let mut fzf = Command::new("fzf");
        fzf.arg("--height").arg("100%");
        fzf.arg("--preview").arg(&preview_cmd);
        fzf.arg("--preview-window").arg("right:64%");
        // Static header showing shortcuts + current theme name.
        fzf.arg("--header").arg(&initial_header);
        // ctrl-/ cycles through preview layouts.
        fzf.arg("--bind")
            .arg("ctrl-/:change-preview-window(right:64%|up:70%|down:40%|hidden)");
        // ctrl-n / ctrl-b cycle themes: update prefs, refresh preview, update header.
        fzf.arg("--bind").arg(format!(
            "ctrl-n:execute-silent({next_theme_cmd})+refresh-preview+transform-header({header_cmd})"
        ));
        fzf.arg("--bind").arg(format!(
            "ctrl-b:execute-silent({prev_theme_cmd})+refresh-preview+transform-header({header_cmd})"
        ));

        // ctrl-v: toggle vendor-file filter (only when not using piped candidates).
        // Uses the prompt text as state: "filtered> " vs "all> ".
        if piped_candidates.is_none() && fd_bin.is_some() {
            // Start with vendor dirs filtered out.
            fzf.arg("--prompt").arg("filtered> ");
            fzf.arg("--bind").arg(format!(
                "ctrl-v:transform:[[ {{fzf:prompt}} =~ filtered ]] \
                 && echo \"change-prompt(all> )+reload({fd_unfiltered})\" \
                 || echo \"change-prompt(filtered> )+reload({fd_filtered})\""
            ));
            // Use the filtered command as the initial source.
            fzf.env("FZF_DEFAULT_COMMAND", &fd_filtered);
        }

        // Feed piped candidates, or let fzf use its default file finder.
        if piped_candidates.is_some() {
            fzf.stdin(std::process::Stdio::piped());
        }

        // fzf needs the real TTY for its UI, and writes the selection to stdout.
        let mut child = fzf
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("failed to launch fzf — is it installed? (brew install fzf)")?;

        // If we have buffered candidates, write them to fzf's stdin.
        if let Some(ref candidates) = piped_candidates
            && let Some(mut stdin) = child.stdin.take()
        {
            use std::io::Write;
            let _ = stdin.write_all(candidates.as_bytes());
            // stdin drops here, signalling EOF to fzf.
        }

        let output = child.wait_with_output().context("fzf process failed")?;

        if !output.status.success() {
            // fzf exits 1 on Ctrl-C / Esc — not an error, just quit.
            return Ok(());
        }

        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if selected.is_empty() {
            return Ok(());
        }

        let file = PathBuf::from(&selected);

        // Add to buffer ring (avoid duplicates at the end).
        if buffer_ring.last() != Some(&file) {
            buffer_ring.push(file.clone());
        }
        buffer_index = buffer_ring.len() - 1;

        // Open the selected file — then loop on Ctrl-n / Ctrl-p to cycle
        // through the buffer ring without returning to fzf.
        loop {
            let cur_file = &buffer_ring[buffer_index];

            // Document files: convert to markdown via pandoc and display
            // in the built-in viewer.
            if is_document_path(cur_file) && has_command("pandoc") {
                let markdown = pandoc_to_markdown(cur_file)?;
                let filename = cur_file.display().to_string();
                let base_dir = cur_file
                    .canonicalize()
                    .unwrap_or_else(|_| cur_file.clone())
                    .parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                let buf_info = if buffer_ring.len() > 1 {
                    Some((buffer_index + 1, buffer_ring.len()))
                } else {
                    None
                };
                let exit = viewer::run(
                    &markdown,
                    max_scrollback,
                    theme,
                    &filename,
                    &base_dir,
                    None,
                    None,
                    Some(cur_file.as_path()),
                    buf_info,
                )?;
                match exit {
                    viewer::ViewerExit::Quit => break,
                    viewer::ViewerExit::BufferNext => {
                        if buffer_ring.len() > 1 {
                            buffer_index = (buffer_index + 1) % buffer_ring.len();
                        }
                    }
                    viewer::ViewerExit::BufferPrev => {
                        if buffer_ring.len() > 1 {
                            buffer_index =
                                (buffer_index + buffer_ring.len() - 1) % buffer_ring.len();
                        }
                    }
                }
                continue;
            }

            // Binary files: images are displayed natively when the
            // terminal supports a graphics protocol; other binaries
            // open in hexyl.
            if is_binary(cur_file) {
                let gfx = viewer::detect_graphics_protocol();
                if images::is_image_path(cur_file) && gfx != images::GraphicsProtocol::None {
                    display_image(cur_file, gfx)?;
                } else if has_command("hexyl") {
                    open_in_hexyl(cur_file)?;
                } else {
                    tracing::warn!(
                        "skipping binary file {} (install hexyl for hex viewing)",
                        cur_file.display()
                    );
                }
                break; // Return to fzf picker.
            }

            let raw_content = std::fs::read_to_string(cur_file)
                .with_context(|| format!("failed to read {}", cur_file.display()))?;

            let is_markdown = highlight::is_markdown_path(cur_file);
            let code_lang = if is_markdown {
                None
            } else {
                highlight::lang_for_path(cur_file)
            };

            let filename = cur_file.display().to_string();
            let base_dir = cur_file
                .canonicalize()
                .unwrap_or_else(|_| cur_file.clone())
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

            // Code / config files: open in an external editor if available,
            // otherwise fall back to the built-in viewer.  Markdown always
            // uses the built-in viewer.
            if !is_markdown
                && highlight::is_editor_preferred(cur_file)
                && let Some(editor) = detect_editor()
            {
                open_in_editor(&editor, cur_file)?;
                break; // Return to fzf picker after editor exits.
            }

            let buf_info = if buffer_ring.len() > 1 {
                Some((buffer_index + 1, buffer_ring.len()))
            } else {
                None
            };
            let exit = viewer::run(
                &raw_content,
                max_scrollback,
                theme,
                &filename,
                &base_dir,
                None,
                code_lang.as_deref(),
                Some(cur_file.as_path()),
                buf_info,
            )?;

            match exit {
                viewer::ViewerExit::Quit => break, // Back to fzf picker.
                viewer::ViewerExit::BufferNext => {
                    if buffer_ring.len() > 1 {
                        buffer_index = (buffer_index + 1) % buffer_ring.len();
                    }
                }
                viewer::ViewerExit::BufferPrev => {
                    if buffer_ring.len() > 1 {
                        buffer_index = (buffer_index + buffer_ring.len() - 1) % buffer_ring.len();
                    }
                }
            }
        }
    }
}

/// Check whether a command is available on `$PATH`.
fn has_command(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Detect the preferred code editor.
///
/// Resolution order:
/// 1. `emacs` if available.
/// 2. `nvim` if available.
/// 3. `$EDITOR` environment variable (if set and available on `$PATH`).
///
/// Returns `None` when no suitable editor is found.
fn detect_editor() -> Option<String> {
    for candidate in &["emacs", "nvim"] {
        if has_command(candidate) {
            return Some((*candidate).to_string());
        }
    }
    if let Ok(editor) = std::env::var("EDITOR") {
        // $EDITOR may contain arguments (e.g. "code --wait"), take the
        // first token as the command name.
        let cmd = editor.split_whitespace().next().unwrap_or(&editor);
        if has_command(cmd) {
            return Some(editor);
        }
        tracing::debug!("$EDITOR={editor:?} is not available on $PATH, trying fallbacks");
    }
    None
}

/// Open a file in the given editor command.  Blocks until the editor exits.
///
/// `editor` may contain arguments (e.g. `"code --wait"`); the file path is
/// appended as the last argument.
fn open_in_editor(editor: &str, path: &Path) -> Result<()> {
    let mut parts = editor.split_whitespace();
    let cmd = parts.next().context("empty editor command")?;
    let mut command = Command::new(cmd);
    for arg in parts {
        command.arg(arg);
    }
    command.arg(path);
    command
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let status = command
        .status()
        .with_context(|| format!("failed to launch {cmd}"))?;
    if !status.success() {
        tracing::warn!("{cmd} exited with status {status}");
    }
    Ok(())
}

/// Heuristic binary detection: read up to 8 KiB and look for NUL bytes.
fn is_binary(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let n = file.take(8192).read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

/// Document extensions that `pandoc` can convert to markdown.
const DOCUMENT_EXTS: &[&str] = &["docx", "pptx", "xlsx", "odt", "rtf", "epub"];

/// Returns `true` when `path` has a document extension convertible by pandoc.
fn is_document_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| DOCUMENT_EXTS.iter().any(|d| d.eq_ignore_ascii_case(ext)))
}

/// Convert a document file to markdown via `pandoc`.
///
/// Returns the markdown string, or an error if pandoc is not installed or
/// conversion fails.
fn pandoc_to_markdown(path: &Path) -> Result<String> {
    let output = Command::new("pandoc")
        .arg(path)
        .arg("-t")
        .arg("markdown")
        .arg("--wrap=none")
        .output()
        .context("failed to run pandoc (is it installed?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pandoc failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Open a binary file in `hexyl`.  Blocks until hexyl exits.
fn open_in_hexyl(path: &Path) -> Result<()> {
    let status = Command::new("hexyl")
        .arg(path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to launch hexyl")?;
    if !status.success() {
        tracing::warn!("hexyl exited with status {status}");
    }
    Ok(())
}

/// Display an image file directly in the terminal using the Kitty or Sixel
/// graphics protocol.  Enters the alternate screen, shows the image, waits
/// for `q`/`Esc`/`Enter`, then restores the terminal.
fn display_image(path: &Path, gfx: images::GraphicsProtocol) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode};
    use crossterm::{cursor, execute, terminal};
    use std::io::Write;

    let (term_cols, term_rows) = terminal::size().context("failed to query terminal size")?;
    let (cell_w, cell_h) = images::cell_size_px();

    // Reserve one row for the status line at the bottom.
    let max_rows = term_rows.saturating_sub(1);
    let max_cols = term_cols;

    let (png_data, img_cols, img_rows) =
        images::load_image(path, max_cols, cell_w, cell_h).context("failed to load image")?;

    // Clamp display rows so the image doesn't overflow the screen.
    let display_rows = img_rows.min(max_rows);
    let display_cols = img_cols.min(max_cols);

    let mut stdout = std::io::stdout();

    terminal::enable_raw_mode()?;
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        terminal::Clear(terminal::ClearType::All),
        cursor::Hide,
        cursor::MoveTo(0, 0)
    )?;

    // Emit the image.
    match gfx {
        images::GraphicsProtocol::Kitty => {
            images::emit_kitty_image(&mut stdout, &png_data, display_cols, display_rows)?;
        }
        images::GraphicsProtocol::Sixel => {
            images::emit_sixel_image(&mut stdout, &png_data, display_cols, display_rows)?;
        }
        images::GraphicsProtocol::None => unreachable!(),
    }

    // Status line at the very bottom.
    let filename = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let hint = format!(" {filename} — press q to exit");
    execute!(stdout, cursor::MoveTo(0, term_rows - 1))?;
    write!(
        stdout,
        "\x1b[7m{hint:<width$}\x1b[0m",
        width = usize::from(term_cols)
    )?;
    stdout.flush()?;

    // Wait for quit key.
    loop {
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => break,
                _ => {}
            }
        }
    }

    // Clean up: delete Kitty images, restore terminal.
    if gfx == images::GraphicsProtocol::Kitty {
        write!(stdout, "\x1b_Ga=d,q=2;\x1b\\")?;
    }
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    Ok(())
}

/// Emit an image to stdout (for `--print` / `--preview` mode) without
/// entering the alternate screen or waiting for input.
///
/// When running inside the fzf preview pane, respects `FZF_PREVIEW_COLUMNS`
/// and `FZF_PREVIEW_LINES` so the image is sized to the pane rather than
/// the full terminal.  A Kitty "delete all" is emitted first so that
/// switching to a non-image file in fzf clears the previous image.
fn print_image(path: &Path, gfx: images::GraphicsProtocol) -> Result<()> {
    use std::io::Write;

    // Prefer fzf preview dimensions, fall back to terminal size.
    let preview_cols: u16 = std::env::var("FZF_PREVIEW_COLUMNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .or_else(|| crossterm::terminal::size().ok().map(|(c, _)| c))
        .unwrap_or(80);
    let preview_rows: u16 = std::env::var("FZF_PREVIEW_LINES")
        .ok()
        .and_then(|v| v.parse().ok())
        .or_else(|| crossterm::terminal::size().ok().map(|(_, r)| r))
        .unwrap_or(24);

    let (cell_w, cell_h) = images::cell_size_px();

    let (png_data, img_cols, img_rows) =
        images::load_image(path, preview_cols, cell_w, cell_h).context("failed to load image")?;

    // Clamp to available preview rows so the image doesn't overflow.
    let display_rows = img_rows.min(preview_rows);
    let display_cols = img_cols.min(preview_cols);

    let mut stdout = std::io::stdout();

    // Delete any previously placed Kitty images so switching files in
    // fzf clears the old image.
    if gfx == images::GraphicsProtocol::Kitty {
        write!(stdout, "\x1b_Ga=d,d=A,q=2;\x1b\\")?;
    }

    match gfx {
        images::GraphicsProtocol::Kitty => {
            images::emit_kitty_image(&mut stdout, &png_data, display_cols, display_rows)?;
        }
        images::GraphicsProtocol::Sixel => {
            images::emit_sixel_image(&mut stdout, &png_data, display_cols, display_rows)?;
        }
        images::GraphicsProtocol::None => unreachable!(),
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

/// Escape a path for use inside a shell command string.
fn shell_escape(path: &Path) -> String {
    let s = path.display().to_string();
    if s.contains(' ') || s.contains('\'') || s.contains('"') || s.contains('\\') {
        // Single-quote the path, escaping any embedded single quotes.
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s
    }
}
