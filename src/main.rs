mod config;
mod highlight;
mod images;
mod input;
mod mermaid;
mod theme;
mod viewer;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

/// Terminal file viewer with syntax highlighting, powered by libghostty-vt.
#[derive(Parser)]
#[command(name = "reed", version, about)]
struct Cli {
    /// File to display (markdown rendered richly; code files syntax-highlighted).
    file: PathBuf,

    /// Maximum scrollback lines (default: 100 000).
    #[arg(long, default_value_t = 100_000)]
    max_scrollback: usize,

    /// Print rendered markdown to stdout instead of launching the interactive viewer.
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    let raw_content = std::fs::read_to_string(&cli.file)
        .with_context(|| format!("failed to read {}", cli.file.display()))?;

    let is_markdown = highlight::is_markdown_path(&cli.file);
    let code_lang = if is_markdown {
        None
    } else {
        highlight::lang_for_path(&cli.file)
    };

    let filename = cli.file.display().to_string();

    // Resolve the directory containing the file (for relative image paths).
    let base_dir = cli
        .file
        .canonicalize()
        .unwrap_or_else(|_| cli.file.clone())
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
