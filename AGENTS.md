---
description: Rust project conventions for md-ghostty.
globs: "*.rs, Cargo.toml, Cargo.lock"
alwaysApply: true
---

# Rust — md-ghostty

Production-grade terminal markdown viewer powered by libghostty-vt.
This is a Rust 2024-edition project. Use `cargo` for all build/test/run tasks.

## Build & Run

- `cargo build` to compile (requires Zig 0.15.x on PATH).
- `cargo run -- <file.md>` to view a markdown file.
- `cargo test` to run unit tests.
- `cargo clippy` for lints.
- `cargo fmt` to format code.
- On macOS: prefix with `DYLD_LIBRARY_PATH=$(dirname $(find target/debug/build/libghostty-vt-sys-*/out -name "libghostty-vt*" | head -1))`.

## Dependencies

Core crates: `libghostty-vt` (git, Uzaaft/libghostty-rs), `termimad`, `crossterm`, `clap` (derive), `anyhow`, `tracing`, `tracing-subscriber`.

## Architecture

```
src/
  main.rs       — entry point, CLI args (clap), tracing init
  viewer.rs     — core viewer loop: Terminal + RenderState + frame rendering
  input.rs      — keyboard/scroll event handling (crossterm events → ScrollViewport)
```

## Key Design Decisions

- **libghostty-vt as VT backend**: Markdown is rendered to ANSI via termimad, fed into a Terminal instance, then read back cell-by-cell for display.
- **Dirty tracking**: Only redraws rows marked dirty; forces Full redraw on scroll/resize.
- **Scrollback**: Uses Terminal scrollback for document navigation (ScrollViewport::Delta, Top, Bottom).
- **Single-threaded**: All libghostty-vt types are !Send; everything runs on the main thread.

## Conventions

- Use `anyhow::Result` for all fallible functions.
- Use `tracing::{info, warn, error, debug}` instead of `println!` / `eprintln!`.
- Keep modules small and focused; one responsibility per file.
