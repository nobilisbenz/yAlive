//! The Review page, the deck chooser, and the review session itself.

use std::collections::HashMap;
use std::sync::LazyLock;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, ListItem, Paragraph, Wrap};
use regex::Regex;

use super::super::util::{canonical_answer, display_markdown, fit, matches_gap, short_date};
use super::super::{App, ReviewItem, ReviewPhase, ReviewSession, theme};
use super::widgets;
use super::{detail_pane, list_pane};
use crate::model::{CardContent, GapDefinition, ReviewCard};

/// Compiled once. These used to be rebuilt on every frame, which meant
/// recompiling two regular expressions sixty times a second while a card was
/// merely sitting on screen.
static CLOZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{c(\d+)::([^}:]+)(?:::([^}]+))?\}\}").expect("valid cloze"));
static GAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{gap:([a-zA-Z0-9_-]+)\}\}").expect("valid gap"));

// ---------------------------------------------------------------- review page

pub fn draw_page(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (left, right) = widgets::split_detail(area, 44);
    draw_organiser(app, frame, left);
    draw_detail(app, frame, right);
}

fn draw_organiser(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let items = app.review_items();
    let mut rows: Vec<ListItem<'static>> = Vec::new();
    let mut selected_row = None;
    let mut seen = [false; 3];

    for (index, item) in items.iter().enumerate() {
        let (group, line) = match item {
            ReviewItem::Section(section_index) => {
                let section = &app.review_sections[*section_index];
                (
                    0,
                    Line::from(vec![
                        Span::styled(if section.enrolled { "✓ " } else { "  " }, theme::accent()),
                        Span::raw(fit(
                            &if section.note_title == section.heading {
                                section.heading.clone()
                            } else {
                                format!("{} / {}", section.note_title, section.heading)
                            },
                            width.saturating_sub(2),
                        )),
                    ]),
                )
            }
            ReviewItem::Deck(deck_index) => {
                let deck = &app.decks[*deck_index];
                let active = *deck_index == app.active_deck;
                (
                    1,
                    widgets::spread(
                        &format!("{}{}", if active { "● " } else { "  " }, deck.name),
                        &format!("{} cards", deck.card_count),
                        width,
                        if active {
                            theme::accent()
                        } else {
                            theme::faint()
                        },
                    ),
                )
            }
            ReviewItem::Card(card_index) => (
                2,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        fit(&app.cards[*card_index].label, width.saturating_sub(2)),
                        theme::dim(),
                    ),
                ]),
            ),
        };
        if !seen[group] {
            if group > 0 {
                rows.push(ListItem::new(Line::raw("")));
            }
            rows.push(ListItem::new(widgets::heading(
                ["Sections", "Decks", "Cards"][group],
            )));
            seen[group] = true;
        }
        if index == app.selected {
            selected_row = Some(rows.len());
        }
        rows.push(ListItem::new(line));
    }

    if rows.is_empty() {
        rows.push(ListItem::new(Line::styled(
            "Nothing to review yet",
            theme::dim(),
        )));
        rows.push(ListItem::new(Line::styled(
            "Enrol a section with space, or write a quiz card",
            theme::faint(),
        )));
    }

    list_pane(frame, area, "", app.focused_panel == 0, rows, selected_row);
}

fn draw_detail(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let width = area.width as usize;
    let active = app
        .decks
        .get(app.active_deck)
        .map_or("No deck", |deck| deck.name.as_str());

    let (title, lines) = match app.review_items().get(app.selected) {
        Some(ReviewItem::Section(index)) => {
            let section = &app.review_sections[*index];
            let body = app
                .sections
                .iter()
                .find(|candidate| candidate.uid == section.uid)
                .map(|candidate| display_markdown(&candidate.body))
                .unwrap_or_default();
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(fit(&section.note_title, width / 2), theme::dim()),
                    Span::styled("  ·  ", theme::faint()),
                    Span::styled(
                        if section.enrolled {
                            "enrolled"
                        } else {
                            "not enrolled"
                        },
                        if section.enrolled {
                            theme::ok()
                        } else {
                            theme::faint()
                        },
                    ),
                ]),
                Line::raw(""),
            ];
            lines.extend(body.lines().map(|line| Line::raw(line.to_string())));
            (section.heading.clone(), lines)
        }
        Some(ReviewItem::Deck(index)) => {
            let deck = &app.decks[*index];
            let mut lines = vec![
                Line::styled(format!("{} cards", deck.card_count), theme::dim()),
                Line::raw(""),
            ];
            lines.extend(
                app.cards
                    .iter()
                    .filter(|card| card.decks.contains(&deck.id))
                    .map(|card| {
                        Line::raw(format!("  {}", fit(&card.label, width.saturating_sub(2))))
                    }),
            );
            (deck.name.clone(), lines)
        }
        Some(ReviewItem::Card(index)) => {
            let card = &app.cards[*index];
            let assigned = app
                .decks
                .iter()
                .filter(|deck| card.decks.contains(&deck.id))
                .map(|deck| deck.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (
                card.label.clone(),
                vec![
                    widgets::kv(
                        "decks",
                        if assigned.is_empty() {
                            "unassigned"
                        } else {
                            &assigned
                        },
                        width,
                    ),
                    widgets::kv("active", active, width),
                ],
            )
        }
        None => (
            "Review".into(),
            widgets::empty_state(
                "Nothing is enrolled for review.",
                "Select a section and press space to enrol it.",
            ),
        ),
    };
    detail_pane(frame, area, &title, app.focused_panel == 1, lines);
}

