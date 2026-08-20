//! The Stats page: what is due, how recall is going, and what needs attention.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::util::{day_label, fit};
use super::super::{App, theme};
use super::widgets;

pub fn draw(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(area);

    draw_pulse(app, frame, rows[0]);

    let (left, right) = widgets::split_detail(rows[1], 50);
    frame.render_widget(
        Paragraph::new(activity(app, left.width as usize)).scroll((app.scroll, 0)),
        left,
    );
    frame.render_widget(Paragraph::new(signals(app, right.width as usize)), right);
}

/// The three numbers worth knowing before anything else.
///
/// These were three bordered boxes a third of the screen wide, each spending
/// two rows and four columns of frame on a single number, and each truncating
/// its own caption to fit — "clear today's qu".
fn draw_pulse(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let stats = &app.statistics;
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(area);

    let cells = [
        (
            "Due now",
            stats.due_now.to_string(),
            if stats.due_now > 0 {
                "waiting for you".to_string()
            } else {
                "queue is clear".to_string()
            },
            if stats.due_now > 0 {
                theme::accent()
            } else {
                theme::ok()
            },
        ),
        (
            "Accuracy",
            percentage(stats.accuracy_week),
            "last 7 days".to_string(),
            theme::text(),
        ),
        (
            "Streak",
            format!("{} days", stats.streak_days),
            format!("{} reviewed today", stats.reviewed_today),
            theme::text(),
        ),
    ];

    for (column, (label, value, caption, style)) in columns.iter().zip(cells) {
        let width = column.width.saturating_sub(1) as usize;
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(fit(&label.to_uppercase(), width), theme::heading()),
                Line::styled(
                    fit(&value, width),
                    style.add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Line::styled(fit(&caption, width), theme::faint()),
            ]),
            *column,
        );
    }
}

fn activity(app: &App, width: usize) -> Vec<Line<'static>> {
    let stats = &app.statistics;
    let response = stats.average_response_ms.map_or("--".into(), |value| {
        format!("{:.1}s", value as f64 / 1000.0)
    });

    let mut lines = vec![
        widgets::heading("Activity · 14 days"),
        Line::styled(
            fit(
                &format!(
                    "{} reviews this week  ·  {} average response",
                    stats.reviewed_week, response
                ),
                width,
            ),
            theme::faint(),
        ),
        Line::raw(""),
    ];
    let maximum = stats
        .daily_reviews
        .iter()
        .map(|(_, count)| *count)
        .max()
        .unwrap_or(1);
    lines.extend(
        stats
            .daily_reviews
            .iter()
            .map(|(day, count)| bar_row(&day_label(*day), *count, maximum, width)),
    );
    lines
}

fn signals(app: &App, width: usize) -> Vec<Line<'static>> {
    let stats = &app.statistics;
    let mut lines = vec![widgets::heading("Workload · next 7 days"), Line::raw("")];
    let forecast_max = stats
        .due_forecast
        .iter()
        .map(|(_, count)| *count)
        .max()
        .unwrap_or(1);
    lines.extend(
        stats
            .due_forecast
            .iter()
            .map(|(day, count)| bar_row(&day_label(*day), *count, forecast_max, width)),
    );

    lines.extend([
        Line::raw(""),
        widgets::heading("Ratings · 30 days"),
        Line::styled(
            format!("{} accuracy", percentage(stats.accuracy_month)),
            theme::faint(),
        ),
        Line::raw(""),
    ]);
    let ratings_max = stats.rating_counts.iter().copied().max().unwrap_or(1);
    lines.extend(
        ["again", "hard", "good", "easy"]
            .iter()
            .zip(stats.rating_counts)
            .map(|(label, count)| bar_row(label, count, ratings_max, width)),
    );

    lines.extend([
        Line::raw(""),
        widgets::heading("Needs attention"),
        Line::raw(""),
    ]);
    if stats.weak_notes.is_empty() {
        lines.push(Line::styled(
            "Not enough review history yet",
            theme::faint(),
        ));
    }
    for (title, reviews, score) in &stats.weak_notes {
        lines.push(widgets::spread(
            title,
            &format!("{:.0}%  ·  {reviews} reviews", score * 100.0),
            width,
            theme::warn(),
        ));
    }

    lines.extend([
        Line::raw(""),
        widgets::heading("Library"),
        Line::styled(
            fit(
                &format!(
                    "{} notes  ·  {} topics  ·  {} active cards",
                    stats.note_count, stats.topic_count, stats.card_count
                ),
                width,
            ),
            theme::dim(),
        ),
        Line::styled(
            fit(
                &match stats.untopiced_count {
                    0 => "Every note has a topic".to_string(),
                    1 => "1 note still needs a topic".to_string(),
                    count => format!("{count} notes still need a topic"),
                },
                width,
            ),
            if stats.untopiced_count > 0 {
                theme::warn()
            } else {
                theme::faint()
            },
        ),
    ]);
    lines
}

/// A labelled bar: name, meter, count. The meter is sized from the pane so it
/// grows with the terminal instead of sitting at a hardcoded 24 columns.
fn bar_row(label: &str, count: usize, maximum: usize, width: usize) -> Line<'static> {
    let label_width = 8usize;
    let count_text = count.to_string();
    let meter_width = width
        .saturating_sub(label_width + count_text.chars().count() + 3)
        .clamp(1, 32);
    let mut spans = vec![Span::styled(
        super::super::util::pad(label, label_width),
        theme::dim(),
    )];
    spans.extend(widgets::meter(count, maximum, meter_width));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(count_text, theme::faint()));
    Line::from(spans)
}

fn percentage(value: Option<f64>) -> String {
    value.map_or("--".into(), |value| format!("{:.0}%", value * 100.0))
}
