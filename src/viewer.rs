use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};
use crossterm::{
    cursor, execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use termimad::MadSkin;

use crate::input;

/// Print rendered markdown to stdout (non-interactive, no TTY required).
pub fn print_to_stdout(markdown: &str) -> Result<()> {
    let skin = MadSkin::default();
    let width = terminal::size().map(|(c, _)| c as usize).unwrap_or(80);
    let rendered = skin.text(markdown, Some(width));
    print!("{rendered}");
    Ok(())
}

/// Run the interactive markdown viewer loop.
/// Falls back to print mode if no TTY is available.
pub fn run(markdown: &str, max_scrollback: usize) -> Result<()> {
    if !io::stdout().is_terminal() {
        return print_to_stdout(markdown);
    }

    let (cols, rows) = terminal::size().context("no terminal available")?;
    if cols == 0 || rows == 0 {
        return print_to_stdout(markdown);
    }

    // Render markdown → ANSI-styled text.
    let skin = MadSkin::default();
    let ansi_text = skin.text(markdown, Some(cols as usize)).to_string();

    // Create the virtual terminal and feed rendered content.
    let mut term = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback,
    })
    .context("failed to create libghostty-vt terminal")?;
    term.vt_write(ansi_text.as_bytes());

    // Allocate render iterators (reused every frame).
    let mut render_state = RenderState::new().context("failed to create render state")?;
    let mut row_it = RowIterator::new().context("failed to create row iterator")?;
    let mut cell_it = CellIterator::new().context("failed to create cell iterator")?;

    // Enter raw mode / alternate screen.
    terminal::enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();

    let result = run_loop(
        &mut term,
        &mut render_state,
        &mut row_it,
        &mut cell_it,
        &mut stdout,
        rows,
    );

    // Always restore terminal, even on error.
    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    result
}

fn run_loop<'a>(
    term: &mut Terminal<'a, 'a>,
    render_state: &mut RenderState<'a>,
    row_it: &mut RowIterator<'a>,
    cell_it: &mut CellIterator<'a>,
    stdout: &mut io::Stdout,
    mut rows: u16,
) -> Result<()> {
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        cursor::Hide,
        terminal::Clear(ClearType::All)
    )?;

    loop {
        // Render: snapshot the terminal state and draw every row/cell.
        // Following ghostling_rs pattern: render unconditionally, skip dirty tracking.
        {
            let snapshot = render_state.update(term)?;
            let colors = snapshot.colors()?;

            let mut row_iter = row_it.update(&snapshot)?;
            let mut screen_row: u16 = 0;

            while let Some(row) = row_iter.next() {
                if screen_row >= rows {
                    break;
                }

                queue!(stdout, cursor::MoveTo(0, screen_row))?;

                let mut cell_iter = cell_it.update(row)?;
                while let Some(cell) = cell_iter.next() {
                    let graphemes: Vec<char> = cell.graphemes()?;
                    let style = cell.style()?;

                    let fg = cell.fg_color()?.unwrap_or(colors.foreground);
                    let bg = cell.bg_color()?.unwrap_or(colors.background);

                    let (draw_fg, draw_bg) = if style.inverse {
                        (rgb_to_color(bg), rgb_to_color(fg))
                    } else {
                        (rgb_to_color(fg), rgb_to_color(bg))
                    };

                    queue!(
                        stdout,
                        SetForegroundColor(draw_fg),
                        SetBackgroundColor(draw_bg),
                    )?;

                    if graphemes.is_empty() {
                        queue!(stdout, Print(' '))?;
                    } else {
                        let text: String = graphemes.into_iter().collect();
                        queue!(stdout, Print(&text))?;
                    }
                }

                queue!(stdout, ResetColor)?;
                screen_row += 1;
            }

            stdout.flush()?;
        }
        // snapshot dropped here — render_state is free for input::poll

        match input::poll(term, render_state, rows)? {
            input::Action::Continue => {}
            input::Action::Quit => break,
            input::Action::Resize(new_rows) => {
                rows = new_rows;
            }
        }
    }

    Ok(())
}

fn rgb_to_color(rgb: libghostty_vt::style::RgbColor) -> Color {
    Color::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}