// -------------------------------------------------------------- deck chooser

pub fn draw_deck_choice(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let deckless = app
        .cards
        .iter()
        .filter(|card| card.decks.is_empty())
        .count();
    let mut rows = vec![ListItem::new(widgets::spread(
        "No deck",
        &format!("{deckless} cards"),
        58,
        theme::faint(),
    ))];
    rows.extend(app.decks.iter().map(|deck| {
        ListItem::new(widgets::spread(
            &deck.name,
            &format!("{} cards", deck.card_count),
            58,
            theme::faint(),
        ))
    }));

    let height = (rows.len() as u16 + 4).min(area.height);
    let region = widgets::centered(area, 62, height);
    frame.render_widget(Clear, region);
    let block = widgets::overlay_block("Review which deck?");
    let inner = block.inner(region);
    frame.render_widget(block, region);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    let mut state = widgets::list_state(Some(app.review_scope_selected), rows.len());
    frame.render_stateful_widget(widgets::list(rows, true), split[0], &mut state);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⏎", theme::accent()),
            Span::styled(" due cards    ", theme::faint()),
            Span::styled("f", theme::accent()),
            Span::styled(" every card    ", theme::faint()),
            Span::styled("esc", theme::accent()),
            Span::styled(" cancel", theme::faint()),
        ])),
        split[1],
    );
}

// ------------------------------------------------------------ review session

/// The review session, drawn without a frame around it.
///
/// This is the screen a daily user spends the most time in, so it gets the most
/// space: no box, no title bar, and the card set in from the left margin with
/// room above and below it.
pub fn draw_session(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(session) = &app.review else {
        return;
    };
    let Some(card) = session.card() else {
        let region = widgets::centered(area, 40, 3);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Session complete", theme::ok()),
                Line::raw(""),
                Line::styled("⏎ back to the vault", theme::faint()),
            ]),
            region,
        );
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // progress
            Constraint::Length(1), // metadata
            Constraint::Length(2), // space
            Constraint::Min(1),    // card
        ])
        .split(area);

    let width = rows[0].width as usize;
    let total = session.cards.len();
    let position = session.current + 1;
    let counter = format!("{position} / {total}");
    let meter_width = 14.min(width.saturating_sub(counter.chars().count() + 12));
    let mut header = vec![Span::styled("Review", theme::strong())];
    if meter_width > 2 {
        header.push(Span::raw("   "));
        header.extend(widgets::meter(session.current, total, meter_width));
    }
    let used: usize = header.iter().map(|s| s.content.chars().count()).sum();
    header.push(Span::raw(
        " ".repeat(width.saturating_sub(used + counter.chars().count())),
    ));
    header.push(Span::styled(counter, theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(header)), rows[0]);

    frame.render_widget(
        Paragraph::new(Line::styled(fit(&metadata(card), width), theme::faint())),
        rows[1],
    );

    let body = Rect {
        x: rows[3].x + 3,
        width: rows[3].width.saturating_sub(3),
        ..rows[3]
    };
    frame.render_widget(
        Paragraph::new(card_lines(session, card)).wrap(Wrap { trim: false }),
        body,
    );
}

fn metadata(card: &ReviewCard) -> String {
    let seen = if card.review_count == 0 {
        "new".to_string()
    } else {
        format!("seen {}", card.review_count)
    };
    format!(
        "{}  ·  {}  ·  due {}",
        card.section_uid,
        seen,
        short_date(card.due_at)
    )
}

