//! Syntax highlighting for fenced code blocks.
//!
//! Uses `syntect` to produce per-token ANSI foreground colors. The highlighted
//! text replaces the original code content *inside* the fences so that:
//!
//! 1. `join_paragraphs()` passes it through verbatim (inside code fence).
//! 2. `termimad` applies `code_bg` background while syntect's fg colors
//!    override the mono `code_fg`.
//! 3. The VT terminal renders full-color syntax highlighting.

use std::sync::OnceLock;

use crossterm::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxDefinition, SyntaxSet, SyntaxSetBuilder};
use syntect::util::as_24_bit_terminal_escaped;
use tracing::{debug, warn};

// ── Bundled syntax definitions ───────────────────────────────────

/// Zig `.sublime-syntax` bundled from ziglang/sublime-zig-language (MIT).
const ZIG_SYNTAX: &str = include_str!("../syntaxes/Zig.sublime-syntax");

/// TypeScript `.sublime-syntax` — standalone (no v2 `extends`).
const TS_SYNTAX: &str = include_str!("../syntaxes/TypeScript.sublime-syntax");

/// TSX `.sublime-syntax` — standalone TypeScript + JSX (no v2 `extends`).
const TSX_SYNTAX: &str = include_str!("../syntaxes/TSX.sublime-syntax");

/// Elixir `.sublime-syntax` — covers core Elixir language features.
const ELIXIR_SYNTAX: &str = include_str!("../syntaxes/Elixir.sublime-syntax");

// ── Lazy-loaded syntect resources ────────────────────────────────

/// Default syntaxes shipped with syntect.
fn default_syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Additional syntaxes bundled with reed (Zig, TypeScript, TSX, etc.).
fn custom_syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(|| {
        let mut builder = SyntaxSetBuilder::new();
        for (src, name) in [
            (ZIG_SYNTAX, "Zig"),
            (TS_SYNTAX, "TypeScript"),
            (TSX_SYNTAX, "TSX"),
            (ELIXIR_SYNTAX, "Elixir"),
        ] {
            match SyntaxDefinition::load_from_str(src, true, Some("syntaxes")) {
                Ok(def) => builder.add(def),
                Err(e) => warn!("failed to load bundled {name} syntax: {e}"),
            }
        }
        builder.build()
    })
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

// ── Theme selection ──────────────────────────────────────────────

/// Determine whether a `Color` represents a "light" background (luminance > 128).
fn is_light_bg(color: Color) -> bool {
    match color {
        Color::Rgb { r, g, b } => {
            let luminance = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
            luminance > 128.0
        }
        // Color::Reset = transparent / terminal default — assume dark.
        _ => false,
    }
}

/// Pick a syntect theme name that complements the reed theme's background.
fn syntect_theme_name(bg: Color) -> &'static str {
    if is_light_bg(bg) {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    }
}

// ── File-type detection ──────────────────────────────────────────

/// Markdown file extensions (case-insensitive check done by caller).
const MARKDOWN_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "mdx"];

/// Return `true` if `path` looks like a Markdown file based on its extension.
pub fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| MARKDOWN_EXTS.iter().any(|m| m.eq_ignore_ascii_case(ext)))
}

/// Extensions for config/data files that should always open in an external
/// editor from the fzf picker, even when syntect doesn't recognize them.
const EDITOR_PREFERRED_EXTS: &[&str] = &[
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "json5",
    "xml",
    "svg",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    "csv",
    "tsv",
];

/// Return `true` if `path` has an extension that should be opened in an
/// external editor from the fzf picker.
///
/// This catches config/data formats (TOML, YAML, JSON, XML, SVG, etc.) that
/// syntect may not recognize, plus any extension that syntect *does* recognize
/// (i.e. `lang_for_path` returns `Some`).
pub fn is_editor_preferred(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    // Explicit list first (handles toml, svg, etc. that syntect misses).
    if EDITOR_PREFERRED_EXTS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
    {
        return true;
    }
    // Fall back to syntect recognition (all code files).
    lang_for_path(path).is_some()
}

