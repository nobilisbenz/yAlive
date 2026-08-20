//! Full-text search: the query, the hits, and a preview of the selected hit.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph};

use super::super::util::display_markdown;
use super::super::{App, theme};
use super::widgets;
use super::{detail_pane, plain_list};

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Search  ", theme::heading()),
                Span::raw(app.query.clone()),
                Span::styled("▏", theme::accent()),
            ]),
            Line::styled(
                match app.sections.len() {
                    0 if app.query.is_empty() => "Type to search every section".to_string(),
                    0 => "No section matches".to_string(),
                    1 => "1 section".to_string(),
                    count => format!("{count} sections"),
                },
                theme::faint(),
            ),
        ]),
        rows[0],
    );

    let (left, right) = widgets::split_detail(rows[1], 38);
    let width = left.width.saturating_sub(2) as usize;
    let items: Vec<ListItem<'static>> = app
        .sections
        .iter()
        .map(|section| {
            ListItem::new(super::relations::section_row(
                &section.note_title,
                &section.heading,
                width,
            ))
        })
        .collect();
    plain_list(
        frame,
        left,
        items,
        app.focused_panel == 0,
        (!app.sections.is_empty()).then_some(app.selected),
    );

    let (title, lines) = app.sections.get(app.selected).map_or_else(
        || {
            (
                "No match".to_string(),
                widgets::empty_state("Nothing matches that query.", "esc returns to the library."),
            )
        },
        |section| {
            (
                section.heading.clone(),
                display_markdown(&section.body)
                    .lines()
                    .map(|line| Line::raw(line.to_string()))
                    .collect(),
            )
        },
    );
    detail_pane(frame, right, &title, app.focused_panel == 1, lines);
}