/// The card itself. Prompt-side and answer-side, without the key hints that
/// used to be baked into the body — the footer carries those now, so they do
/// not shift the card's text every time the phase changes.
fn card_lines(session: &ReviewSession, card: &ReviewCard) -> Vec<Line<'static>> {
    let revealed = session.phase == ReviewPhase::Revealed;
    let mut lines = Vec::new();
    // Held back so the verdict lands directly under the answers, where the eye
    // already is, rather than below a paragraph of explanation.
    let mut aside: Vec<Line<'static>> = Vec::new();

    match &card.content {
        CardContent::Section { title, body } => {
            lines.push(Line::styled(title.clone(), theme::strong()));
            lines.push(Line::raw(""));
            if revealed {
                lines.extend(
                    display_markdown(body)
                        .lines()
                        .map(|line| Line::raw(line.to_string())),
                );
            } else {
                lines.push(Line::styled(
                    "Recall this section, then reveal it.",
                    theme::faint(),
                ));
            }
        }
        CardContent::Cloze { prompt, cloze, .. } => {
            lines.extend(render_cloze(prompt, *cloze, revealed));
        }
        CardContent::MultipleChoice {
            question,
            answers,
            explanation,
            ..
        } => {
            lines.extend(question.lines().map(|line| Line::raw(line.to_string())));
            lines.push(Line::raw(""));
            for (position, index) in session.answer_order.iter().copied().enumerate() {
                let answer = &answers[index];
                let chosen = session.selected.contains(&index);
                let cursor = position == session.choice_cursor;
                let style = if revealed && answer.correct {
                    theme::ok()
                } else if revealed && chosen {
                    theme::err()
                } else if chosen {
                    theme::strong()
                } else {
                    theme::text()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if cursor {
                            theme::CURSOR
                        } else {
                            theme::NO_CURSOR
                        },
                        theme::accent(),
                    ),
                    Span::styled(if chosen { " ● " } else { " ○ " }, style),
                    Span::styled(answer.text.clone(), style),
                ]));
            }
            if revealed && let Some(explanation) = explanation {
                aside.push(Line::raw(""));
                aside.push(Line::styled("Why", theme::heading()));
                aside.extend(explanation.lines().map(|line| Line::raw(line.to_string())));
            }
        }
        CardContent::CodeGap {
            language,
            prompt,
            code,
            gaps,
            ..
        } => {
            if let Some(prompt) = prompt {
                lines.extend(prompt.lines().map(|line| Line::raw(line.to_string())));
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(language.to_lowercase(), theme::heading()));
            lines.extend(render_code(code, session, gaps));
            if !revealed {
                let current = session
                    .gap_names
                    .get(session.gap_cursor)
                    .map_or("", String::as_str);
                let value = session.gap_values.get(current).map_or("", String::as_str);
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled(format!("{current}  "), theme::accent()),
                    if value.is_empty() {
                        Span::styled("type your answer", theme::faint())
                    } else {
                        Span::raw(value.to_string())
                    },
                ]));
                lines.push(Line::styled("tab next gap    ⏎ check", theme::faint()));
            }
        }
    }

    if revealed {
        if !session.feedback.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                session.feedback.clone(),
                if session.correct == Some(false) {
                    theme::err()
                } else {
                    theme::ok()
                },
            ));
        }
        lines.extend(aside);
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("1", theme::accent()),
            Span::styled(" again    ", theme::faint()),
            Span::styled("2", theme::accent()),
            Span::styled(" hard    ", theme::faint()),
            Span::styled("3", theme::accent()),
            Span::styled(" good    ", theme::faint()),
            Span::styled("4", theme::accent()),
            Span::styled(" easy", theme::faint()),
        ]));
    }
    lines
}

fn render_cloze(prompt: &str, target: u32, revealed: bool) -> Vec<Line<'static>> {
    let rendered = CLOZE.replace_all(prompt, |capture: &regex::Captures<'_>| {
        let number: u32 = capture[1].parse().unwrap_or_default();
        if number == target && !revealed {
            capture
                .get(3)
                .map_or("[…]", |hint| hint.as_str())
                .to_string()
        } else {
            capture[2].to_string()
        }
    });
    rendered
        .lines()
        .map(|line| Line::raw(line.to_string()))
        .collect()
}

fn render_code(
    code: &str,
    session: &ReviewSession,
    gaps: &HashMap<String, GapDefinition>,
) -> Vec<Line<'static>> {
    code.lines()
        .enumerate()
        .map(|(index, line)| {
            let mut spans = vec![Span::styled(format!("{:>3}  ", index + 1), theme::faint())];
            let mut end = 0;
            for capture in GAP.captures_iter(line) {
                let whole = capture.get(0).expect("group 0 always matches");
                spans.push(Span::raw(line[end..whole.start()].to_string()));
                let name = &capture[1];
                // A gap named in the code with no definition in the card is an
                // authoring mistake; show the name rather than panicking on a
                // missing map entry, which is what indexing used to do.
                let definition = gaps.get(name);
                let (value, style) = match (session.phase == ReviewPhase::Revealed, definition) {
                    (true, Some(definition)) => {
                        let submitted = session.gap_values.get(name).map_or("", String::as_str);
                        let value = if matches_gap(submitted, definition) {
                            submitted.to_string()
                        } else {
                            canonical_answer(definition)
                        };
                        (value, theme::ok().add_modifier(Modifier::BOLD))
                    }
                    (true, None) => (format!("[{name}?]"), theme::err()),
                    (false, _) => {
                        let value = session.gap_values.get(name).map_or("", String::as_str);
                        let value = if value.is_empty() {
                            format!("[{name}]")
                        } else {
                            value.to_string()
                        };
                        let active = session
                            .gap_names
                            .get(session.gap_cursor)
                            .is_some_and(|active| active == name);
                        let style: Style = if active {
                            theme::accent().add_modifier(Modifier::BOLD)
                        } else {
                            theme::dim()
                        };
                        (value, style)
                    }
                };
                spans.push(Span::styled(value, style));
                end = whole.end();
            }
            spans.push(Span::raw(line[end..].to_string()));
            Line::from(spans)
        })
        .collect()
}
