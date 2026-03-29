use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{RenderState, Terminal};

/// Result of processing one input cycle.
pub enum Action {
    Continue,
    Quit,
    Resize(u16),
}

/// Poll for one input event and update terminal state accordingly.
pub fn poll<'a>(
    term: &mut Terminal<'a, 'a>,
    _render: &mut RenderState<'a>,
    rows: u16,
) -> Result<Action> {
    if !event::poll(Duration::from_millis(16))? {
        return Ok(Action::Continue);
    }

    match event::read()? {
        Event::Key(key) => match (key.code, key.modifiers) {
            // Quit
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return Ok(Action::Quit),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(Action::Quit),

            // Scroll down
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                term.scroll_viewport(ScrollViewport::Delta(1));
            }

            // Scroll up
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                term.scroll_viewport(ScrollViewport::Delta(-1));
            }

            // Page down
            (KeyCode::PageDown, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(rows as isize));
            }

            // Page up
            (KeyCode::PageUp, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(-(rows as isize)));
            }

            // Half-page down / up
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta((rows / 2) as isize));
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                term.scroll_viewport(ScrollViewport::Delta(-((rows / 2) as isize)));
            }

            // Top / bottom
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => {
                term.scroll_viewport(ScrollViewport::Top);
            }
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => {
                term.scroll_viewport(ScrollViewport::Bottom);
            }

            // Space = page down (like less/man)
            (KeyCode::Char(' '), _) => {
                term.scroll_viewport(ScrollViewport::Delta(rows as isize));
            }

            _ => {}
        },

        Event::Resize(new_cols, new_rows) => {
            term.resize(new_cols, new_rows, 0, 0)?;
            return Ok(Action::Resize(new_rows));
        }

        _ => {}
    }

    Ok(Action::Continue)
}
