# Reed Test Document

This file tests **mermaid diagrams**, code blocks, and general markdown rendering.

## Flowchart

A simple flowchart rendered via mmdc:

```mermaid
graph TD
    A[Start] --> B{Is mmdc installed?}
    B -->|Yes| C[Render as image]
    B -->|No| D[Show raw code block]
    C --> E[Display via Kitty protocol]
    D --> E
    E --> F[Done]
```

Some text after the flowchart. This paragraph should appear below the rendered diagram.

## Sequence Diagram

```mermaid
sequenceDiagram
    participant User
    participant Reed
    participant VT as libghostty-vt
    User->>Reed: Open markdown file
    Reed->>Reed: Extract mermaid blocks
    Reed->>Reed: Render to PNG via mmdc
    Reed->>VT: Feed processed markdown
    VT-->>Reed: Cell grid
    Reed-->>User: Display with Kitty images
```

## Regular Code Block

This should remain untouched (not treated as mermaid):

```rust
fn main() {
    println!("Hello from reed!");
}
```

## Mixed Content

- Bullet one
- Bullet two
- Bullet three

> A blockquote to test general rendering.

| Feature   | Status    |
|-----------|-----------|
| Images    | Done      |
| Mermaid   | Testing   |
| Scrolling | Done      |

## Final Notes

If everything works, you should see **two rendered diagrams** above (flowchart and sequence diagram) with text flowing normally around them. If mmdc is not found, the mermaid blocks appear as regular fenced code.
