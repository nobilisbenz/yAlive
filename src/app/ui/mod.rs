//! The render layer: chrome, page dispatch, and overlays.

pub mod archived;
pub mod clean;
pub mod library;
pub mod options;
pub mod relations;
pub mod review;
pub mod search;
pub mod stats;
pub mod widgets;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, ListItem, Paragraph, Wrap};

use super::keymap;
use super::util::fit;
use super::{App, Mode, Page, palette, theme};
use widgets::GUTTER;

/// The four pages that earn permanent chrome, in tab order.
pub const TABS: [(Page, &str); 4] = [
    (Page::Library, "Library"),
    (Page::Review, "Review"),
    (Page::Relations, "Relations"),
    (Page::Stats, "Stats"),
];

pub fn draw(app: &App, frame: &mut Frame<'_>) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Length(1), // active-tab underline
            Constraint::Length(1), // breathing room
            Constraint::Min(1),    // page
            Constraint::Length(1), // rule
            Constraint::Length(1), // footer
        ])
        .split(frame.area());

    draw_tabs(app, frame, areas[0], areas[1]);

    let content = gutter(areas[3]);
    match app.mode {
        Mode::Review => review::draw_session(app, frame, content),
        Mode::Search => search::draw(app, frame, content),
        _ => match app.page {
            Page::Library => library::draw(app, frame, content),
            Page::Review => review::draw_page(app, frame, content),
            Page::Relations => relations::draw(app, frame, content),
            Page::Stats => stats::draw(app, frame, content),
            Page::Clean => clean::draw(app, frame, content),
            Page::Options => options::draw(app, frame, content),
            Page::Archived => archived::draw(app, frame, content),
        },
    }

    frame.render_widget(
        Paragraph::new(widgets::rule(areas[4].width as usize)),
        areas[4],
    );
    draw_footer(app, frame, gutter(areas[5]));

    match app.mode {
        Mode::Palette => draw_palette(app, frame, areas[3]),
        Mode::Help => draw_help(app, frame, areas[3]),
        Mode::ReviewDeckChoice => review::draw_deck_choice(app, frame, areas[3]),
        Mode::DeckInput | Mode::NoteInput | Mode::VaultInput | Mode::GitRemoteInput => {
            draw_prompt(app, frame, areas[3])
        }
        _ => {}
    }
}

/// Inset an area by the page gutter.
fn gutter(area: Rect) -> Rect {
    Rect {
        x: area.x + GUTTER,
        width: area.width.saturating_sub(GUTTER * 2),
        ..area
    }
}

/// The tab row, and the accent rule that marks which tab is live.
///
/// The underline is drawn on its own row directly beneath the active label
/// rather than as a filled background: a reversed block is the loudest mark a
/// terminal can make, and spending it on "you are here" leaves nothing louder
/// for anything that matters.
fn draw_tabs(app: &App, frame: &mut Frame<'_>, tabs_area: Rect, rule_area: Rect) {
    let width = tabs_area.width as usize;
    let mut spans = vec![Span::raw(" ".repeat(GUTTER as usize))];
    let mut underline_start = 0usize;
    let mut underline_width = 0usize;
    let mut cursor = GUTTER as usize;

    for (index, (page, label)) in TABS.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
            cursor += 3;
        }
        let active = app.page == *page;
        spans.push(Span::styled(
            (*label).to_string(),
            if active {
                theme::strong()
            } else {
                theme::faint()
            },
        ));
        if active {
            underline_start = cursor;
            underline_width = label.chars().count();
        }
        cursor += label.chars().count();
    }

    // Pages reached from the palette are not tabs, but the user still has to be
    // told where they are — otherwise Clean looks like a Library that lost its
    // contents.
    if !TABS.iter().any(|(page, _)| *page == app.page) {
        spans.push(Span::styled("   ·   ", theme::faint()));
        cursor += 7;
        let label = app.page.label();
        spans.push(Span::styled(label.to_string(), theme::strong()));
        underline_start = cursor;
        underline_width = label.chars().count();
        cursor += underline_width;
    }

    // Right-hand context shares the row, so it is appended rather than drawn
    // over the top of it.
    let context = context_spans(app);
    let context_width: usize = context
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    let padding = width
        .saturating_sub(cursor)
        .saturating_sub(context_width)
        .saturating_sub(GUTTER as usize);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
        spans.extend(context);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

    if underline_width > 0 && width > underline_start {
        let available = width.saturating_sub(underline_start);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(underline_start)),
                Span::styled("─".repeat(underline_width.min(available)), theme::accent()),
            ])),
            rule_area,
        );
    }
}

