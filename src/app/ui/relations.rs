//! The Relations page: what points here, what this is, and what it points at.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::super::util::fit;
use super::super::{App, theme};
use super::list_pane;
use super::widgets;
use crate::model::RelationRow;

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(28),
            Constraint::Length(widgets::GUTTER),
            Constraint::Min(0),
            Constraint::Length(widgets::GUTTER),
            Constraint::Percentage(28),
        ])
        .split(area);

    let incoming = app.incoming_relations();
    let outgoing = app.outgoing_relations();

    list_pane(
        frame,
        columns[0],
        "Incoming",
        app.focused_panel == 0,
        relation_rows(&incoming, columns[0].width, "Nothing links here yet"),
        (!incoming.is_empty()).then_some(app.incoming_selected),
    );

    draw_sections(app, frame, columns[2]);

    list_pane(
        frame,
        columns[4],
        "Outgoing",
        app.focused_panel == 2,
        relation_rows(&outgoing, columns[4].width, "This section links nowhere"),
        (!outgoing.is_empty()).then_some(app.outgoing_selected),
    );
}

/// One row per relation.
///
/// This used to be three stacked lines per relation — type, heading, uid — so a
/// pane eight rows tall held two relations and read as a list of duplicates.
/// The heading is what you navigate by; the type is context, so it sits at the
/// right edge where it can be scanned down the column.
fn relation_rows(
    relations: &[&RelationRow],
    width: u16,
    empty: &'static str,
) -> Vec<ListItem<'static>> {
    if relations.is_empty() {
        return vec![ListItem::new(Line::styled(empty, theme::faint()))];
    }
    let width = width.saturating_sub(2) as usize;
    relations
        .iter()
        .map(|relation| {
            ListItem::new(widgets::spread(
                relation
                    .target_heading
                    .as_deref()
                    .unwrap_or(&relation.target_uid),
                &relation.relation_type.to_lowercase(),
                width,
                theme::faint(),
            ))
        })
        .collect()
}

/// The section list in the middle, one row per section.
///
/// The note title is only repeated when it differs from the heading — a note's
/// root section carries the note's own title, and printing "Linear Algebra /
/// Linear Algebra" on every one of them taught the reader to skip the column.
fn draw_sections(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let rows: Vec<ListItem<'static>> = app
        .sections
        .iter()
        .map(|section| {
            if section.note_title == section.heading {
                ListItem::new(Line::styled(fit(&section.heading, width), theme::text()))
            } else {
                ListItem::new(widgets::spread(
                    &section.heading,
                    &section.note_title,
                    width,
                    theme::faint(),
                ))
            }
        })
        .collect();

    let rows = if rows.is_empty() {
        vec![ListItem::new(Line::styled(
            "No sections indexed",
            theme::faint(),
        ))]
    } else {
        rows
    };

    let title = app
        .sections
        .get(app.relation_section)
        .map(|section| section.uid.clone())
        .unwrap_or_else(|| "Sections".into());

    list_pane(
        frame,
        area,
        &title,
        app.focused_panel == 1,
        rows,
        (!app.sections.is_empty()).then_some(app.relation_section),
    );
}

/// Used by the search page, which shows the same section rows.
pub fn section_row(note_title: &str, heading: &str, width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(fit(heading, width * 2 / 3), theme::text()),
        Span::raw("  "),
        Span::styled(fit(note_title, width / 3), theme::faint()),
    ])
}
