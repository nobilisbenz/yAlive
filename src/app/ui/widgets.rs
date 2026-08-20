//! The primitives every page is built from.
//!
//! The old interface drew a bordered `Block` around every panel: four columns
//! and two rows spent per panel, nested two deep, on a 100-column terminal.
//! Structure here comes from alignment and space instead, so the only borders
//! left are on overlays — the one case where a floating surface genuinely has
//! to separate itself from the content underneath.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding};

use super::super::theme;
use super::super::util::{fit, pad};

/// Horizontal breathing room, applied once at the page level.
pub const GUTTER: u16 = 2;

/// Split a page into a left list and a right detail pane, with a gap between
/// them rather than two facing borders.
pub fn split_detail(area: Rect, left_percent: u16) -> (Rect, Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_percent),
            Constraint::Length(GUTTER),
            Constraint::Min(0),
        ])
        .split(area);
    (columns[0], columns[2])
}

/// A quiet all-caps group label. Structure without a box and without colour.
pub fn heading(text: &str) -> Line<'static> {
    Line::styled(text.to_uppercase(), theme::heading())
}

/// A label/value row, where the value is truncated to whatever space is left
/// rather than being clipped by the terminal.
pub fn kv(label: &str, value: &str, width: usize) -> Line<'static> {
    let label_width = 9usize.min(width.saturating_sub(1));
    let value_width = width.saturating_sub(label_width);
    Line::from(vec![
        Span::styled(pad(label, label_width), theme::faint()),
        Span::raw(fit(value, value_width)),
    ])
}

/// A row whose value sits hard against the right edge — used wherever a name
/// and a number share a line and the number is the thing being compared.
pub fn spread(left: &str, right: &str, width: usize, right_style: Style) -> Line<'static> {
    let right = fit(right, width.saturating_sub(2));
    let right_width = right.chars().count();
    let left_width = width.saturating_sub(right_width).saturating_sub(1);
    Line::from(vec![
        Span::raw(pad(left, left_width)),
        Span::raw(" "),
        Span::styled(right, right_style),
    ])
}

/// A horizontal rule, used to separate chrome from content.
pub fn rule(width: usize) -> Line<'static> {
    Line::styled("─".repeat(width), theme::faint())
}

/// A proportional bar. Filled in the accent, empty in the faintest grey, so a
/// row of them reads as a shape before it reads as numbers.
pub fn meter(value: usize, maximum: usize, width: usize) -> Vec<Span<'static>> {
    let maximum = maximum.max(1);
    let filled = if value == 0 {
        0
    } else {
        (value * width).div_ceil(maximum).min(width)
    };
    vec![
        Span::styled("█".repeat(filled), theme::accent()),
        Span::styled("·".repeat(width.saturating_sub(filled)), theme::faint()),
    ]
}

/// The standard list: an accent bar for the cursor, no highlight block.
///
/// A list that does not hold focus still shows where its cursor is, but in grey
/// — otherwise moving focus between panes loses the reader's place entirely.
pub fn list(items: Vec<ListItem<'static>>, focused: bool) -> List<'static> {
    List::new(items)
        .highlight_style(if focused {
            theme::selected()
        } else {
            theme::selected_blurred()
        })
        .highlight_symbol(if focused { concat!("▍", " ") } else { "  " })
}

/// State for a list, guarding against a cursor left past the end of a list that
/// shrank underneath it.
pub fn list_state(selected: Option<usize>, len: usize) -> ListState {
    ListState::default().with_selected(match selected {
        Some(index) if len > 0 => Some(index.min(len - 1)),
        _ => None,
    })
}

/// A pane title. The only thing marking focus, now that borders are gone.
///
/// Left in the case it was given: these titles are note titles, section
/// headings, and section UIDs, and shouting `LINEAR#ROOT` at the reader makes
/// an identifier harder to read, not easier. Small all-caps labels are for
/// group headings, which is what [`heading`] is for.
pub fn pane_title(text: &str, focused: bool, width: usize) -> Vec<Line<'static>> {
    let style = if focused {
        theme::accent()
    } else {
        theme::heading()
    };
    vec![
        Line::styled(fit(text, width), style),
        Line::styled(
            if focused {
                "─".repeat(text.chars().count().min(width))
            } else {
                String::new()
            },
            theme::accent(),
        ),
    ]
}

/// The block used by overlays — the palette, the deck chooser, help.
///
/// Overlays float above the page, so unlike a pane they do need an edge.
pub fn overlay_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Line::styled(format!(" {title} "), theme::accent()))
        .borders(Borders::ALL)
        .border_style(theme::faint())
        .padding(Padding::new(1, 1, 0, 0))
}

/// Centre a rectangle of at most `width` × `height` inside `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

/// The message shown where a list would be, when the list is empty.
///
/// Every empty state says what would appear here and which key puts it there —
/// an empty pane that only says "nothing" wastes the best teaching moment the
/// interface gets.
pub fn empty_state(message: &str, hint: &str) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        Line::styled(message.to_string(), theme::dim()),
        Line::raw(""),
        Line::styled(hint.to_string(), theme::faint()),
    ]
}
