# 0001 -- Architecture Rationale

This document records the key architectural decisions in reed and the reasoning
behind them. Each section covers a non-obvious choice that future contributors
(or future-us) will want context on.

---

## 1. libghostty-vt as the VT backend

**Decision**: Render markdown to ANSI escape sequences (via termimad), feed them
into a libghostty-vt `Terminal` instance, then read back cell-by-cell for
display.

**Why**: A real VT parser handles the full complexity of ANSI rendering --
wrapping, cursor positioning, SGR stacking, and scrollback -- so reed does not
have to reimplement any of it. This gives correct rendering for arbitrarily
complex ANSI output without writing a layout engine.

**Trade-off**: All libghostty-vt types are `!Send`, which forces a
single-threaded architecture. This is acceptable for a viewer with no background
I/O.

---

## 2. Two rendering pipelines (markdown vs code)

**Decision**: Markdown files flow through termimad + libghostty-vt. Non-markdown
source files bypass termimad entirely and use syntect directly.

**Why**: termimad escapes ANSI codes inside fenced code blocks. When syntect
produces `\x1b[38;2;R;G;Bm` sequences, termimad renders them as literal text
(`38;2;...`). The only reliable fix is to keep syntect output away from termimad
for standalone code files.

For markdown files, fenced code blocks are pre-processed by syntect *before*
being passed to termimad -- the highlighted output replaces the raw fenced block,
so termimad sees pre-rendered ANSI rather than triple-backtick code regions.

**Pipelines**:

```
Markdown:
  md -> highlight_code_blocks() -> build_processed_markdown()
     -> join_paragraphs() -> termimad skin.text() -> \r\n fix
     -> vt_write() -> cell-by-cell render

Code files:
  source -> highlight_code(source, lang, bg) -> ansi_bg() per line
         -> stdout (preview) or vt_write() (interactive)
```

---

## 3. `\n` to `\r\n` conversion

**Decision**: Convert all `\n` to `\r\n` before calling `term.vt_write()`.

**Why**: libghostty-vt follows strict VT terminal semantics where LF (`\n`) only
moves the cursor down one row *without* returning to column 0. A bare `\n`
produces staircase output. Real terminals auto-translate, but the library does
not. Explicit `\r\n` is required.

---

## 4. Images bypass both termimad and libghostty-vt

**Decision**: Images are extracted from markdown during pre-processing, replaced
with placeholder lines, and emitted directly to stdout via the Kitty graphics
protocol.

**Why**: termimad has zero image awareness -- `![alt](path)` passes through as
literal text. The libghostty-vt cell iterator also exposes no image-related data.
The only viable path is to handle images out-of-band: extract them before
termimad sees the markdown, track their row positions, and emit Kitty escape
sequences directly to stdout during frame rendering.

**Kitty protocol details**: PNG format (`f=100`), base64-encoded payload chunked
at 4096 bytes, wrapped in `\x1b_G...\x1b\\` sequences. Frames are wrapped in
`BeginSynchronizedUpdate` / `EndSynchronizedUpdate` to prevent blink/flicker.

---

## 5. Kitty graphics detection and fallback

**Decision**: Detect `TERM=tmux-256color`, `TMUX` env var, or `screen` to
disable Kitty graphics. Fall back to showing raw code blocks for mermaid diagrams
and alt text for images.

**Why**: tmux and screen do not pass through Kitty graphics escapes. Attempting
to emit them produces garbage output. Explicit detection avoids this.

---

## 6. Mermaid as optional dependency

**Decision**: Check for `mmdc` on PATH at runtime. If present, render mermaid
blocks to PNG and display as inline images. If absent, show the raw source as a
fenced code block.

**Why**: `mmdc` (mermaid-cli) is a heavy Node.js dependency. Making it optional
means reed works everywhere, with mermaid as a bonus when available.

---

## 7. Two-set SyntaxSet architecture for custom syntaxes

**Decision**: Maintain two separate `SyntaxSet` instances -- one from syntect's
default set, one built from bundled `.sublime-syntax` files (currently Zig).
A `find_syntax()` helper searches both and returns `(&SyntaxReference, &SyntaxSet)`.

**Why**: `SyntaxSetBuilder::add()` accepts `SyntaxDefinition` objects, but
there is no way to extract definitions from an existing `SyntaxSet` to merge
them. A `SyntaxReference` from one set cannot be used with a different set for
highlighting. The two-set approach is the only correct solution without forking
syntect.

---

## 8. Theme background for code files

**Decision**: For non-markdown files, manually emit ANSI 24-bit background codes
(`\x1b[48;2;R;G;Bm`) per line, followed by `\x1b[K` (clear to EOL) and
`\x1b[0m` (reset).

**Why**: syntect's `as_24_bit_terminal_escaped(&ranges, false)` only produces
foreground color codes. When termimad is bypassed, there is no other mechanism to
apply the theme background. Manual per-line wrapping is the simplest approach
that produces correct full-width background fills.

---

## 9. SGR edge cases

Three SGR pitfalls caught during development:

| Issue | Root cause | Fix |
|-------|-----------|-----|
| `Attribute::NoBold` renders as double underline | SGR 21 = double underline per spec | Use `Attribute::NormalIntensity` (SGR 22) |
| `ResetColor` leaves bold/italic active | SGR 39/49 only reset colors | Use `SetAttribute(Attribute::Reset)` at boundaries |
| `Attribute::Reset` clears fg/bg colors | SGR 0 resets everything | Re-emit fg/bg after every `Reset` |

---

## 10. fzf as the file picker

**Decision**: Running `reed` with no arguments spawns fzf as a child process
with `--preview 'reed --preview {}'`. Selected files open in the interactive
viewer.

**Why**: fzf is ubiquitous, fast, and handles fuzzy matching, layout, and
keybinding better than any custom implementation. Reed focuses on rendering;
fzf handles selection.

**macOS constraint**: Option/Alt modifier keys send special Unicode characters in
macOS terminals instead of acting as modifiers. All fzf bindings use ctrl-based
keys (`ctrl-/`, `ctrl-n`, `ctrl-b`).

**Theme cycling in fzf**: `ctrl-n` triggers
`execute-silent(reed --next-theme)+refresh-preview+transform-header(reed --print-header)`.
This writes the new theme to preferences, re-renders the preview, and updates
the header line showing the active theme name -- all in one fzf binding.

---

## 11. Single-threaded design

**Decision**: Everything runs on the main thread.

**Why**: libghostty-vt types are `!Send` and `!Sync`. There is no benefit to
threading for a viewer that blocks on user input between frames. The dirty
tracking system (only redraw changed rows) keeps frame rendering fast enough
that threading would add complexity with no measurable gain.

---

## 12. Dirty tracking for partial redraws

**Decision**: Only redraw rows marked dirty by libghostty-vt. Force a full
redraw (`DirtyType::Full`) on scroll and resize events.

**Why**: Redrawing all rows on every keypress causes visible flicker, especially
with Kitty image sequences. Dirty tracking reduces the per-frame work to the
minimum necessary. Full redraws on scroll/resize are unavoidable since every
visible row changes.
