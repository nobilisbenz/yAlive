//! The Library page: every note and its sections, with a preview beside it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::super::util::{display_markdown, fit, short_date};
use super::super::{App, LibraryItem, theme};
use super::widgets;
use super::{detail_pane, list_pane};

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (left, right) = widgets::split_detail(area, 38);
    draw_tree(app, frame, left);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(8)])
        .split(right);
    draw_detail(app, frame, rows[0]);
    draw_recent(app, frame, rows[1]);
}

fn draw_tree(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let items = app.library_items();
    let mut rows: Vec<ListItem<'static>> = Vec::new();
    let mut selected_row = None;
    let mut last_topic: Option<String> = None;

    for (index, item) in items.iter().enumerate() {
        let row = match item {
            LibraryItem::Note(note_index) => {
                let note = &app.notes[*note_index];
                let topic = note.topic.clone().unwrap_or_else(|| "No topic".into());
                if last_topic.as_deref() != Some(topic.as_str()) {
                    if last_topic.is_some() {
                        rows.push(ListItem::new(Line::raw("")));
                    }
                    rows.push(ListItem::new(widgets::heading(&topic)));
                    last_topic = Some(topic);
                }
                ListItem::new(Line::from(vec![
                    Span::styled(if note.pinned { "★ " } else { "  " }, theme::accent()),
                    Span::styled(fit(&note.title, width.saturating_sub(2)), theme::strong()),
                ]))
            }
            LibraryItem::Section(section_index) => ListItem::new(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    fit(
                        &app.sections[*section_index].heading,
                        width.saturating_sub(4),
                    ),
                    theme::dim(),
                ),
            ])),
        };
        if index == app.selected {
            selected_row = Some(rows.len());
        }
        rows.push(row);
    }

    if rows.is_empty() {
        rows.push(ListItem::new(Line::styled("No notes yet", theme::dim())));
        rows.push(ListItem::new(Line::styled(
            "Press n to write the first one",
            theme::faint(),
        )));
    }

    list_pane(frame, area, "", app.focused_panel == 0, rows, selected_row);
}

fn draw_detail(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    let (title, lines) = match app.library_items().get(app.selected) {
        Some(LibraryItem::Note(index)) => {
            let Some(note) = app.notes.get(*index) else {
                return;
            };
            let sections: Vec<_> = app
                .sections
                .iter()
                .filter(|section| section.path == note.path)
                .collect();
            // Same reasoning as `library_items`: the root section's heading is
            // the note's title, which is already the pane's own title.
            let listed: Vec<_> = sections
                .iter()
                .enumerate()
                .filter(|(index, section)| !(*index == 0 && section.heading == note.title))
                .map(|(_, section)| *section)
                .collect();
            let mut meta = vec![
                note.topic.clone().unwrap_or_else(|| "No topic".into()),
                if sections.len() == 1 {
                    "1 section".to_string()
                } else {
                    format!("{} sections", sections.len())
                },
                note.path.display().to_string(),
            ];
            if note.pinned {
                meta.push("pinned".into());
            }
            let mut lines = vec![
                Line::styled(fit(&meta.join("  ·  "), width), theme::dim()),
                Line::styled(
                    fit(
                        &format!(
                            "edited {}  ·  created {}",
                            short_date(note.modified_at),
                            short_date(note.created_at)
                        ),
                        width,
                    ),
                    theme::faint(),
                ),
            ];
            // A note whose only section is its root has nothing to list here,
            // and an empty heading is worse than no heading.
            if !listed.is_empty() {
                lines.push(Line::raw(""));
                lines.push(widgets::heading("Sections"));
                lines.extend(listed.iter().map(|section| {
                    Line::raw(format!(
                        "  {}",
                        fit(&section.heading, width.saturating_sub(2))
                    ))
                }));
            }
            (note.title.clone(), lines)
        }
        Some(LibraryItem::Section(index)) => {
            let section = &app.sections[*index];
            let mut lines = vec![
                Line::styled(
                    fit(
                        &format!("{}  ·  line {}", section.note_title, section.start_line),
                        width,
                    ),
                    theme::dim(),
                ),
                Line::raw(""),
            ];
            lines.extend(
                display_markdown(&section.body)
                    .lines()
                    .map(|line| Line::raw(line.to_string())),
            );
            (section.heading.clone(), lines)
        }
        None => (
            "Nothing selected".into(),
            widgets::empty_state(
                "This vault has no notes yet.",
                "n creates one and opens your editor.",
            ),
        ),
    };
    detail_pane(frame, area, &title, app.focused_panel == 1, lines);
}

/// Pinned notes and this week's edits, in one pane rather than two.
///
/// They were separate panels that each spent a border on a handful of rows and
/// truncated their contents mid-word to fit.
fn draw_recent(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    let week_ago = chrono::Utc::now().timestamp() - 7 * 86_400;
    let mut lines = Vec::new();

    let pinned: Vec<_> = app.notes.iter().filter(|note| note.pinned).collect();
    if !pinned.is_empty() {
        lines.push(widgets::heading("Pinned"));
        lines.extend(pinned.iter().take(3).map(|note| {
            widgets::spread(
                &format!("★ {}", note.title),
                note.topic.as_deref().unwrap_or("No topic"),
                width,
                theme::faint(),
            )
        }));
        lines.push(Line::raw(""));
    }

    let recent: Vec<_> = app
        .notes
        .iter()
        .filter(|note| note.modified_at >= week_ago)
        .collect();
    lines.push(widgets::heading("Edited this week"));
    if recent.is_empty() {
        lines.push(Line::styled("Nothing yet", theme::faint()));
    } else {
        lines.extend(recent.iter().take(3).map(|note| {
            widgets::spread(
                &note.title,
                &short_date(note.modified_at),
                width,
                theme::faint(),
            )
        }));
    }

    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}
