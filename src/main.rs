mod config;
mod highlight;
mod images;
mod input;
mod mermaid;
mod theme;
mod viewer;

use std::io::IsTerminal;
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
    /// Respects FZF_PREVIEW_COLUMNS and FZF_PREVIEW_LINES if set.
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

    // No file argument → launch fzf picker mode.
    let file = match cli.file {
        Some(f) => f,
        None => {
            if cli.preview || cli.print {
                bail!("--preview and --print require a file argument");
            }
            return fzf_pick_and_view(cli.theme.as_deref(), cli.max_scrollback);
        }
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
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if cli.print {
        if is_markdown {
            viewer::print_to_stdout(&raw_content)
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
    let prefs = config::load_preferences();
    let current = theme::theme_index_by_name(&prefs.theme);
    let len = theme::THEMES.len();
    let next = if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    };
    let new_prefs = config::Preferences {
        theme: theme::THEMES[next].name.to_string(),
    };
    config::save_preferences(&new_prefs)?;
    Ok(())
}

// ── fzf picker mode ─────────────────────────────────────────────

/// Launch fzf with reed as the preview command. When the user selects a
/// file, open it in the interactive viewer.
///
/// If stdin is not a TTY (i.e. something is piped in), fzf reads its
/// candidates from that pipe automatically. Otherwise fzf uses its
/// default file finder.
fn fzf_pick_and_view(theme: Option<&str>, max_scrollback: usize) -> Result<()> {
    // If the user passed --theme, save it as the current preference so the
    // preview command (which reads from prefs) picks it up.
    if let Some(t) = theme {
        let prefs = config::Preferences {
            theme: t.to_string(),
        };
        config::save_preferences(&prefs)?;
    }

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

    let mut fzf = Command::new("fzf");
    fzf.arg("--preview").arg(&preview_cmd);
    fzf.arg("--preview-window").arg("right:60%");
    // ctrl-/ cycles through preview layouts.
    fzf.arg("--bind")
        .arg("ctrl-/:change-preview-window(right:60%|up:70%|down:40%|hidden)");
    // ctrl-n / ctrl-b cycle themes (next / back).
    fzf.arg("--bind").arg(format!(
        "ctrl-n:execute-silent({next_theme_cmd})+refresh-preview"
    ));
    fzf.arg("--bind").arg(format!(
        "ctrl-b:execute-silent({prev_theme_cmd})+refresh-preview"
    ));

    // If stdin is piped, fzf inherits it and reads candidates from there.
    // If stdin is a TTY, fzf uses its built-in file finder.
    if !std::io::stdin().is_terminal() {
        fzf.stdin(std::process::Stdio::inherit());
    }

    // fzf needs the real TTY for its UI, and writes the selection to stdout.
    let output = fzf
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("failed to launch fzf — is it installed? (brew install fzf)")?
        .wait_with_output()
        .context("fzf process failed")?;

    if !output.status.success() {
        // fzf exits 1 on Ctrl-C / Esc — not an error, just quit silently.
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
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    viewer::run(
        &raw_content,
        max_scrollback,
        theme,
        &filename,
        &base_dir,
        None,
        code_lang.as_deref(),
    )
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