/// Vault name and due count, for the right end of the tab row.
fn context_spans(app: &App) -> Vec<Span<'static>> {
    let vault = app
        .vault
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| app.vault.display().to_string());
    let mut spans = vec![Span::styled(vault, theme::dim())];
    if app.statistics.due_now > 0 {
        spans.push(Span::styled("  ·  ", theme::faint()));
        spans.push(Span::styled(
            format!("{} due", app.statistics.due_now),
            theme::accent(),
        ));
    }
    spans
}

/// The which-key footer: the keys that work right here, then the status.
///
/// Bindings are printed in priority order until the row runs out of width, so a
/// narrow terminal loses the rarest hints rather than wrapping them into an
/// unreadable second line — which is what the old detail-pane hints did.
fn draw_footer(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    if let Some(hint) = keymap::prompt_hint(&app.mode) {
        frame.render_widget(
            Paragraph::new(Line::styled(fit(hint, width), theme::faint())),
            area,
        );
        return;
    }

    // A review session has its own grammar, so it advertises its own keys
    // rather than the browse table's.
    if app.mode == Mode::Review {
        let session = app.review.as_ref();
        let revealed = session.is_some_and(|session| session.is_revealed());
        let has_clip = session.is_some_and(|session| session.current_has_clip());
        let card = session
            .and_then(|session| session.card())
            .map(|card| &card.content);
        let mut spans = Vec::new();
        for (key, label) in keymap::review_session_bindings(card, revealed, has_clip) {
            if !spans.is_empty() {
                spans.push(Span::raw("   "));
            }
            spans.push(Span::styled(key, theme::accent()));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, theme::faint()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let status_width = if app.status.is_empty() {
        0
    } else {
        (width / 2).min(app.status.chars().count() + 2)
    };
    let keys_width = width.saturating_sub(status_width);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for binding in keymap::bindings(app) {
        let key = binding.key.label();
        let cost = key.chars().count() + binding.label.chars().count() + 4;
        if used + cost > keys_width {
            break;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(key, theme::accent()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(binding.label.to_string(), theme::faint()));
        used += cost;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if status_width > 0 {
        let status = fit(&app.status, status_width.saturating_sub(1));
        let style = if app.status_error {
            theme::err()
        } else {
            theme::dim()
        };
        let padding = width.saturating_sub(status.chars().count());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(padding)),
                Span::styled(status, style),
            ])),
            area,
        );
    }
}

fn draw_palette(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let entries = palette::matching(&app.query);
    let height = (entries.len() as u16 + 4).min(area.height.saturating_sub(2));
    let region = widgets::centered(area, 68, height.max(5));
    frame.render_widget(Clear, region);

    let inner = widgets::overlay_block("Commands").inner(region);
    frame.render_widget(widgets::overlay_block("Commands"), region);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", theme::accent()),
            Span::raw(app.query.clone()),
            Span::styled("▏", theme::accent()),
        ])),
        rows[0],
    );

    let width = rows[1].width as usize;
    let items: Vec<ListItem<'static>> = entries
        .iter()
        .map(|entry| {
            let name_width = 26.min(width);
            ListItem::new(Line::from(vec![
                Span::styled(super::util::pad(entry.name, name_width), theme::text()),
                Span::styled(
                    fit(entry.detail, width.saturating_sub(name_width + 2)),
                    theme::faint(),
                ),
            ]))
        })
        .collect();

    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No matching command", theme::faint())),
            rows[1],
        );
        return;
    }
    let mut state = widgets::list_state(Some(app.palette_selected), items.len());
    frame.render_stateful_widget(widgets::list(items, true), rows[1], &mut state);
}

