//! The Options page: scheduling, storage, and GitHub sync.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::ListItem;

use super::super::util::fit;
use super::super::{App, theme};
use super::widgets;
use super::{detail_pane, list_pane};
use crate::config::ReviewOrder;

/// Label, value, and the explanation shown beside it.
///
/// Values were previously written into a fixed 28-column label field and then
/// clipped by whatever width the panel happened to have, so "Sync now" read
/// "Set repository firs". The value is right-aligned against the real pane
/// width now, and truncates with an ellipsis when it genuinely does not fit.
pub fn rows(app: &App) -> Vec<(&'static str, String, &'static str)> {
    let order = match app.config.review_order {
        ReviewOrder::Due => "Due first",
        ReviewOrder::Random => "Random",
    };
    vec![
        (
            "Desired retention",
            format!("{:.0}%", app.config.desired_retention * 100.0),
            "FSRS aims for this recall probability when it picks the next interval. Higher means shorter intervals and more reviews.",
        ),
        (
            "New cards per day",
            app.config.new_cards_per_day.to_string(),
            "Caps cards you have never seen. Cards introduced earlier today count against it.",
        ),
        (
            "Maximum reviews per day",
            app.config.max_reviews_per_day.to_string(),
            "Caps the whole daily workload, including reviews already done today.",
        ),
        (
            "Review order",
            order.into(),
            "Due first clears the oldest scheduled cards. Random shuffles the queue.",
        ),
        (
            "Bury sibling cards",
            if app.config.bury_siblings {
                "On"
            } else {
                "Off"
            }
            .into(),
            "Shows at most one due card per section each session, so related cards stop giving each other away.",
        ),
        (
            "GitHub authentication",
            "Run".into(),
            "Signs in through GitHub CLI and configures Git's credential helper. SSH users can skip this. yalive never sees or stores a token.",
        ),
        (
            "Repository URL",
            app.sync_remote
                .clone()
                .unwrap_or_else(|| "Not configured".into()),
            "The repository this vault syncs with. Use the SSH form, or https:// after signing in. URLs containing tokens are rejected.",
        ),
        (
            "Sync now",
            if app.sync_remote.is_some() {
                "Run".into()
            } else {
                "Set a repository first".to_string()
            },
            "Commits local changes, fetches and integrates the remote, then pushes. Conflicts stop safely. The SQLite index is never uploaded.",
        ),
        (
            "Open another vault",
            "Run".into(),
            "Closes this vault and opens an existing one, remembering it as the default.",
        ),
        (
            "Create new vault",
            "Run".into(),
            "Creates the directory when needed, initialises .notes, and remembers it as the default.",
        ),
    ]
}

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (left, right) = widgets::split_detail(area, 48);
    let rows = rows(app);
    let width = left.width.saturating_sub(2) as usize;

    let items: Vec<ListItem<'static>> = rows
        .iter()
        .map(|(label, value, _)| {
            ListItem::new(widgets::spread(label, value, width, theme::accent()))
        })
        .collect();

    list_pane(
        frame,
        left,
        "",
        app.focused_panel == 0,
        items,
        Some(app.selected),
    );

    let index = app.selected.min(rows.len() - 1);
    let (label, _, description) = &rows[index];
    let inner = right.width as usize;
    let mut lines: Vec<Line<'static>> = description
        .split_terminator(". ")
        .map(|sentence| {
            Line::raw(if sentence.ends_with('.') {
                sentence.to_string()
            } else {
                format!("{sentence}.")
            })
        })
        .collect();

    lines.extend([
        Line::raw(""),
        widgets::heading("Stored in"),
        Line::styled(".notes/config.toml", theme::dim()),
        Line::styled("Git credentials: system credential helper", theme::faint()),
        Line::raw(""),
        widgets::heading("Environment"),
        widgets::kv(
            "editor",
            app.config
                .editor
                .as_deref()
                .unwrap_or("$VISUAL / $EDITOR / nvim"),
            inner,
        ),
        widgets::kv(
            "reindex",
            &format!("{} ms", app.config.reindex_interval_ms),
            inner,
        ),
        // The resolved template, not the configured one: a configured player
        // that is not installed falls back, and the reader should see what will
        // actually run.
        widgets::kv(
            "player",
            &crate::player::resolve(app.config.player.as_deref()).join(" "),
            inner,
        ),
    ]);

    detail_pane(
        frame,
        right,
        &fit(label, inner),
        app.focused_panel == 1,
        lines,
    );
}
