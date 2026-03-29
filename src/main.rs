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

/// Terminal markdown viewer powered by libghostty-vt.
#[derive(Parser)]
#[command(name = "reed", version, about)]
struct Cli {
    /// Markdown file to display.
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

    let markdown = std::fs::read_to_string(&cli.file)
        .with_context(|| format!("failed to read {}", cli.file.display()))?;

    let filename = cli.file.display().to_string();

    // Resolve the directory containing the markdown file (for relative image paths).
    let base_dir = cli
        .file
        .canonicalize()
        .unwrap_or_else(|_| cli.file.clone())
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if cli.print {
        viewer::print_to_stdout(&markdown)
    } else if cli.preview {
        viewer::preview(&markdown, cli.theme.as_deref(), cli.line)
    } else {
        viewer::run(
            &markdown,
            cli.max_scrollback,
            cli.theme.as_deref(),
            &filename,
            &base_dir,
            cli.line,
        )
    }
}
