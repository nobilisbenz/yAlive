//! The one place colour is decided.
//!
//! yalive's icon is a black disc with a blue centre (`#168bff`), and that blue
//! is the only hue the interface spends. Everything else is the terminal's own
//! foreground plus two greys, so the TUI sits inside whatever colour scheme the
//! user already runs instead of fighting it. Colour therefore always *means*
//! something — focus, selection, or a state worth reacting to.
//!
//! The greys are the same values yClippy uses for `--text-dim` and
//! `--text-faint`, so the desktop app and the terminal read as one product.

use ratatui::style::{Color, Modifier, Style};

/// yalive blue. Focus, selection, and the active tab — nothing else.
pub const ACCENT: Color = Color::Rgb(0x16, 0x8b, 0xff);
/// Secondary text: labels, metadata, captions.
pub const DIM: Color = Color::Rgb(0xa1, 0xa1, 0xaa);
/// Tertiary text: rules, empty-state filler, inactive tabs.
pub const FAINT: Color = Color::Rgb(0x52, 0x52, 0x5b);
/// Something needs attention but nothing is broken.
pub const WARN: Color = Color::Rgb(0xf5, 0x9e, 0x0b);
/// A thing succeeded, or an answer was right.
pub const OK: Color = Color::Rgb(0x22, 0xc5, 0x5e);
/// A thing failed, or an answer was wrong.
pub const ERR: Color = Color::Rgb(0xef, 0x44, 0x44);

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
