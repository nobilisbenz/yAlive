//! The Clean page: everything in the vault that has no home yet.

use std::fs;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::ListItem;

use super::super::util::fit;
use super::super::{App, CleanItem, theme};
use super::widgets;
use super::{detail_pane, list_pane};

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (left, right) = widgets::split_detail(area, 44);
    draw_queue(app, frame, left);
    draw_detail(app, frame, right);
}

fn draw_queue(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let items = app.clean_items();
    let mut rows: Vec<ListItem<'static>> = Vec::new();
    let mut selected_row = None;
    let mut seen = [false; 3];

    for (index, item) in items.iter().enumerate() {
        let (group, text) = match item {
            CleanItem::Note(note) => (0, app.notes[*note].title.clone()),
            CleanItem::Card(card) => (1, app.cards[*card].label.clone()),
            CleanItem::Image(image) => (2, app.orphan_images[*image].display().to_string()),
        };
        if !seen[group] {
            if group > 0 && !rows.is_empty() {
                rows.push(ListItem::new(Line::raw("")));
            }
            rows.push(ListItem::new(widgets::heading(
                [
                    "Notes without topics",
                    "Cards without decks",
                    "Unused images",
                ][group],
            )));
            seen[group] = true;
        }
        if index == app.selected {
            selected_row = Some(rows.len());
        }
        rows.push(ListItem::new(Line::raw(fit(&text, width))));
    }

    if rows.is_empty() {
        rows.push(ListItem::new(Line::styled(
            "Everything has a home",
            theme::ok(),
        )));
    }

    list_pane(frame, area, "", app.focused_panel == 0, rows, selected_row);
}

fn draw_detail(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    let active = app
        .decks
        .get(app.active_deck)
        .map_or("no deck", |deck| deck.name.as_str());

    let (title, lines) = match app.clean_items().get(app.selected) {
        Some(CleanItem::Note(index)) => (
            app.notes[*index].title.clone(),
            vec![
                Line::styled("This note has no topic.", theme::warn()),
                Line::raw(""),
                Line::styled("Add one to its front matter:", theme::dim()),
                Line::styled("topic: Your topic", theme::accent()),
            ],
        ),
        Some(CleanItem::Card(index)) => (
            app.cards[*index].label.clone(),
            vec![
                Line::styled("This card belongs to no deck.", theme::warn()),
                Line::raw(""),
                widgets::kv("active", active, width),
                Line::raw(""),
                Line::styled(
                    "a assigns it to the active deck; change that deck on the Review page with [ and ].",
                    theme::faint(),
                ),
            ],
        ),
        Some(CleanItem::Image(index)) => {
            let path = &app.orphan_images[*index];
            let size = fs::metadata(app.vault.join(path))
                .map(|metadata| human_size(metadata.len()))
                .unwrap_or_else(|_| "unknown".into());
            (
                path.display().to_string(),
                vec![
                    Line::styled("No Markdown link points at this file.", theme::warn()),
                    Line::raw(""),
                    widgets::kv("size", &size, width),
                    Line::raw(""),
                    Line::styled("d deletes it permanently.", theme::err()),
                ],
            )
        }
        None => (
            "Vault is clean".into(),
            widgets::empty_state(
                "Every note has a topic, every card has a deck, and every image is referenced.",
                "Nothing to do here.",
            ),
        ),
    };
    detail_pane(frame, area, &title, app.focused_panel == 1, lines);
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