/// Derive a language tag from a file path's extension.
///
/// Returns `Some("rs")` for `foo.rs`, `None` for files without an extension
/// or whose extension isn't recognized by syntect. Checks both the default
/// syntax set and reed's bundled custom syntaxes (e.g. Zig).
/// The returned tag is suitable for use with `highlight_code()`.
pub fn lang_for_path(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    // Validate that at least one syntax set knows this extension.
    if default_syntax_set().find_syntax_by_extension(ext).is_none()
        && custom_syntax_set().find_syntax_by_extension(ext).is_none()
    {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Wrap raw source code in a Markdown fenced code block.
///
/// Used to route non-Markdown files through the normal rendering pipeline
/// (termimad + syntect highlighting).
#[cfg(test)]
fn wrap_in_code_fence(source: &str, lang: &str) -> String {
    // Use a long fence to avoid conflicts with content.
    let mut out = String::with_capacity(source.len() + lang.len() + 16);
    out.push_str("```");
    out.push_str(lang);
    out.push('\n');
    out.push_str(source);
    if !source.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

// ── Core highlighting ────────────────────────────────────────────

/// Highlight a code block using syntect.
///
/// Returns the highlighted source as a string with ANSI foreground escape
/// sequences (no background), or `None` if the language is not recognized
/// by syntect. Searches both default and custom (bundled) syntax sets.
pub fn highlight_code(source: &str, lang: &str, bg: Color) -> Option<String> {
    let ts = theme_set();

    // Try default syntaxes first, then custom bundled ones.
    let (syntax, ss) = find_syntax(lang)?;

    let theme_name = syntect_theme_name(bg);
    let theme = ts.themes.get(theme_name)?;

    let mut h = HighlightLines::new(syntax, theme);
    let mut output = String::new();

    for line in source.lines() {
        let ranges = h.highlight_line(line, ss).ok()?;
        let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
        output.push_str(&escaped);
        output.push('\n');
    }

    // Remove trailing newline if source didn't have one.
    if !source.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }

    Some(output)
}

/// Look up a syntax by token name or extension across both syntax sets.
///
/// Returns a reference to the matched `SyntaxReference` and the `SyntaxSet`
/// it belongs to (needed for `HighlightLines`).
fn find_syntax(
    lang: &str,
) -> Option<(
    &'static syntect::parsing::SyntaxReference,
    &'static SyntaxSet,
)> {
    let defaults = default_syntax_set();
    if let Some(s) = defaults
        .find_syntax_by_token(lang)
        .or_else(|| defaults.find_syntax_by_extension(lang))
    {
        return Some((s, defaults));
    }

    let custom = custom_syntax_set();
    if let Some(s) = custom
        .find_syntax_by_token(lang)
        .or_else(|| custom.find_syntax_by_extension(lang))
    {
        return Some((s, custom));
    }

    None
}

// ── Markdown-level replacement ───────────────────────────────────

/// Extract the language tag from a code fence opening line.
///
/// Returns `Some("rust")` for `` ```rust ``, `None` for bare `` ``` ``.
fn extract_fence_lang(trimmed: &str) -> Option<String> {
    let rest = if let Some(r) = trimmed.strip_prefix("```") {
        r
    } else if let Some(r) = trimmed.strip_prefix("~~~") {
        r
    } else {
        return None;
    };

    let lang = rest.trim();
    if lang.is_empty() {
        None
    } else {
        Some(lang.to_lowercase())
    }
}

/// Check if a trimmed line is a closing code fence (triple backticks or tildes with no lang tag).
fn is_closing_fence(trimmed: &str, fence_char: char) -> bool {
    let prefix: String = std::iter::repeat_n(fence_char, 3).collect();
    trimmed.starts_with(&prefix) && trimmed.trim_start_matches(fence_char).trim().is_empty()
}

/// Replace fenced code block contents with syntax-highlighted ANSI text.
///
/// Keeps the code fences intact so downstream processing (`join_paragraphs`,
/// `termimad`) handles structure correctly. Only replaces content for blocks
/// with recognized language tags. Blocks tagged `mermaid` are skipped (handled
/// separately by the mermaid pipeline).
///
/// `bg` is the theme's background color, used to select a dark or light
/// syntect highlighting palette.
pub fn highlight_code_blocks(markdown: &str, bg: Color) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut output = String::with_capacity(markdown.len());
    let mut i = 0;
    let mut highlighted_count = 0u32;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();

        // Check for opening code fence with language tag.
        if let Some(lang) = extract_fence_lang(trimmed) {
            // Skip mermaid blocks — handled by the mermaid pipeline.
            if lang == "mermaid" {
                output.push_str(lines[i]);
                output.push('\n');
                i += 1;
                continue;
            }

            let fence_char = if trimmed.starts_with('`') { '`' } else { '~' };

            // Collect code content until closing fence.
            let mut code_lines = Vec::new();
            let mut j = i + 1;
            let mut found_close = false;

            while j < lines.len() {
                let next_trimmed = lines[j].trim_start();
                if is_closing_fence(next_trimmed, fence_char) {
                    found_close = true;
                    break;
                }
                code_lines.push(lines[j]);
                j += 1;
            }

            if !found_close {
                // Unclosed fence — pass through as-is.
                output.push_str(lines[i]);
                output.push('\n');
                i += 1;
                continue;
            }

            // Attempt syntax highlighting.
            let code_source: String = code_lines.join("\n");
            if let Some(highlighted) = highlight_code(&code_source, &lang, bg) {
                // Opening fence (unchanged).
                output.push_str(lines[i]);
                output.push('\n');

                // Highlighted content (replaces original code lines).
                for hl_line in highlighted.lines() {
                    output.push_str(hl_line);
                    output.push('\n');
                }

                // Closing fence (unchanged).
                output.push_str(lines[j]);
                output.push('\n');

                highlighted_count += 1;
            } else {
                // Language not recognized — pass through as-is.
                output.push_str(lines[i]);
                output.push('\n');
                for &code_line in &code_lines {
                    output.push_str(code_line);
                    output.push('\n');
                }
                output.push_str(lines[j]);
                output.push('\n');
            }

            i = j + 1;
        } else {
            output.push_str(lines[i]);
            output.push('\n');
            i += 1;
        }
    }

    // Preserve trailing newline behavior.
    if !markdown.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }

    debug!(count = highlighted_count, "syntax-highlighted code blocks");
    output
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_rust_block() {
        let md = "# Title\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n";
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code_blocks(md, dark_bg);

        // Fences should be preserved.
        assert!(result.contains("```rust"));
        assert!(result.lines().filter(|l| l.trim() == "```").count() >= 1);

        // ANSI escapes should be present (syntect output).
        assert!(result.contains("\x1b["));

        // Original un-highlighted text should NOT be present inside the fence.
        // (The line `fn main() {` should now have ANSI codes around it.)
        // We check that the raw `fn main()` without ANSI prefix is gone.
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```rust"))
            .skip(1) // skip the fence line
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "code section should contain ANSI escapes"
        );
    }

    #[test]
    fn skip_mermaid_blocks() {
        let md = "```mermaid\ngraph TD\n    A --> B\n```\n";
        let result = highlight_code_blocks(md, Color::Reset);
        // Mermaid blocks should be untouched.
        assert_eq!(result, md);
    }

    #[test]
    fn pass_through_bare_fence() {
        let md = "```\nplain code\n```\n";
        let result = highlight_code_blocks(md, Color::Reset);
        // No language tag — should be unchanged.
        assert_eq!(result, md);
    }

    #[test]
    fn unknown_language_unchanged() {
        // "xyzlang" is not recognized by syntect.
        let md = "```xyzlang\nsome code\n```\n";
        let result = highlight_code_blocks(md, Color::Reset);
        assert_eq!(result, md);
    }

    #[test]
    fn unclosed_fence_unchanged() {
        let md = "```rust\nfn main() {}\n";
        let result = highlight_code_blocks(md, Color::Reset);
        assert_eq!(result, md);
    }

    #[test]
    fn multiple_blocks_highlighted() {
        let md = "\
```rust
fn foo() {}
```

```python
def bar():
    pass
```
";
        let result = highlight_code_blocks(md, Color::Reset);
        // Both blocks should have ANSI escapes.
        let blocks: Vec<&str> = result.split("```").collect();
        // blocks: ["", "rust\n...\n", "\n\n", "python\n...\n", "\n"]
        assert!(blocks.len() >= 4, "should have content between fences");
        assert!(
            blocks[1].contains("\x1b["),
            "rust block should be highlighted"
        );
        assert!(
            blocks[3].contains("\x1b["),
            "python block should be highlighted"
        );
    }

    #[test]
    fn light_theme_uses_different_palette() {
        let code = "fn main() {}";
        let dark = highlight_code(code, "rust", Color::Rgb { r: 0, g: 0, b: 0 });
        let light = highlight_code(
            code,
            "rust",
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        );
        assert!(dark.is_some());
        assert!(light.is_some());
        // Different palettes should produce different ANSI output.
        assert_ne!(dark.unwrap(), light.unwrap());
    }

    #[test]
    fn preserves_line_count() {
        let md = "line1\n```rust\nfn a() {}\nfn b() {}\n```\nline6\n";
        let result = highlight_code_blocks(md, Color::Reset);
        assert_eq!(
            md.lines().count(),
            result.lines().count(),
            "line count should be preserved"
        );
    }

    #[test]
    fn tilde_fences_supported() {
        let md = "~~~rust\nlet x = 42;\n~~~\n";
        let result = highlight_code_blocks(md, Color::Reset);
        assert!(
            result.contains("\x1b["),
            "tilde fences should be highlighted"
        );
        assert!(result.contains("~~~rust"));
    }

    #[test]
    fn extract_fence_lang_cases() {
        assert_eq!(extract_fence_lang("```rust"), Some("rust".to_string()));
        assert_eq!(extract_fence_lang("```Python"), Some("python".to_string()));
        assert_eq!(extract_fence_lang("~~~js"), Some("js".to_string()));
        assert_eq!(extract_fence_lang("```"), None);
        assert_eq!(extract_fence_lang("~~~"), None);
        assert_eq!(extract_fence_lang("not a fence"), None);
    }

    #[test]
    fn is_markdown_path_cases() {
        use std::path::Path;
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("notes.markdown")));
        assert!(is_markdown_path(Path::new("doc.MDX")));
        assert!(!is_markdown_path(Path::new("main.rs")));
        assert!(!is_markdown_path(Path::new("script.py")));
        assert!(!is_markdown_path(Path::new("noext")));
    }

    #[test]
    fn lang_for_path_known_extensions() {
        use std::path::Path;
        assert_eq!(lang_for_path(Path::new("main.rs")), Some("rs".to_string()));
        assert_eq!(lang_for_path(Path::new("app.py")), Some("py".to_string()));
        assert_eq!(lang_for_path(Path::new("index.js")), Some("js".to_string()));
    }

    #[test]
    fn lang_for_path_config_and_data_extensions() {
        use std::path::Path;
        // Config / data file formats — check which syntect recognizes.
        let toml = lang_for_path(Path::new("config.toml"));
        let yaml = lang_for_path(Path::new("data.yaml"));
        let yml = lang_for_path(Path::new("data.yml"));
        let json = lang_for_path(Path::new("data.json"));
        let xml = lang_for_path(Path::new("page.xml"));
        let svg = lang_for_path(Path::new("icon.svg"));
        // At least yaml, json, xml should be recognized by syntect defaults.
        assert!(yaml.is_some(), "yaml not recognized: {yaml:?}");
        assert!(yml.is_some(), "yml not recognized: {yml:?}");
        assert!(json.is_some(), "json not recognized: {json:?}");
        assert!(xml.is_some(), "xml not recognized: {xml:?}");
        // toml and svg may not be in syntect's default set.
        // Our editor-preferred list handles them regardless.
        let _ = (toml, svg);
    }

    #[test]
    fn is_editor_preferred_catches_all_targets() {
        use std::path::Path;
        // Extensions that syntect may not recognize but should still open in editor.
        assert!(is_editor_preferred(Path::new("config.toml")));
        assert!(is_editor_preferred(Path::new("icon.svg")));
        assert!(is_editor_preferred(Path::new("settings.ini")));
        assert!(is_editor_preferred(Path::new("data.csv")));
        assert!(is_editor_preferred(Path::new("app.env")));
        // Extensions syntect does recognize — should also be editor-preferred.
        assert!(is_editor_preferred(Path::new("data.yaml")));
        assert!(is_editor_preferred(Path::new("data.json")));
        assert!(is_editor_preferred(Path::new("page.xml")));
        assert!(is_editor_preferred(Path::new("main.rs")));
        assert!(is_editor_preferred(Path::new("app.py")));
        // Markdown should NOT be editor-preferred (uses built-in viewer).
        // is_editor_preferred doesn't check markdown; that's the caller's job.
        // But markdown has no syntect code_lang, so it would return false
        // unless .md is in a syntax set. Let's just verify the caller logic
        // is correct by checking that .md is not in EDITOR_PREFERRED_EXTS.
        assert!(!EDITOR_PREFERRED_EXTS.iter().any(|e| *e == "md"));
        // No extension → false.
        assert!(!is_editor_preferred(Path::new("Makefile")));
    }

    #[test]
    fn lang_for_path_zig_extensions() {
        use std::path::Path;
        assert_eq!(
            lang_for_path(Path::new("build.zig")),
            Some("zig".to_string())
        );
        assert_eq!(
            lang_for_path(Path::new("build.zig.zon")),
            Some("zon".to_string())
        );
    }

    #[test]
    fn lang_for_path_unknown_extension() {
        use std::path::Path;
        // .xyzlang should not be recognized by syntect.
        assert_eq!(lang_for_path(Path::new("file.xyzlang")), None);
        // No extension at all.
        assert_eq!(lang_for_path(Path::new("Makefile")), None);
    }

    #[test]
    fn wrap_in_code_fence_basic() {
        let src = "fn main() {}\n";
        let wrapped = wrap_in_code_fence(src, "rs");
        assert_eq!(wrapped, "```rs\nfn main() {}\n```\n");
    }

    #[test]
    fn wrap_in_code_fence_no_trailing_newline() {
        let src = "hello";
        let wrapped = wrap_in_code_fence(src, "txt");
        assert_eq!(wrapped, "```txt\nhello\n```\n");
    }

    #[test]
    fn wrap_in_code_fence_bare() {
        let src = "some content\n";
        let wrapped = wrap_in_code_fence(src, "");
        assert_eq!(wrapped, "```\nsome content\n```\n");
    }

    #[test]
    fn highlight_zig_code() {
        let src = "const std = @import(\"std\");\n\npub fn main() !void {\n    std.debug.print(\"hello\\n\", .{});\n}\n";
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code(src, "zig", dark_bg);
        assert!(result.is_some(), "Zig should be recognized");
        let highlighted = result.unwrap();
        assert!(
            highlighted.contains("\x1b["),
            "output should contain ANSI escapes"
        );
    }

    #[test]
    fn highlight_zig_fenced_block() {
        let md = "# Zig Example\n\n```zig\nconst x: u32 = 42;\n```\n";
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code_blocks(md, dark_bg);
        assert!(result.contains("```zig"), "fence should be preserved");
        // Code content should have ANSI escapes.
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```zig"))
            .skip(1)
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "zig code block should be highlighted"
        );
    }

    #[test]
    fn highlight_zon_code() {
        let src = ".{\n    .name = \"my-project\",\n    .version = \"0.1.0\",\n}\n";
        let result = highlight_code(src, "zon", Color::Reset);
        assert!(result.is_some(), "zon should be recognized via Zig syntax");
    }

    #[test]
    fn find_syntax_resolves_zig() {
        let result = find_syntax("zig");
        assert!(result.is_some(), "zig should resolve in custom syntax set");
    }

    #[test]
    fn find_syntax_resolves_typescript() {
        // Token name (lowercased from fence tag)
        assert!(
            find_syntax("typescript").is_some(),
            "typescript should resolve by name"
        );
        // File extension
        assert!(
            find_syntax("ts").is_some(),
            "ts should resolve by extension"
        );
    }

    #[test]
    fn find_syntax_resolves_tsx() {
        assert!(
            find_syntax("tsx").is_some(),
            "tsx should resolve by name/ext"
        );
    }

    #[test]
    fn highlight_typescript_code() {
        let src = concat!(
            "interface User {\n",
            "  name: string;\n",
            "  age: number;\n",
            "}\n",
            "\n",
            "function greet(user: User): string {\n",
            "  return `Hello, ${user.name}`;\n",
            "}\n",
        );
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code(src, "typescript", dark_bg);
        assert!(result.is_some(), "TypeScript should be recognized");
        let highlighted = result.unwrap();
        assert!(
            highlighted.contains("\x1b["),
            "output should contain ANSI escapes"
        );
    }

    #[test]
    fn highlight_typescript_fenced_block() {
        let md = concat!(
            "# Example\n",
            "\n",
            "```typescript\n",
            "const x: number = 42;\n",
            "```\n",
        );
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code_blocks(md, dark_bg);
        assert!(
            result.contains("```typescript"),
            "fence should be preserved"
        );
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```typescript"))
            .skip(1)
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "typescript code block should be highlighted"
        );
    }

    #[test]
    fn highlight_ts_fenced_block() {
        let md = "```ts\nconst x: number = 42;\n```\n";
        let result = highlight_code_blocks(
            md,
            Color::Rgb {
                r: 30,
                g: 30,
                b: 30,
            },
        );
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```ts"))
            .skip(1)
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "ts fence tag should trigger highlighting"
        );
    }

    #[test]
    fn highlight_tsx_code() {
        let src = concat!(
            "interface Props {\n",
            "  name: string;\n",
            "}\n",
            "\n",
            "function App({ name }: Props) {\n",
            "  return <div className=\"app\">{name}</div>;\n",
            "}\n",
        );
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code(src, "tsx", dark_bg);
        assert!(result.is_some(), "TSX should be recognized");
        let highlighted = result.unwrap();
        assert!(
            highlighted.contains("\x1b["),
            "output should contain ANSI escapes"
        );
    }

    #[test]
    fn highlight_tsx_fenced_block() {
        let md = "```tsx\nconst el = <div>hello</div>;\n```\n";
        let result = highlight_code_blocks(
            md,
            Color::Rgb {
                r: 30,
                g: 30,
                b: 30,
            },
        );
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```tsx"))
            .skip(1)
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "tsx code block should be highlighted"
        );
    }

    #[test]
    fn find_syntax_resolves_elixir() {
        assert!(
            find_syntax("elixir").is_some(),
            "elixir should resolve by name"
        );
        assert!(
            find_syntax("ex").is_some(),
            "ex should resolve by extension"
        );
        assert!(
            find_syntax("exs").is_some(),
            "exs should resolve by extension"
        );
    }

    #[test]
    fn highlight_elixir_code() {
        let src = concat!(
            "defmodule Greeter do\n",
            "  @moduledoc \"A simple greeter.\"\n",
            "\n",
            "  def hello(name) do\n",
            "    \"Hello, #{name}!\"\n",
            "  end\n",
            "end\n",
        );
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code(src, "elixir", dark_bg);
        assert!(result.is_some(), "Elixir should be recognized");
        let highlighted = result.unwrap();
        assert!(
            highlighted.contains("\x1b["),
            "output should contain ANSI escapes"
        );
    }

    #[test]
    fn highlight_elixir_fenced_block() {
        let md = concat!(
            "# Example\n",
            "\n",
            "```elixir\n",
            "defmodule Math do\n",
            "  def add(a, b), do: a + b\n",
            "end\n",
            "```\n",
        );
        let dark_bg = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };
        let result = highlight_code_blocks(md, dark_bg);
        assert!(result.contains("```elixir"), "fence should be preserved");
        let code_section: String = result
            .lines()
            .skip_while(|l| !l.contains("```elixir"))
            .skip(1)
            .take_while(|l| !l.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_section.contains("\x1b["),
            "elixir code block should be highlighted"
        );
    }

    #[test]
    fn lang_for_path_elixir_extensions() {
        use std::path::Path;
        assert_eq!(lang_for_path(Path::new("app.ex")), Some("ex".to_string()));
        assert_eq!(
            lang_for_path(Path::new("test_helper.exs")),
            Some("exs".to_string())
        );
    }

    #[test]
    fn lang_for_path_typescript_extensions() {
        use std::path::Path;
        assert_eq!(lang_for_path(Path::new("app.ts")), Some("ts".to_string()));
        assert_eq!(lang_for_path(Path::new("app.tsx")), Some("tsx".to_string()));
        assert_eq!(lang_for_path(Path::new("app.mts")), Some("mts".to_string()));
    }
}
