//! One table describing what every key does, in every context.
//!
//! The footer is rendered from this table and key dispatch reads the same
//! table, so a binding cannot exist without being advertised and a hint cannot
//! advertise a key that does nothing. Before this existed the two were written
//! out by hand in separate places and had already drifted: `set_page_status`
//! promised `[1-7] pages` on a build with four tabs, and the same letter meant
//! different things on different pages with nothing on screen saying so.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, CleanItem, LibraryItem, Mode, Page, ReviewItem};
use crate::model::CardContent;

/// A key as the user presses it, and as the footer prints it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Space,
    Ctrl(char),
}

impl Key {
    pub fn label(self) -> String {
        match self {
            Key::Char(character) => character.to_string(),
            Key::Enter => "⏎".into(),
            Key::Space => "space".into(),
            Key::Ctrl(character) => format!("^{character}"),
        }
    }

    pub fn matches(self, event: KeyEvent) -> bool {
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        match self {
            Key::Char(character) => !control && event.code == KeyCode::Char(character),
            Key::Enter => event.code == KeyCode::Enter,
            Key::Space => !control && event.code == KeyCode::Char(' '),
            Key::Ctrl(character) => control && event.code == KeyCode::Char(character),
        }
    }
}

/// Everything a binding can ask the application to do.
///
/// One verb means one thing everywhere it appears. `n` is always "new" and `x`
/// is always "archive"; only the object changes with context, and the footer
/// names that object.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Edit,
    New,
    Archive,
    Restore,
    Delete,
    Assign,
    Enrol,
    ChooseDeck,
    PreviousDeck,
    NextDeck,
    Decrease,
    Increase,
    Search,
    PlayVideo,
    OpenUrl,
    OpenImage,
    FollowRelations,
    Palette,
    Help,
    Sync,
    Reindex,
    Quit,
}

pub struct Binding {
    pub key: Key,
    /// What this key does *here*, phrased as the object it acts on.
    pub label: &'static str,
    pub action: Action,
}

const fn binding(key: Key, label: &'static str, action: Action) -> Binding {
    Binding { key, label, action }
}

/// The bindings live on the current page, most useful first.
///
/// The footer prints as many as the terminal is wide enough to hold, so order
/// is the priority: the first entries are the ones a user reaches for daily.
pub fn bindings(app: &App) -> Vec<Binding> {
    let mut bindings = match app.page {
        Page::Library => library_bindings(app),
        Page::Review => review_bindings(app),
        Page::Relations => vec![
            binding(Key::Enter, "follow", Action::Open),
            binding(Key::Char('e'), "edit", Action::Edit),
        ],
        Page::Stats => Vec::new(),
        Page::Clean => clean_bindings(app),
        Page::Options => options_bindings(app),
        Page::Archived => vec![
            binding(Key::Char('u'), "restore", Action::Restore),
            binding(Key::Enter, "open", Action::Open),
        ],
    };
    // Global verbs come last so a page's own actions win the footer's width.
    bindings.extend([
        binding(Key::Char('/'), "search", Action::Search),
        binding(Key::Ctrl('k'), "commands", Action::Palette),
        binding(Key::Ctrl('s'), "sync", Action::Sync),
        binding(Key::Char('R'), "reindex", Action::Reindex),
        binding(Key::Char('?'), "help", Action::Help),
        binding(Key::Char('q'), "quit", Action::Quit),
    ]);
    bindings
}

fn library_bindings(app: &App) -> Vec<Binding> {
    let mut bindings = vec![
        binding(Key::Enter, "open", Action::Open),
        binding(Key::Char('n'), "new note", Action::New),
    ];
    // Only advertise the media keys when the selection actually carries media.
    // A hint for a key that will answer "no URL in this section" is noise.
    if let Some(section) = app.selected_library_section() {
        if app.section_has_video(section) {
            bindings.push(binding(Key::Char('v'), "play clip", Action::PlayVideo));
        }
        if app.section_has_url(section) {
            bindings.push(binding(Key::Char('o'), "open url", Action::OpenUrl));
        }
        if app.section_has_image(section) {
            bindings.push(binding(Key::Char('i'), "open image", Action::OpenImage));
        }
    }
    if matches!(
        app.library_items().get(app.selected),
        Some(LibraryItem::Note(_) | LibraryItem::Section(_))
    ) {
        bindings.push(binding(
            Key::Char('g'),
            "relations",
            Action::FollowRelations,
        ));
        bindings.push(binding(Key::Char('x'), "archive", Action::Archive));
    }
    bindings
}

