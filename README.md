# reed

Terminal file viewer with syntax highlighting, inline images, and Mermaid
diagrams -- powered by [libghostty-vt](https://github.com/Uzaaft/libghostty-rs).

Reed renders Markdown and source code files in a full-screen TUI. Markdown is
converted to ANSI by termimad, fed through a libghostty-vt terminal instance,
and painted cell-by-cell with dirty tracking. Source code files bypass termimad
entirely and use syntect for direct syntax highlighting.

## Features

- **14 color themes** (7 dark, 7 light) -- cycle at runtime with `t` / `T`
- **Syntax highlighting** for fenced code blocks and standalone source files
  (Rust, Python, Go, Zig, and 40+ languages via syntect)
- **Inline images** via the Kitty graphics protocol (PNG, JPEG, GIF, WebP)
- **Mermaid diagrams** rendered to inline images when `mmdc` is on PATH
- **fzf integration** -- run `reed` with no arguments to get an interactive
  file picker with live preview; pipe a file list to narrow candidates
- **Scrollback navigation** -- PgUp/PgDn, Home/End, arrow keys, `j`/`k`
- **Fuzzy heading jump** -- press `s` to search headings via fzf
- **Theme persistence** -- saved to `~/.config/reed/preferences.toml`
- **Ghostty detection** -- auto-selects "FFE Dark" in Ghostty terminal
- **Pipe-friendly** -- `reed --print FILE` dumps themed output to stdout

## Install

Requires Rust (nightly or stable 1.85+) and **Zig 0.15.x** on PATH (used by
the libghostty-vt build).

```sh
cargo build --release
cp target/release/reed ~/bin/reed
```

On **macOS / Apple Silicon** you must re-sign after copying:

```sh
codesign -f -s - ~/bin/reed    # cp invalidates the ad-hoc signature
```

## Usage

```
reed <file>              # view a file in interactive mode
reed                     # launch fzf file picker with preview
find . -name '*.rs' | reed   # pipe candidates into the picker
reed --print <file>      # print themed output to stdout
reed --preview <file>    # fzf preview mode (used internally)
reed --theme "Gruvbox" <file>  # override saved theme
```

### Interactive keybindings

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl-c` | Quit |
| `j` / `Down` / `Scroll down` | Scroll down |
| `k` / `Up` / `Scroll up` | Scroll up |
| `PgDn` / `Space` | Page down |
| `PgUp` | Page up |
| `Home` / `g` | Top of file |
| `End` / `G` | Bottom of file |
| `t` | Next theme |
| `T` | Previous theme |
| `s` | Fuzzy heading search (fzf) |

### fzf picker keybindings

| Key | Action |
|-----|--------|
| `Enter` | Open file in viewer |
| `Ctrl-/` | Cycle preview layout |
| `Ctrl-n` | Next theme |
| `Ctrl-b` | Previous theme |

## Themes

Dark: Default, Gruvbox, Solarized, Ayu, Flexoki, Zoegi, FFE Dark

Light: Default Light, Gruvbox Light, Solarized Light, Ayu Light, Flexoki Light,
Zoegi Light, FFE Light

## Architecture

```
src/
  main.rs       entry point, CLI (clap), fzf picker, tracing init
  viewer.rs     core viewer loop, frame rendering, image/mermaid pipeline
  input.rs      keyboard/scroll event handling, heading picker
  highlight.rs  syntax highlighting (syntect), Zig syntax bundling
  images.rs     Kitty graphics protocol, image loading/resizing
  mermaid.rs    Mermaid diagram detection and rendering via mmdc
  theme.rs      14 themes, skin builder
  config.rs     preferences persistence, Ghostty detection
```

## Development

```sh
cargo build      # compile (requires Zig 0.15.x)
cargo test       # run 72 unit tests
cargo clippy     # lint
cargo fmt        # format
```

On macOS, set the dynamic library path before running from the build directory:

```sh
DYLD_LIBRARY_PATH=$(dirname $(find target/debug/build/libghostty-vt-sys-*/out \
  -name "libghostty-vt*" | head -1)) cargo run -- README.md
```

## License

[MIT](LICENSE) -- Copyright (c) 2026 Pastel Sketchbook
