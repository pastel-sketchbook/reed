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
#[derive(Parser)]
#[command(name = "reed", version, about)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// File to display. If omitted, launches fzf for interactive file picking.
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

    /// Print the ANSI-styled fzf header line (shortcuts + theme name) and exit.
    /// Used internally by fzf transform-header to update the header on theme change.
    #[arg(long)]
    print_header: bool,
}

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
        let theme = &theme::THEMES[theme::theme_index_by_name(config::active_theme(&prefs))];
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

    let raw_content = std::fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;

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
        )
    }
}

// ── Theme cycling (for fzf integration) ─────────────────────────

/// Cycle to the next or previous theme, save the preference, and exit.
fn cycle_theme(forward: bool) -> Result<()> {
    let mut prefs = config::load_preferences();
    let current = theme::theme_index_by_name(config::active_theme(&prefs));
    let len = theme::THEMES.len();
    let next = if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    };
    config::set_active_theme(&mut prefs, theme::THEMES[next].name);
    config::save_preferences(&prefs).context("failed to save theme preference")?;
    Ok(())
}

// ── fzf picker mode ─────────────────────────────────────────────

/// Launch fzf with reed as the preview command. When the user selects a
/// file, open it in the interactive viewer.  Quitting the viewer returns
/// to the fzf picker; quitting fzf itself exits reed.
///
/// If stdin is not a TTY (i.e. something is piped in), candidates are
/// buffered and re-fed to fzf on each iteration so the picker can be
/// re-launched after the viewer exits.
fn fzf_pick_and_view(theme: Option<&str>, max_scrollback: usize) -> Result<()> {
    // If the user passed --theme, save it as the current preference so the
    // preview command (which reads from prefs) picks it up.
    if let Some(t) = theme {
        let mut prefs = config::load_preferences();
        config::set_active_theme(&mut prefs, t);
        config::save_preferences(&prefs).context("failed to save theme preference")?;
    }

    // If stdin is piped, buffer the candidates so we can re-feed them to fzf
    // on each loop iteration (the pipe is consumed on first read).
    let piped_candidates = if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read piped candidates")?;
        Some(buf)
    } else {
        None
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
        let excludes: String = VENDOR_DIRS
            .iter()
            .map(|d| format!(" --exclude {d}"))
            .collect();
        (
            format!("{bin} --type f --hidden{excludes}"),
            format!("{bin} --type f --hidden"),
        )
    } else {
        // Fallback: use find (no vendor filtering without fd).
        let cmd = "find . -type f".to_string();
        (cmd.clone(), cmd)
    };

    loop {
        // Build the initial header from current preferences (may have changed
        // via theme cycling in the previous iteration).
        let prefs = config::load_preferences();
        let initial_theme =
            &theme::THEMES[theme::theme_index_by_name(config::active_theme(&prefs))];
        let initial_header = viewer::fzf_header_line(initial_theme);

        let mut fzf = Command::new("fzf");
        fzf.arg("--height").arg("90%");
        fzf.arg("--preview").arg(&preview_cmd);
        fzf.arg("--preview-window").arg("right:60%");
        // Static header showing shortcuts + current theme name.
        fzf.arg("--header").arg(&initial_header);
        // ctrl-/ cycles through preview layouts.
        fzf.arg("--bind")
            .arg("ctrl-/:change-preview-window(right:60%|up:70%|down:40%|hidden)");
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
        let raw_content = std::fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()))?;

        let is_markdown = highlight::is_markdown_path(&file);
        let code_lang = if is_markdown {
            None
        } else {
            highlight::lang_for_path(&file)
        };

        let filename = file.display().to_string();
        let base_dir = file
            .canonicalize()
            .unwrap_or_else(|_| file.clone())
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

        // Code files: open in nvim if available, otherwise fall back to
        // the built-in viewer.  Markdown always uses the built-in viewer.
        if code_lang.is_some() && has_nvim() {
            open_in_nvim(&file)?;
        } else {
            viewer::run(
                &raw_content,
                max_scrollback,
                theme,
                &filename,
                &base_dir,
                None,
                code_lang.as_deref(),
            )?;
        }
    }
}

/// Check whether `nvim` is available on `$PATH`.
fn has_nvim() -> bool {
    Command::new("nvim")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Open a file in `nvim`.  Blocks until the editor exits.
fn open_in_nvim(path: &Path) -> Result<()> {
    let status = Command::new("nvim")
        .arg(path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to launch nvim")?;
    if !status.success() {
        tracing::warn!("nvim exited with status {status}");
    }
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
