mod input;
mod viewer;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// Terminal markdown viewer powered by libghostty-vt.
#[derive(Parser)]
#[command(name = "md-ghostty", version, about)]
struct Cli {
    /// Markdown file to display.
    file: PathBuf,

    /// Maximum scrollback lines (default: 100 000).
    #[arg(long, default_value_t = 100_000)]
    max_scrollback: usize,

    /// Print rendered markdown to stdout instead of launching the interactive viewer.
    #[arg(long)]
    print: bool,
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

    if cli.print {
        viewer::print_to_stdout(&markdown)
    } else {
        viewer::run(&markdown, cli.max_scrollback)
    }
}