/// The full binding reference, for the keys the footer had no room to show.
fn draw_help(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let mut lines = vec![widgets::heading("On this page"), Line::raw("")];
    for binding in keymap::bindings(app) {
        lines.push(Line::from(vec![
            Span::styled(super::util::pad(&binding.key.label(), 9), theme::accent()),
            Span::raw(binding.label.to_string()),
        ]));
    }
    lines.extend([Line::raw(""), widgets::heading("Everywhere"), Line::raw("")]);
    for (key, label) in [
        ("1-4", "switch tab"),
        ("j k ↑ ↓", "move the cursor"),
        ("H J K L", "move focus between panes"),
        ("^k", "command palette"),
        ("^s", "sync vault"),
        ("esc", "back"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(super::util::pad(key, 9), theme::accent()),
            Span::raw(label.to_string()),
        ]));
    }

    let height = (lines.len() as u16 + 2).min(area.height);
    let region = widgets::centered(area, 54, height);
    frame.render_widget(Clear, region);
    frame.render_widget(
        Paragraph::new(lines).block(widgets::overlay_block("Keys")),
        region,
    );
}

/// The single-field prompts: new note, new deck, vault path, repository URL.
///
/// These used to be typed into the status bar, one character at a time, with
/// the caret implied. A focused overlay says what is being asked and shows the
/// caret where the text actually goes.
fn draw_prompt(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (title, hint) = match app.mode {
        Mode::DeckInput => (
            "New deck",
            "A deck groups cards you want to review together.",
        ),
        Mode::NoteInput => (
            "New note",
            "Creates the Markdown file and opens your editor.",
        ),
        Mode::VaultInput if app.create_vault => (
            "Create vault",
            "The directory is created if it does not exist.",
        ),
        Mode::VaultInput => ("Open vault", "Path to an existing vault directory."),
        Mode::GitRemoteInput => (
            "Repository URL",
            "git@github.com:owner/vault.git, or the https:// form after signing in.",
        ),
        _ => return,
    };
    let region = widgets::centered(area, 68, 6);
    frame.render_widget(Clear, region);
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled("› ", theme::accent()),
                Span::raw(app.query.clone()),
                Span::styled("▏", theme::accent()),
            ]),
            Line::raw(""),
            Line::styled(hint.to_string(), theme::faint()),
        ])
        .wrap(Wrap { trim: false })
        .block(widgets::overlay_block(title)),
        region,
    );
}

/// Shared by every page that shows a scrolling list beside a detail pane.
pub fn detail_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    focused: bool,
    lines: Vec<Line<'static>>,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    frame.render_widget(
        Paragraph::new(widgets::pane_title(title, focused, area.width as usize)),
        rows[0],
    );
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[1]);
}

/// Shared by every page that shows a scrolling list on the left.
pub fn list_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    focused: bool,
    items: Vec<ListItem<'static>>,
    selected: Option<usize>,
) {
    let body = if title.is_empty() {
        area
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);
        frame.render_widget(
            Paragraph::new(widgets::pane_title(title, focused, area.width as usize)),
            rows[0],
        );
        rows[1]
    };
    let mut state = widgets::list_state(selected, items.len());
    frame.render_stateful_widget(widgets::list(items, focused), body, &mut state);
}

/// A list rendered without a title row, for panes that are already labelled.
pub fn plain_list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'static>>,
    focused: bool,
    selected: Option<usize>,
) {
    let mut state = widgets::list_state(selected, items.len());
    frame.render_stateful_widget(widgets::list(items, focused), area, &mut state);
}