fn review_bindings(app: &App) -> Vec<Binding> {
    let mut bindings = vec![binding(Key::Char('r'), "start review", Action::ChooseDeck)];
    match app.review_items().get(app.selected) {
        Some(ReviewItem::Section(_)) => bindings.extend([
            binding(Key::Space, "enrol", Action::Enrol),
            binding(Key::Enter, "open", Action::Open),
            binding(Key::Char('x'), "archive", Action::Archive),
        ]),
        Some(ReviewItem::Deck(_)) => bindings.extend([
            binding(Key::Enter, "make active", Action::Open),
            binding(Key::Char('x'), "archive", Action::Archive),
        ]),
        Some(ReviewItem::Card(_)) => bindings.extend([
            binding(Key::Enter, "review now", Action::Open),
            binding(Key::Char('a'), "assign to deck", Action::Assign),
            binding(Key::Char('x'), "archive", Action::Archive),
        ]),
        None => {}
    }
    bindings.extend([
        binding(Key::Char('n'), "new deck", Action::New),
        binding(Key::Char('['), "previous deck", Action::PreviousDeck),
        binding(Key::Char(']'), "next deck", Action::NextDeck),
    ]);
    bindings
}

fn clean_bindings(app: &App) -> Vec<Binding> {
    match app.clean_items().get(app.selected) {
        Some(CleanItem::Note(_)) => vec![
            binding(Key::Enter, "open", Action::Open),
            binding(Key::Char('x'), "archive", Action::Archive),
        ],
        Some(CleanItem::Card(_)) => vec![
            binding(Key::Char('a'), "assign to deck", Action::Assign),
            binding(Key::Enter, "review", Action::Open),
            binding(Key::Char('x'), "archive", Action::Archive),
        ],
        Some(CleanItem::Image(_)) => vec![
            binding(Key::Enter, "open", Action::Open),
            binding(Key::Char('d'), "delete file", Action::Delete),
        ],
        None => Vec::new(),
    }
}

fn options_bindings(app: &App) -> Vec<Binding> {
    if app.selected < App::ADJUSTABLE_OPTIONS {
        vec![
            binding(Key::Char('h'), "decrease", Action::Decrease),
            binding(Key::Char('l'), "increase", Action::Increase),
        ]
    } else {
        vec![binding(Key::Enter, "run", Action::Open)]
    }
}

/// The bindings shown while a review session is in progress.
///
/// Review is its own mode with its own grammar, so it is described separately
/// rather than pretending to share the browse table. The grammar also differs
/// per card type: `space` reveals a cloze but *selects* an answer on a
/// multiple-choice card, and a code-gap card wants typed text and `Tab`. The
/// footer said "space reveal" for all three.
pub fn review_session_bindings(
    card: Option<&CardContent>,
    revealed: bool,
    has_clip: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut hints: Vec<(&'static str, &'static str)> = if revealed {
        vec![("1-4", "rate")]
    } else {
        match card {
            Some(CardContent::MultipleChoice { .. }) => {
                vec![("j k", "move"), ("space", "select"), ("⏎", "submit")]
            }
            Some(CardContent::CodeGap { .. }) => {
                vec![
                    ("type", "fill the gap"),
                    ("tab", "next gap"),
                    ("⏎", "check"),
                ]
            }
            Some(CardContent::Cloze { .. } | CardContent::Section { .. }) => {
                vec![("space", "reveal")]
            }
            None => vec![("⏎", "finish")],
        }
    };
    if has_clip {
        hints.push(("v", "play clip"));
    }
    hints.push(("esc", "end session"));
    hints
}

/// True when the key is one of the universal motions, which every list honours
/// and which the footer therefore never bothers to print.
pub fn is_motion(key: KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Down | KeyCode::Up
    ) && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// The page a digit selects, or `None` when the digit is not a tab.
pub fn tab_for(character: char) -> Option<Page> {
    match character {
        '1' => Some(Page::Library),
        '2' => Some(Page::Review),
        '3' => Some(Page::Relations),
        '4' => Some(Page::Stats),
        _ => None,
    }
}

/// Mode-specific bindings for the transient input prompts.
pub fn prompt_hint(mode: &Mode) -> Option<&'static str> {
    match mode {
        Mode::DeckInput | Mode::NoteInput | Mode::VaultInput | Mode::GitRemoteInput => {
            Some("⏎ confirm   esc cancel")
        }
        Mode::Search => Some("⏎ open   esc back"),
        Mode::Palette => Some("⏎ run   esc close"),
        Mode::Help => Some("any key closes"),
        _ => None,
    }
}
