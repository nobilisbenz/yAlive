//! The Archived page: what was removed from view, and how to bring it back.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::ListItem;

use super::super::util::fit;
use super::super::{App, theme};
use super::widgets;
use super::{detail_pane, list_pane};
use crate::model::ArchivedItem;

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (left, right) = widgets::split_detail(area, 44);

    let width = left.width.saturating_sub(2) as usize;
    let rows: Vec<ListItem<'static>> = app
        .archived
        .iter()
        .map(|item| {
            let (kind, label) = describe(item);
            ListItem::new(widgets::spread(label, kind, width, theme::faint()))
        })
        .collect();

    let rows = if rows.is_empty() {
        vec![ListItem::new(Line::styled(
            "Nothing is archived",
            theme::faint(),
        ))]
    } else {
        rows
    };

    list_pane(
        frame,
        left,
        "",
        app.focused_panel == 0,
        rows,
        (!app.archived.is_empty()).then_some(app.selected),
    );

    let inner_width = right.width as usize;
    let (title, lines) = match app.archived.get(app.selected) {
        None => (
            "Archive".to_string(),
            widgets::empty_state(
                "The archive is empty.",
                "x archives the selected item on Library, Review, or Clean.",
            ),
        ),
        Some(ArchivedItem::Note {
            title,
            path,
            section_count,
            quiz_count,
            ..
        }) => (
            title.clone(),
            vec![
                Line::styled(fit(&path.display().to_string(), inner_width), theme::dim()),
                Line::raw(""),
                Line::styled(
                    format!("{section_count} sections and {quiz_count} quizzes are hidden with it"),
                    theme::faint(),
                ),
            ],
        ),
        Some(ArchivedItem::Section {
            note_title,
            heading,
            quiz_count,
            ..
        }) => (
            heading.clone(),
            vec![
                Line::styled(fit(note_title, inner_width), theme::dim()),
                Line::raw(""),
                Line::styled(
                    format!("{quiz_count} quizzes are hidden with this section"),
                    theme::faint(),
                ),
            ],
        ),
        Some(ArchivedItem::Quiz {
            label, card_count, ..
        }) => (
            label.clone(),
            vec![Line::styled(
                format!("{card_count} card variants retained"),
                theme::faint(),
            )],
        ),
        Some(ArchivedItem::Deck {
            name, quiz_count, ..
        }) => (
            name.clone(),
            vec![
                Line::styled(format!("{quiz_count} quizzes assigned"), theme::dim()),
                Line::raw(""),
                Line::styled(
                    "Quizzes only in this deck are hidden; those shared with an active deck stay active.",
                    theme::faint(),
                ),
            ],
        ),
    };
    detail_pane(frame, right, &title, app.focused_panel == 1, lines);
}

fn describe(item: &ArchivedItem) -> (&'static str, &str) {
    match item {
        ArchivedItem::Note { title, .. } => ("note", title.as_str()),
        ArchivedItem::Section { heading, .. } => ("section", heading.as_str()),
        ArchivedItem::Quiz { label, .. } => ("quiz", label.as_str()),
        ArchivedItem::Deck { name, .. } => ("deck", name.as_str()),
    }
}
