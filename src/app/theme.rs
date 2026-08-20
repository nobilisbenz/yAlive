//! How the terminal interface spends the ecosystem's tokens.
//!
//! The values themselves live in [`super::tokens`], generated from
//! `assets/design/tokens.json` — the same source yGraphy, yClippy, and yReviewy
//! read. This module decides what the terminal *does* with them, which is a
//! separate question from what they are.
//!
//! Two rules the terminal adds on top of the shared system:
//!
//! 1. **Body text is not a token.** A TUI is a guest in someone else's colour
//!    scheme. Painting `--text` over their foreground makes yalive the one
//!    window that ignores the theme they chose, so body text is left at the
//!    terminal's own default and only the supporting weights are token greys.
//! 2. **The accent is the only hue spent.** yalive's is `#168bff`, the blue at
//!    the centre of its icon. Colour therefore always means something: focus,
//!    selection, or a state worth reacting to.

use ratatui::style::{Color, Modifier, Style};

use super::tokens;

const fn colour(value: tokens::Rgb) -> Color {
    Color::Rgb(value.0, value.1, value.2)
}

/// yalive blue. Focus, selection, and the active tab — nothing else.
pub const ACCENT: Color = colour(tokens::ACCENT);
/// Secondary text: labels, metadata, captions.
pub const DIM: Color = colour(tokens::TEXT_DIM);
/// Tertiary text: rules, empty-state filler, inactive tabs.
pub const FAINT: Color = colour(tokens::TEXT_FAINT);
/// Something needs attention but nothing is broken.
pub const WARN: Color = colour(tokens::WARNING);
/// A thing succeeded, or an answer was right.
pub const OK: Color = colour(tokens::SUCCESS);
/// A thing failed, or an answer was wrong.
pub const ERR: Color = colour(tokens::DANGER);

/// Body text, in whatever colour the terminal already uses.
pub fn text() -> Style {
    Style::default()
}

/// The one line on screen the eye should land on first.
pub fn strong() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Labels and metadata that support the primary text without competing.
pub fn dim() -> Style {
    Style::default().fg(DIM)
}

/// Chrome that should be legible but never noticed.
pub fn faint() -> Style {
    Style::default().fg(FAINT)
}

/// The accent, for the selected row's marker and the active tab.
pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

/// A section heading: small, quiet, and set in caps rather than in colour.
pub fn heading() -> Style {
    Style::default().fg(FAINT).add_modifier(Modifier::BOLD)
}

/// The selected row. A bold foreground and an accent bar, deliberately not a
/// filled background — a highlight block is the single loudest thing a TUI can
/// draw, and it fights every other signal on the page.
pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// The selected row while its panel does not hold focus.
pub fn selected_blurred() -> Style {
    Style::default().fg(DIM)
}

pub fn ok() -> Style {
    Style::default().fg(OK)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn err() -> Style {
    Style::default().fg(ERR)
}

/// The marker drawn to the left of the selected row.
pub const CURSOR: &str = "▍";
/// Same width as [`CURSOR`], for every row that is not selected.
pub const NO_CURSOR: &str = " ";

#[cfg(test)]
mod tests {
    use super::*;

    /// The interface must keep spending exactly one hue. A second accent added
    /// to a page module would show up here.
    #[test]
    fn the_accent_is_the_icons_blue() {
        assert_eq!(ACCENT, Color::Rgb(0x16, 0x8b, 0xff));
    }

    /// Body text deliberately has no colour: the terminal's own foreground is
    /// the right answer, and overriding it is what makes a TUI feel foreign.
    #[test]
    fn body_text_defers_to_the_terminal() {
        assert_eq!(text().fg, None);
        assert_eq!(strong().fg, None);
    }
}
