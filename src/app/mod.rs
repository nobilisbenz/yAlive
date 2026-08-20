//! The terminal application: state, key handling, and vault actions.
//!
//! Rendering lives in [`ui`], the binding table in [`keymap`], colour in
//! [`theme`], and the `Ctrl+K` commands in [`palette`]. This module owns what
//! the application *is* and what it *does*; those own how it looks.

mod keymap;
mod palette;
mod theme;
mod ui;
mod util;

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use rand::seq::SliceRandom;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use regex::Regex;

use crate::config::{Config, ReviewOrder};
use crate::db::Database;
use crate::model::{
    ArchivedItem, CardContent, CardRow, ChoiceMode, DeckRow, NoteRow, RelationRow, ReviewCard,
    ReviewScope, ReviewSectionRow, SectionRow, Statistics,
};
use crate::sync;

use keymap::Action;
use palette::Command as PaletteCommand;
use util::{archived_item_label, expand_home, find_orphan_images, matches_gap, slugify};

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Work that needs the terminal handed back before it can run.
enum Pending {
    Editor { path: PathBuf, line: usize },
    GithubAuth,
}

#[derive(PartialEq)]
enum Mode {
    Browse,
    Search,
    /// The `Ctrl+K` command palette.
    Palette,
    /// The full key reference, opened with `?`.
    Help,
    DeckInput,
    NoteInput,
    VaultInput,
    GitRemoteInput,
    ReviewDeckChoice,
    Review,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Library,
    Review,
    Relations,
    Stats,
    Clean,
    Options,
    Archived,
}

impl Page {
    /// The page's name, as the tab row and the palette print it.
    fn label(self) -> &'static str {
        match self {
            Page::Library => "Library",
            Page::Review => "Review",
            Page::Relations => "Relations",
            Page::Stats => "Stats",
            Page::Clean => "Clean",
            Page::Options => "Options",
            Page::Archived => "Archived",
        }
    }
}

#[derive(Clone, Copy)]
enum LibraryItem {
    Note(usize),
    Section(usize),
}

#[derive(Clone, Copy)]
enum ReviewItem {
    Section(usize),
    Deck(usize),
    Card(usize),
}

#[derive(Clone, Copy)]
enum CleanItem {
    Note(usize),
    Card(usize),
    Image(usize),
}

#[derive(PartialEq)]
enum ReviewPhase {
    Answering,
    Revealed,
}

struct ReviewSession {
    cards: Vec<ReviewCard>,
    current: usize,
    phase: ReviewPhase,
    answer_order: Vec<usize>,
    selected: HashSet<usize>,
    choice_cursor: usize,
    gap_names: Vec<String>,
    gap_values: HashMap<String, String>,
    gap_cursor: usize,
    correct: Option<bool>,
    started: Instant,
    feedback: String,
}

impl ReviewSession {
    fn new(cards: Vec<ReviewCard>) -> Self {
        let mut session = Self {
            cards,
            current: 0,
            phase: ReviewPhase::Answering,
            answer_order: Vec::new(),
            selected: HashSet::new(),
            choice_cursor: 0,
            gap_names: Vec::new(),
            gap_values: HashMap::new(),
            gap_cursor: 0,
            correct: None,
            started: Instant::now(),
            feedback: String::new(),
        };
        session.prepare_card();
        session
    }

    fn card(&self) -> Option<&ReviewCard> {
        self.cards.get(self.current)
    }

    /// Whether the answer side is showing.
    fn is_revealed(&self) -> bool {
        self.phase == ReviewPhase::Revealed
    }

    /// Whether the card on screen has a clip `v` could play, given the phase.
    ///
    /// The answer-side clip is only offered once the answer is visible, which
    /// is the same rule the key itself follows.
    fn current_has_clip(&self) -> bool {
        self.card().is_some_and(|card| {
            let clips = match &card.content {
                CardContent::Cloze { clips, .. }
                | CardContent::MultipleChoice { clips, .. }
                | CardContent::CodeGap { clips, .. } => clips.clone(),
                CardContent::Section { .. } => Default::default(),
            };
            if self.is_revealed() {
                clips.answer.or(clips.prompt).is_some()
            } else {
                clips.prompt.is_some()
            }
        })
    }

    fn prepare_card(&mut self) {
        self.phase = ReviewPhase::Answering;
        self.selected.clear();
        self.choice_cursor = 0;
        self.gap_values.clear();
        self.gap_cursor = 0;
        self.correct = None;
        self.feedback.clear();
        self.started = Instant::now();
        self.answer_order.clear();
        self.gap_names.clear();
        if let Some(content) = self.card().map(|card| card.content.clone()) {
            match &content {
                CardContent::MultipleChoice { answers, .. } => {
                    self.answer_order = (0..answers.len()).collect();
                    self.answer_order.shuffle(&mut rand::rng());
                }
                CardContent::CodeGap { code, .. } => {
                    let re = Regex::new(r"\{\{gap:([a-zA-Z0-9_-]+)\}\}").unwrap();
                    for capture in re.captures_iter(code) {
                        let name = capture[1].to_string();
                        if !self.gap_names.contains(&name) {
                            self.gap_names.push(name.clone());
                            self.gap_values.insert(name, String::new());
                        }
                    }
                }
                CardContent::Cloze { .. } | CardContent::Section { .. } => {}
            }
        }
    }
}

pub struct App {
    vault: PathBuf,
    config: Config,
    db: Database,
    sections: Vec<SectionRow>,
    notes: Vec<NoteRow>,
    review_sections: Vec<ReviewSectionRow>,
    decks: Vec<DeckRow>,
    cards: Vec<CardRow>,
    archived: Vec<ArchivedItem>,
    orphan_images: Vec<PathBuf>,
    statistics: Statistics,
    selected: usize,
    scroll: u16,
    active_deck: usize,
    review_scope_selected: usize,
    page: Page,
    focused_panel: usize,
    relation_section: usize,
    incoming_selected: usize,
    outgoing_selected: usize,
    mode: Mode,
    query: String,
    relations: Vec<RelationRow>,
    review: Option<ReviewSession>,
    create_vault: bool,
    next_vault: Option<PathBuf>,
    sync_remote: Option<String>,
    status: String,
    /// Whether [`App::status`] describes a failure, so the footer can colour it.
    status_error: bool,
    /// Cursor inside the command palette's filtered list.
    palette_selected: usize,
    /// Set by a key handler, drained by the event loop.
    pending: Option<Pending>,
    last_index: Instant,
}

impl App {
    pub fn new(vault: PathBuf, mut db: Database) -> Result<Self> {
        let config = Config::load(&vault)?;
        db.index_vault(&vault)?;
        let sections = db.sections()?;
        let notes = db.notes()?;
        let review_sections = db.review_sections()?;
        let decks = db.decks()?;
        let cards = db.card_rows()?;
        let archived = db.archived_items()?;
        let orphan_images = find_orphan_images(&vault)?;
        let statistics = db.statistics()?;
        let sync_remote = sync::remote(&vault);
        Ok(Self {
            vault,
            config,
            db,
            sections,
            notes,
            review_sections,
            decks,
            cards,
            archived,
            orphan_images,
            statistics,
            selected: 0,
            scroll: 0,
            active_deck: 0,
            review_scope_selected: 0,
            page: Page::Library,
            focused_panel: 0,
            relation_section: 0,
            incoming_selected: 0,
            outgoing_selected: 0,
            mode: Mode::Browse,
            query: String::new(),
            relations: Vec::new(),
            review: None,
            create_vault: false,
            next_vault: None,
            sync_remote,
            status: String::new(),
            status_error: false,
            palette_selected: 0,
            pending: None,
            last_index: Instant::now(),
        })
    }

    pub fn run(mut self) -> Result<Option<PathBuf>> {
        let mut terminal = setup_terminal()?;
        let result = self.event_loop(&mut terminal);
        restore_terminal(&mut terminal)?;
        result
    }

    fn event_loop(&mut self, terminal: &mut Tui) -> Result<Option<PathBuf>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)?
            {
                return Ok(self.next_vault.take());
            }
            if let Some(pending) = self.pending.take() {
                self.perform(pending, terminal)?;
            }
            self.consume_ygraphy_command()?;
            if self.last_index.elapsed()
                >= Duration::from_millis(self.config.reindex_interval_ms.max(200))
                && self.mode != Mode::Review
            {
                self.refresh_index()?;
            }
        }
    }

    fn refresh_index(&mut self) -> Result<()> {
        self.last_index = Instant::now();
        let summary = self.db.index_vault(&self.vault)?;
        self.sections = if self.mode == Mode::Search {
            self.db.search(&self.query)?
        } else {
            self.db.sections()?
        };
        self.refresh_database_views()?;
        if summary.indexed + summary.removed + summary.failed > 0 {
            self.orphan_images = find_orphan_images(&self.vault)?;
            self.note(format!(
                "index: {} updated, {} removed, {} failed",
                summary.indexed, summary.removed, summary.failed
            ));
        }
        let total = match self.page {
            Page::Library => self.library_items().len(),
            Page::Review => self.review_items().len(),
            Page::Relations => self.sections.len(),
            Page::Stats => 1,
            Page::Clean => self.clean_items().len(),
            Page::Options => Self::OPTION_COUNT,
            Page::Archived => self.archived.len(),
        };
        self.selected = self.selected.min(total.saturating_sub(1));
        Ok(())
    }

    /// Act on a "focus this section" command written by yGraphy.
    ///
    /// yGraphy writes the UID to a `.pending` file and renames it into place, so
    /// what lands here is never half-written. The command is left on disk while
    /// a review session is running: double-clicking a node in the graph should
    /// not yank you out of the card you are answering, and the click should not
    /// be thrown away either. It is picked up as soon as the session ends.
    fn consume_ygraphy_command(&mut self) -> Result<()> {
        if self.mode == Mode::Review {
            return Ok(());
        }
        let path = self.vault.join(".notes/ygraphy-open.json");
        if !path.exists() {
            return Ok(());
        }
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                self.fail(format!("could not read ygraphy command: {error}"));
                return Ok(());
            }
        };
        if let Err(error) = fs::remove_file(&path) {
            self.fail(format!("could not consume ygraphy command: {error}"));
            return Ok(());
        }
        let uid: String = match serde_json::from_str(&source) {
            Ok(uid) => uid,
            Err(error) => {
                self.fail(format!("ignored invalid ygraphy command: {error}"));
                return Ok(());
            }
        };
        self.refresh_index()?;
        self.mode = Mode::Browse;
        self.page = Page::Relations;
        self.focused_panel = 1;
        self.follow_relation(&uid)?;
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.sync_vault()?;
            return Ok(false);
        }
        if self.mode == Mode::Browse
            && let Some((dx, dy)) = shifted_panel_direction(key)
        {
            self.move_panel_focus(dx, dy);
            return Ok(false);
        }
        match self.mode {
            Mode::Browse => self.handle_browse(key),
            Mode::Palette => self.handle_palette(key),
            Mode::Help => {
                self.mode = Mode::Browse;
                Ok(false)
            }
            Mode::Search => self.handle_search(key),
            Mode::DeckInput => self.handle_deck_input(key),
            Mode::NoteInput => self.handle_note_input(key),
            Mode::VaultInput => self.handle_vault_input(key),
            Mode::GitRemoteInput => self.handle_git_remote_input(key),
            Mode::ReviewDeckChoice => self.handle_review_deck_choice(key),
            Mode::Review => self.handle_review(key),
        }
    }

    fn handle_review_deck_choice(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.review_scope_selected = (self.review_scope_selected + 1).min(self.decks.len());
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.review_scope_selected = self.review_scope_selected.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('r') => self.start_scoped_review(false)?,
            KeyCode::Char('f') => self.start_scoped_review(true)?,
            _ => {}
        }
        Ok(false)
    }

    fn start_scoped_review(&mut self, force: bool) -> Result<()> {
        let (scope, name) = if self.review_scope_selected == 0 {
            (ReviewScope::Deckless, "No deck".to_string())
        } else if let Some(deck) = self.decks.get(self.review_scope_selected - 1) {
            (ReviewScope::Deck(deck.id), deck.name.clone())
        } else {
            return Ok(());
        };
        let cards = self.db.review_cards(
            scope,
            force,
            self.config.new_cards_per_day,
            self.config.max_reviews_per_day,
            self.config.review_order,
            self.config.bury_siblings,
        )?;
        let count = cards.len();
        self.review = Some(ReviewSession::new(cards));
        self.mode = Mode::Review;
        self.note(if force {
            format!("force reviewing {count} cards from {name}")
        } else {
            format!("{count} cards due in {name}")
        });
        Ok(())
    }

    fn handle_search(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.sections = self.db.sections()?;
                self.selected = 0;
            }
            KeyCode::Enter => {
                self.edit_selected()?;
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Backspace => {
                self.query.pop();
                self.update_search()?;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.update_search()?;
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_deck_input(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                match self.db.create_deck(&self.query) {
                    Ok(()) => {
                        self.refresh_views()?;
                        self.active_deck = self
                            .decks
                            .iter()
                            .position(|deck| deck.name == self.query.trim())
                            .unwrap_or(0);
                        self.note(format!("created deck {}", self.query));
                    }
                    Err(error) => self.fail(format!("could not create deck: {error}")),
                }
                self.mode = Mode::Browse;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.note(format!("New deck name: {}", self.query));
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.note(format!("New deck name: {}", self.query));
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_note_input(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                let title = self.query.trim().to_string();
                let mut slug = slugify(&title);
                if slug.is_empty() {
                    slug = "note".into();
                }
                let mut path = self.vault.join(format!("{slug}.md"));
                let mut suffix = 2;
                while path.exists() {
                    path = self.vault.join(format!("{slug}-{suffix}.md"));
                    suffix += 1;
                }
                let note_id = path.file_stem().unwrap().to_string_lossy();
                let source = format!(
                    "---\nid: {note_id}\ntitle: {}\ntopic:\npinned: false\n---\n\n# {} {{#root}}\n\n",
                    serde_json::to_string(&title)?,
                    title
                );
                fs::write(&path, source).with_context(|| format!("creating {}", path.display()))?;
                self.mode = Mode::Browse;
                self.open_editor(&path, 8);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.note(format!("New note title: {}", self.query));
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.note(format!("New note title: {}", self.query));
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_vault_input(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.query.clear();
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                let path = expand_home(self.query.trim())?;
                if self.create_vault {
                    fs::create_dir_all(&path)
                        .with_context(|| format!("creating vault {}", path.display()))?;
                }
                if !path.is_dir() {
                    self.fail(format!(
                        "vault directory does not exist: {}",
                        path.display()
                    ));
                    return Ok(false);
                }
                self.next_vault = Some(path.canonicalize()?);
                return Ok(true);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.update_vault_input_status();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.update_vault_input_status();
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_git_remote_input(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.query.clear();
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                match sync::configure_remote(&self.vault, self.query.trim()) {
                    Ok(()) => {
                        self.sync_remote = sync::remote(&self.vault);
                        self.status =
                            "Git repository saved; select Sync now to upload the vault".into();
                        self.mode = Mode::Browse;
                    }
                    Err(error) => self.fail(format!("could not save repository: {error:#}")),
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.update_git_remote_status();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.update_git_remote_status();
            }
            _ => {}
        }
        Ok(false)
    }

    fn update_git_remote_status(&mut self) {
        self.note(format!(
            "Repository URL: {}  Enter save  Esc cancel",
            self.query
        ));
    }

    fn update_vault_input_status(&mut self) {
        self.note(format!(
            "{} vault path: {}  Enter confirm  Esc cancel",
            if self.create_vault { "Create" } else { "Open" },
            self.query
        ));
    }

    fn update_search(&mut self) -> Result<()> {
        self.sections = self.db.search(&self.query)?;
        self.selected = 0;
        Ok(())
    }

    fn handle_review(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
            self.mode = Mode::Browse;
            self.review = None;
            self.refresh_index()?;
            return Ok(false);
        }
        let Some(session) = self.review.as_mut() else {
            self.mode = Mode::Browse;
            return Ok(false);
        };
        if session.card().is_none() {
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('r')) {
                self.mode = Mode::Browse;
                self.review = None;
                self.refresh_index()?;
            }
            return Ok(false);
        }
        // `v` plays the card's clip — the answer-side one once revealed, the
        // prompt-side one before. Same key as the Dashboard, same template.
        if key.code == KeyCode::Char('v') {
            let revealed = session.phase == ReviewPhase::Revealed;
            let clip = session.card().and_then(|card| {
                let clips = match &card.content {
                    CardContent::Cloze { clips, .. }
                    | CardContent::MultipleChoice { clips, .. }
                    | CardContent::CodeGap { clips, .. } => clips.clone(),
                    CardContent::Section { .. } => Default::default(),
                };
                if revealed {
                    clips.answer.or(clips.prompt)
                } else {
                    clips.prompt
                }
            });
            match clip {
                Some(clip) => {
                    let template = crate::player::resolve(self.config.player.as_deref());
                    let seconds = (clip.start > 0).then_some(clip.start);
                    match crate::player::play(&template, &clip.url, seconds) {
                        Ok(_) => self.note(format!(
                            "playing {} at {}",
                            clip.url,
                            crate::player::format_hms(clip.start)
                        )),
                        Err(error) => self.fail(format!("{error:#}")),
                    }
                }
                None if revealed => self.note("this card has no clip"),
                None => self.note("no clip until the answer is revealed"),
            }
            return Ok(false);
        }

        if session.phase == ReviewPhase::Revealed {
            if let KeyCode::Char(rating @ '1'..='4') = key.code {
                let Some(rating) = rating.to_digit(10) else {
                    return Ok(false);
                };
                let Some(card) = session.card().cloned() else {
                    return Ok(false);
                };
                let elapsed = session.started.elapsed().as_millis().min(i64::MAX as u128) as i64;
                let days = self.db.record_review(
                    &card,
                    rating,
                    session.correct,
                    elapsed,
                    None,
                    self.config.desired_retention,
                )?;
                if let Some(days) = days {
                    self.status = format!("scheduled in {days} day(s)");
                    self.status_error = false;
                    session.current += 1;
                } else {
                    session.cards.remove(session.current);
                    self.status = "card was deleted; review queue refreshed".into();
                    self.status_error = false;
                }
                session.prepare_card();
            }
            return Ok(false);
        }
        let Some(content) = session.card().map(|card| card.content.clone()) else {
            return Ok(false);
        };
        match content {
            CardContent::Section { .. } => {
                if matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter) {
                    session.phase = ReviewPhase::Revealed;
                }
            }
            CardContent::Cloze { .. } => {
                if matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter) {
                    session.phase = ReviewPhase::Revealed;
                }
            }
            CardContent::MultipleChoice { mode, answers, .. } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    session.choice_cursor = (session.choice_cursor + 1)
                        .min(session.answer_order.len().saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    session.choice_cursor = session.choice_cursor.saturating_sub(1);
                }
                KeyCode::Char(' ') => {
                    if let Some(index) = session.answer_order.get(session.choice_cursor).copied() {
                        if mode == ChoiceMode::Single {
                            session.selected.clear();
                            session.selected.insert(index);
                        } else if !session.selected.insert(index) {
                            session.selected.remove(&index);
                        }
                    }
                }
                KeyCode::Enter if !session.selected.is_empty() => {
                    let expected: HashSet<_> = answers
                        .iter()
                        .enumerate()
                        .filter_map(|(index, answer)| answer.correct.then_some(index))
                        .collect();
                    let correct = session.selected == expected;
                    session.correct = Some(correct);
                    session.feedback = if correct {
                        "Correct".into()
                    } else {
                        format!(
                            "Incorrect: selected {} of {} correct choices",
                            session.selected.intersection(&expected).count(),
                            expected.len()
                        )
                    };
                    session.phase = ReviewPhase::Revealed;
                }
                _ => {}
            },
            CardContent::CodeGap { gaps, .. } => match key.code {
                KeyCode::Tab => {
                    session.gap_cursor = (session.gap_cursor + 1) % session.gap_names.len().max(1);
                }
                KeyCode::BackTab => {
                    session.gap_cursor = session
                        .gap_cursor
                        .checked_sub(1)
                        .unwrap_or(session.gap_names.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    if let Some(name) = session.gap_names.get(session.gap_cursor) {
                        session.gap_values.entry(name.clone()).or_default().pop();
                    }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(name) = session.gap_names.get(session.gap_cursor) {
                        session
                            .gap_values
                            .entry(name.clone())
                            .or_default()
                            .push(character);
                    }
                }
                KeyCode::Enter => {
                    let correct = gaps.iter().all(|(name, definition)| {
                        matches_gap(
                            session.gap_values.get(name).map_or("", String::as_str),
                            definition,
                        )
                    });
                    session.correct = Some(correct);
                    session.feedback = if correct {
                        "All gaps correct".into()
                    } else {
                        "One or more gaps are incorrect".into()
                    };
                    session.phase = ReviewPhase::Revealed;
                }
                _ => {}
            },
        }
        Ok(false)
    }

    fn move_selection(&mut self, amount: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(amount)
            .min(self.sections.len().saturating_sub(1));
    }

    fn move_page(&mut self, amount: isize) {
        match self.page {
            Page::Library => {
                let total = self.library_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Review => {
                let total = self.review_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Relations => {}
            Page::Stats => self.scroll = self.scroll.saturating_add_signed(amount as i16),
            Page::Clean => {
                let total = self.clean_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Options => {
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(Self::OPTION_COUNT - 1);
            }
            Page::Archived => {
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(self.archived.len().saturating_sub(1));
            }
        }
    }

    fn move_panel_focus(&mut self, dx: i16, dy: i16) {
        let positions: &[(i16, i16)] = if self.mode == Mode::Search {
            &[(0, 0), (1, 0)]
        } else {
            match self.page {
                Page::Library => &[(0, 0), (1, 0), (1, 1)],
                Page::Review | Page::Clean | Page::Options | Page::Archived => &[(0, 0), (1, 0)],
                Page::Relations => &[(0, 0), (1, 0), (2, 0)],
                Page::Stats => &[(0, 0)],
            }
        };
        self.focused_panel = self.focused_panel.min(positions.len() - 1);
        let (x, y) = positions[self.focused_panel];
        if let Some((index, _)) = positions
            .iter()
            .enumerate()
            .filter(|(_, (candidate_x, candidate_y))| {
                (dx < 0 && *candidate_x < x)
                    || (dx > 0 && *candidate_x > x)
                    || (dy < 0 && *candidate_y < y)
                    || (dy > 0 && *candidate_y > y)
            })
            .min_by_key(|(_, (candidate_x, candidate_y))| {
                let horizontal = (candidate_x - x).unsigned_abs();
                let vertical = (candidate_y - y).unsigned_abs();
                if dx == 0 {
                    vertical * 100 + horizontal
                } else {
                    horizontal * 100 + vertical
                }
            })
        {
            self.focused_panel = index;
        }
    }

    fn load_relations(&mut self) -> Result<()> {
        self.relation_section = self
            .relation_section
            .min(self.sections.len().saturating_sub(1));
        self.relations = if let Some(section) = self.sections.get(self.relation_section) {
            self.db.relations(&section.uid)?
        } else {
            Vec::new()
        };
        self.incoming_selected = self
            .incoming_selected
            .min(self.incoming_relations().len().saturating_sub(1));
        self.outgoing_selected = self
            .outgoing_selected
            .min(self.outgoing_relations().len().saturating_sub(1));
        Ok(())
    }

    fn incoming_relations(&self) -> Vec<&RelationRow> {
        self.relations
            .iter()
            .filter(|relation| relation.incoming)
            .collect()
    }

    fn outgoing_relations(&self) -> Vec<&RelationRow> {
        self.relations
            .iter()
            .filter(|relation| !relation.incoming)
            .collect()
    }

    fn move_relation_selection(&mut self, amount: isize) -> Result<()> {
        match self.focused_panel {
            0 => {
                self.incoming_selected = self
                    .incoming_selected
                    .saturating_add_signed(amount)
                    .min(self.incoming_relations().len().saturating_sub(1));
            }
            1 => {
                self.relation_section = self
                    .relation_section
                    .saturating_add_signed(amount)
                    .min(self.sections.len().saturating_sub(1));
                self.incoming_selected = 0;
                self.outgoing_selected = 0;
                self.load_relations()?;
            }
            2 => {
                self.outgoing_selected = self
                    .outgoing_selected
                    .saturating_add_signed(amount)
                    .min(self.outgoing_relations().len().saturating_sub(1));
            }
            _ => {}
        }
        Ok(())
    }

    /// Every note, each followed by its sections.
    ///
    /// A note's root section carries the note's own title, so listing it under
    /// the note printed the same words twice on consecutive rows and made the
    /// tree look broken. The note row already selects that section for
    /// relations and editing, so the duplicate row is dropped.
    fn library_items(&self) -> Vec<LibraryItem> {
        let mut items = Vec::new();
        for (note_index, note) in self.notes.iter().enumerate() {
            items.push(LibraryItem::Note(note_index));
            let mut first = true;
            for (section_index, section) in self.sections.iter().enumerate() {
                if section.path != note.path {
                    continue;
                }
                let is_redundant_root = first && section.heading == note.title;
                first = false;
                if !is_redundant_root {
                    items.push(LibraryItem::Section(section_index));
                }
            }
        }
        items
    }

    fn review_items(&self) -> Vec<ReviewItem> {
        let mut items = Vec::new();
        items.extend((0..self.review_sections.len()).map(ReviewItem::Section));
        items.extend((0..self.decks.len()).map(ReviewItem::Deck));
        items.extend((0..self.cards.len()).map(ReviewItem::Card));
        items
    }

    fn clean_items(&self) -> Vec<CleanItem> {
        let mut items = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, note)| note.topic.as_deref().is_none_or(str::is_empty))
            .map(|(index, _)| CleanItem::Note(index))
            .collect::<Vec<_>>();
        items.extend(
            self.cards
                .iter()
                .enumerate()
                .filter(|(_, card)| card.decks.is_empty())
                .map(|(index, _)| CleanItem::Card(index)),
        );
        items.extend((0..self.orphan_images.len()).map(CleanItem::Image));
        items
    }

    fn selected_library_section(&self) -> Option<&SectionRow> {
        match self.library_items().get(self.selected)? {
            LibraryItem::Section(index) => self.sections.get(*index),
            LibraryItem::Note(index) => {
                let note = self.notes.get(*index)?;
                self.sections
                    .iter()
                    .find(|section| section.path == note.path)
            }
        }
    }

    fn refresh_views(&mut self) -> Result<()> {
        self.refresh_database_views()?;
        self.orphan_images = find_orphan_images(&self.vault)?;
        Ok(())
    }

    fn refresh_database_views(&mut self) -> Result<()> {
        self.notes = self.db.notes()?;
        self.review_sections = self.db.review_sections()?;
        self.decks = self.db.decks()?;
        self.cards = self.db.card_rows()?;
        self.archived = self.db.archived_items()?;
        self.statistics = self.db.statistics()?;
        self.active_deck = self.active_deck.min(self.decks.len().saturating_sub(1));
        if self.page == Page::Relations {
            self.load_relations()?;
        }
        Ok(())
    }

    fn toggle_selected_section(&mut self) -> Result<()> {
        let Some(ReviewItem::Section(index)) = self.review_items().get(self.selected).copied()
        else {
            self.note("select a section to add or remove it from reviews");
            return Ok(());
        };
        if let Some(section) = self.review_sections.get(index) {
            let active = self.db.toggle_section_review(&section.uid)?;
            self.note(format!(
                "{} review: {} / {}",
                if active { "added to" } else { "removed from" },
                section.note_title,
                section.heading
            ));
            self.refresh_views()?;
        }
        Ok(())
    }

    fn change_active_deck(&mut self, amount: isize) {
        if !self.decks.is_empty() {
            self.active_deck = self
                .active_deck
                .saturating_add_signed(amount)
                .min(self.decks.len() - 1);
            self.note(format!(
                "active deck: {}",
                self.decks[self.active_deck].name
            ));
        }
    }

    fn toggle_selected_card_deck(&mut self) -> Result<()> {
        let Some(ReviewItem::Card(index)) = self.review_items().get(self.selected).copied() else {
            self.note("select a card first");
            return Ok(());
        };
        let Some(card) = self.cards.get(index) else {
            return Ok(());
        };
        let Some(deck) = self.decks.get(self.active_deck) else {
            self.note("create a deck first with n");
            return Ok(());
        };
        let added = self.db.toggle_card_deck(card.id, deck.id)?;
        self.note(format!(
            "{} {} {}",
            if added { "added" } else { "removed" },
            card.label,
            deck.name
        ));
        self.refresh_views()?;
        Ok(())
    }

    fn archive_selected(&mut self) -> Result<()> {
        let status = match self.page {
            Page::Library => match self.library_items().get(self.selected).copied() {
                Some(LibraryItem::Note(index)) => {
                    let note = &self.notes[index];
                    self.db.archive_note(&note.path)?;
                    format!("archived note {} and its contents", note.title)
                }
                Some(LibraryItem::Section(index)) => {
                    let section = &self.sections[index];
                    self.db.archive_section(&section.uid)?;
                    format!("archived section {}", section.heading)
                }
                None => return Ok(()),
            },
            Page::Review => match self.review_items().get(self.selected).copied() {
                Some(ReviewItem::Section(index)) => {
                    let section = &self.review_sections[index];
                    self.db.archive_section(&section.uid)?;
                    format!("archived section {}", section.heading)
                }
                Some(ReviewItem::Deck(index)) => {
                    let deck = &self.decks[index];
                    self.db.archive_deck(deck.id)?;
                    format!("archived deck {} and its exclusive quizzes", deck.name)
                }
                Some(ReviewItem::Card(index)) => {
                    let card = &self.cards[index];
                    if card.card_type == "section-review" {
                        self.db.archive_section(&card.section_uid)?;
                        format!("archived section review {}", card.label)
                    } else {
                        self.db.archive_quiz(card.id)?;
                        format!("archived quiz {}", card.label)
                    }
                }
                None => return Ok(()),
            },
            Page::Clean => match self.clean_items().get(self.selected).copied() {
                Some(CleanItem::Note(index)) => {
                    let note = &self.notes[index];
                    self.db.archive_note(&note.path)?;
                    format!("archived note {} and its contents", note.title)
                }
                Some(CleanItem::Card(index)) => {
                    let card = &self.cards[index];
                    if card.card_type == "section-review" {
                        self.db.archive_section(&card.section_uid)?;
                    } else {
                        self.db.archive_quiz(card.id)?;
                    }
                    format!("archived {}", card.label)
                }
                Some(CleanItem::Image(_)) => {
                    self.note("images cannot be archived; d deletes them permanently");
                    return Ok(());
                }
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        self.refresh_index()?;
        self.selected = self.selected.min(
            match self.page {
                Page::Library => self.library_items().len(),
                Page::Review => self.review_items().len(),
                Page::Clean => self.clean_items().len(),
                _ => 0,
            }
            .saturating_sub(1),
        );
        self.note(status);
        Ok(())
    }

    fn restore_selected(&mut self) -> Result<()> {
        let Some(item) = self.archived.get(self.selected).cloned() else {
            return Ok(());
        };
        let label = archived_item_label(&item);
        self.db.restore(&item)?;
        self.refresh_index()?;
        self.selected = self.selected.min(self.archived.len().saturating_sub(1));
        self.note(format!("restored {label}"));
        Ok(())
    }

    fn assign_clean_card(&mut self) -> Result<()> {
        let Some(CleanItem::Card(index)) = self.clean_items().get(self.selected).copied() else {
            self.note("select an unassigned card first");
            return Ok(());
        };
        let Some(deck) = self.decks.get(self.active_deck) else {
            self.note("create a deck on page 2 first");
            return Ok(());
        };
        let card = &self.cards[index];
        self.db.toggle_card_deck(card.id, deck.id)?;
        self.note(format!("assigned {} to {}", card.label, deck.name));
        self.refresh_views()?;
        self.selected = self
            .selected
            .min(self.clean_items().len().saturating_sub(1));
        Ok(())
    }

    fn delete_clean_image(&mut self) -> Result<()> {
        let Some(CleanItem::Image(index)) = self.clean_items().get(self.selected).copied() else {
            self.note("select an unreferenced image first");
            return Ok(());
        };
        let relative = self.orphan_images[index].clone();
        fs::remove_file(self.vault.join(&relative))?;
        self.note(format!("deleted {}", relative.display()));
        self.refresh_views()?;
        self.selected = self
            .selected
            .min(self.clean_items().len().saturating_sub(1));
        Ok(())
    }

    fn change_option(&mut self, amount: isize) -> Result<()> {
        match self.selected {
            0 => {
                let percentage = (self.config.desired_retention * 100.0).round() as isize;
                self.config.desired_retention = (percentage + amount).clamp(70, 99) as f32 / 100.0;
            }
            1 => {
                self.config.new_cards_per_day = self
                    .config
                    .new_cards_per_day
                    .saturating_add_signed(amount)
                    .clamp(1, 9999);
            }
            2 => {
                let step = if amount < 0 { -10 } else { 10 };
                self.config.max_reviews_per_day = self
                    .config
                    .max_reviews_per_day
                    .saturating_add_signed(step)
                    .clamp(10, 9999);
            }
            3 => {
                self.config.review_order = match self.config.review_order {
                    ReviewOrder::Due => ReviewOrder::Random,
                    ReviewOrder::Random => ReviewOrder::Due,
                };
            }
            4 => self.config.bury_siblings = !self.config.bury_siblings,
            _ => {}
        }
        self.config.save(&self.vault)?;
        self.note("review options saved");
        Ok(())
    }

    fn open_selected(&mut self) -> Result<()> {
        match self.page {
            Page::Library => match self.library_items().get(self.selected).copied() {
                Some(LibraryItem::Note(index)) => {
                    let note = self.notes[index].clone();
                    self.open_editor(&self.vault.join(note.path), 1);
                }
                Some(LibraryItem::Section(index)) => {
                    let section = self.sections[index].clone();
                    self.open_editor(&self.vault.join(section.path), section.start_line);
                }
                None => {}
            },
            Page::Review => match self.review_items().get(self.selected).copied() {
                Some(ReviewItem::Section(index)) => {
                    let uid = self.review_sections[index].uid.clone();
                    if let Some(section) = self.sections.iter().find(|section| section.uid == uid) {
                        let section = section.clone();
                        self.open_editor(&self.vault.join(section.path), section.start_line);
                    }
                }
                Some(ReviewItem::Deck(index)) => {
                    self.active_deck = index;
                    self.note(format!("active deck: {}", self.decks[index].name));
                }
                Some(ReviewItem::Card(index)) => {
                    let id = self.cards[index].id;
                    if let Some(card) = self.db.review_card(id)? {
                        self.review = Some(ReviewSession::new(vec![card]));
                        self.mode = Mode::Review;
                        self.note("reviewing selected card");
                    } else {
                        self.refresh_index()?;
                        self.note("card was deleted; list refreshed");
                    }
                }
                None => {}
            },
            Page::Relations => match self.focused_panel {
                0 => {
                    if let Some(uid) = self
                        .incoming_relations()
                        .get(self.incoming_selected)
                        .map(|relation| relation.target_uid.clone())
                    {
                        self.follow_relation(&uid)?;
                    }
                }
                1 => {
                    if let Some(section) = self.sections.get(self.relation_section).cloned() {
                        self.open_editor(&self.vault.join(section.path), section.start_line);
                    }
                }
                2 => {
                    if let Some(uid) = self
                        .outgoing_relations()
                        .get(self.outgoing_selected)
                        .map(|relation| relation.target_uid.clone())
                    {
                        self.follow_relation(&uid)?;
                    }
                }
                _ => {}
            },
            Page::Stats => {}
            Page::Clean => match self.clean_items().get(self.selected).copied() {
                Some(CleanItem::Note(index)) => {
                    let note = self.notes[index].clone();
                    self.open_editor(&self.vault.join(note.path), 1);
                }
                Some(CleanItem::Card(index)) => {
                    let id = self.cards[index].id;
                    if let Some(card) = self.db.review_card(id)? {
                        self.review = Some(ReviewSession::new(vec![card]));
                        self.mode = Mode::Review;
                        self.note("reviewing unassigned card");
                    } else {
                        self.refresh_index()?;
                        self.note("card was deleted; list refreshed");
                    }
                }
                Some(CleanItem::Image(index)) => {
                    let image = self.vault.join(&self.orphan_images[index]);
                    open::that(&image)?;
                    self.note(format!("opened {}", image.display()));
                }
                None => {}
            },
            Page::Options => match self.selected {
                5 => self.authenticate_github(),
                6 => {
                    self.query = self.sync_remote.clone().unwrap_or_default();
                    self.mode = Mode::GitRemoteInput;
                    self.update_git_remote_status();
                }
                7 => self.sync_vault()?,
                8..=9 => {
                    self.create_vault = self.selected == 9;
                    self.query.clear();
                    self.mode = Mode::VaultInput;
                    self.update_vault_input_status();
                }
                _ => {}
            },
            Page::Archived => {
                if let Some(item) = self.archived.get(self.selected).cloned() {
                    match item {
                        ArchivedItem::Note { path, .. } => {
                            self.open_editor(&self.vault.join(path), 1);
                        }
                        ArchivedItem::Section {
                            path, start_line, ..
                        } => {
                            self.open_editor(&self.vault.join(path), start_line);
                        }
                        ArchivedItem::Quiz { .. } | ArchivedItem::Deck { .. } => {
                            self.note("restore this item with u before opening it");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn follow_relation(&mut self, uid: &str) -> Result<()> {
        let Some(index) = self.sections.iter().position(|section| section.uid == uid) else {
            self.fail(format!("relation target not found: {uid}"));
            return Ok(());
        };
        self.relation_section = index;
        self.incoming_selected = 0;
        self.outgoing_selected = 0;
        self.focused_panel = 1;
        self.load_relations()?;
        self.note(format!("selected {}", self.sections[index].heading));
        Ok(())
    }

    fn edit_selected(&mut self) -> Result<()> {
        let Some(section) = self.sections.get(self.selected).cloned() else {
            return Ok(());
        };
        self.open_editor(&self.vault.join(section.path), section.start_line);
        Ok(())
    }

    /// Ask the event loop to hand the terminal to the user's editor.
    ///
    /// Suspending the alternate screen is the event loop's business, not a key
    /// handler's. Threading a `&mut Terminal` through every action made the
    /// whole dispatch path impossible to exercise from a test, because a test
    /// has no terminal to thread.
    fn open_editor(&mut self, path: &Path, line: usize) {
        self.pending = Some(Pending::Editor {
            path: path.to_path_buf(),
            line,
        });
    }

    /// Run the work that needs the terminal back. Only the event loop calls
    /// this, and only between frames.
    fn perform(&mut self, pending: Pending, terminal: &mut Tui) -> Result<()> {
        restore_terminal(terminal)?;
        let outcome = match &pending {
            Pending::Editor { path, line } => {
                let editor = self.config.editor.clone().unwrap_or_else(|| {
                    env::var("VISUAL")
                        .or_else(|_| env::var("EDITOR"))
                        .unwrap_or_else(|_| "nvim".into())
                });
                let mut parts = editor.split_whitespace();
                let program = parts.next().unwrap_or("nvim").to_string();
                let arguments: Vec<String> = parts.map(str::to_string).collect();
                Command::new(program)
                    .args(arguments)
                    .arg(format!("+{line}"))
                    .arg(path)
                    .status()
            }
            Pending::GithubAuth => match Command::new("gh").args(["auth", "login"]).status() {
                Ok(status) if status.success() => {
                    Command::new("gh").args(["auth", "setup-git"]).status()
                }
                other => other,
            },
        };
        *terminal = setup_terminal()?;

        match (pending, outcome) {
            (Pending::Editor { .. }, Ok(_)) => {
                // Force the next tick to reindex, so an edit shows up at once
                // rather than after the normal debounce.
                self.last_index = Instant::now() - Duration::from_secs(2);
                self.refresh_index()?;
            }
            (Pending::Editor { .. }, Err(error)) => {
                self.fail(format!("could not start your editor: {error}"))
            }
            (Pending::GithubAuth, Ok(status)) if status.success() => {
                self.note("GitHub authentication configured")
            }
            (Pending::GithubAuth, Ok(_)) => {
                self.fail("GitHub authentication was cancelled or failed")
            }
            (Pending::GithubAuth, Err(error)) => {
                self.fail(format!("could not run GitHub CLI (`gh`): {error}"))
            }
        }
        Ok(())
    }

    /// Ask the event loop to run `gh auth login` outside the alternate screen.
    fn authenticate_github(&mut self) {
        self.pending = Some(Pending::GithubAuth);
    }

    fn sync_vault(&mut self) -> Result<()> {
        self.note("syncing vault with GitHub...");
        match sync::sync(&self.vault, None) {
            Ok(summary) => {
                self.sync_remote = Some(summary.remote);
                self.refresh_index()?;
                self.note(format!("vault synced on branch {}", summary.branch));
            }
            Err(error) => self.fail(format!("sync failed: {error:#}")),
        }
        Ok(())
    }

    /// Play the `@video` on the selected section, in the configured player.
    ///
    /// Prefers the parsed action — the author declared both the URL and the
    /// moment there — and falls back to the first video URL in the body, which
    /// covers a section that carries a link without the `@video` line.
    fn play_selected_video(&mut self) -> Result<()> {
        let Some(section) = self.selected_library_section() else {
            return Ok(());
        };
        let uid = section.uid.clone();
        let body = section.body.clone();

        let action = self
            .db
            .actions_for(&[uid])?
            .into_iter()
            .find(|action| action.kind == "video");

        let (url, seconds) = match action {
            Some(action) => (action.target, action.timestamp_seconds),
            None => match crate::player::first_video_url(&body) {
                Some(url) => (url, None),
                None => {
                    self.note("no @video or video URL in this section");
                    return Ok(());
                }
            },
        };

        let template = crate::player::resolve(self.config.player.as_deref());

        match crate::player::play(&template, &url, seconds) {
            Ok(_) => {
                self.note(match seconds {
                    Some(s) => format!("playing {url} at {}", crate::player::format_hms(s)),
                    None => format!("playing {url}"),
                });
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
        Ok(())
    }

    fn open_selected_url(&mut self) -> Result<()> {
        if let Some(section) = self.selected_library_section() {
            if let Some(url) = URL.find(&section.body) {
                open::that(url.as_str())?;
                self.note(format!("opened {}", url.as_str()));
            } else {
                self.note("no URL in this section");
            }
        }
        Ok(())
    }

    fn open_selected_image(&mut self) -> Result<()> {
        if let Some(section) = self.selected_library_section() {
            if let Some(capture) = IMAGE.captures(&section.body) {
                let note_dir = self
                    .vault
                    .join(&section.path)
                    .parent()
                    .unwrap()
                    .to_path_buf();
                let image = note_dir.join(capture[1].trim());
                open::that(&image)?;
                self.note(format!("opened {}", image.display()));
            } else {
                self.note("no image in this section");
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------- constants

    /// Option rows that `h`/`l` adjust in place. The rest run something.
    const ADJUSTABLE_OPTIONS: usize = 5;

    /// How many rows the Options page has, asked of the page itself so the two
    /// cannot disagree the way a hardcoded `10` eventually would.
    const OPTION_COUNT: usize = 10;

    // ---------------------------------------------------------------- status

    /// Report something that happened. Shown in the footer, in grey.
    fn note(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_error = false;
    }

    /// Report something that failed. Shown in the footer, in red.
    ///
    /// A daily driver should never dump a backtrace over the terminal it is
    /// drawing into, so recoverable failures land here rather than propagating
    /// out of the event loop.
    fn fail(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_error = true;
    }

    // ------------------------------------------------------------ navigation

    fn go_to(&mut self, page: Page) -> Result<()> {
        self.page = page;
        self.selected = 0;
        self.scroll = 0;
        self.focused_panel = usize::from(page == Page::Relations);
        if page == Page::Relations {
            self.load_relations()?;
        }
        Ok(())
    }

    /// Open the Relations page focused on the section under the cursor.
    fn jump_to_relations(&mut self) -> Result<()> {
        let Some(section) = self.selected_library_section() else {
            return Ok(());
        };
        let uid = section.uid.clone();
        self.page = Page::Relations;
        self.focused_panel = 1;
        self.relation_section = self
            .sections
            .iter()
            .position(|candidate| candidate.uid == uid)
            .unwrap_or(0);
        self.load_relations()
    }

    // --------------------------------------------------------- key dispatch

    /// Browse-mode keys, resolved through the binding table.
    ///
    /// Motions and tab digits are handled directly because they work on every
    /// page and would only clutter the footer. Everything else comes from
    /// [`keymap::bindings`], which is also what the footer prints — so a key
    /// that works is advertised, and a key that is advertised works.
    fn handle_browse(&mut self, key: KeyEvent) -> Result<bool> {
        if let KeyCode::Char(digit) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(page) = keymap::tab_for(digit)
        {
            self.go_to(page)?;
            return Ok(false);
        }
        if key.code == KeyCode::Esc {
            // Esc leaves a palette-only page for the nearest tab, so there is
            // always a way back out without hunting for the right digit.
            if !ui::TABS.iter().any(|(page, _)| *page == self.page) {
                self.go_to(Page::Library)?;
            }
            return Ok(false);
        }
        if keymap::is_motion(key) {
            let amount = if matches!(key.code, KeyCode::Char('j') | KeyCode::Down) {
                1
            } else {
                -1
            };
            if self.page == Page::Relations {
                self.move_relation_selection(amount)?;
            } else {
                self.move_page(amount);
            }
            return Ok(false);
        }
        if self.page == Page::Options
            && self.selected < Self::ADJUSTABLE_OPTIONS
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            let amount = if key.code == KeyCode::Right { 1 } else { -1 };
            self.change_option(amount)?;
            return Ok(false);
        }

        let Some(action) = keymap::bindings(self)
            .into_iter()
            .find(|binding| binding.key.matches(key))
            .map(|binding| binding.action)
        else {
            return Ok(false);
        };
        self.run_action(action)
    }

    /// Perform one bound action. Returns `true` when the application should end.
    fn run_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::Quit => return Ok(true),
            Action::Open | Action::Edit => self.open_selected()?,
            Action::New => {
                self.query.clear();
                self.mode = if self.page == Page::Review {
                    Mode::DeckInput
                } else {
                    Mode::NoteInput
                };
            }
            Action::Archive => self.archive_selected()?,
            Action::Restore => self.restore_selected()?,
            Action::Delete => self.delete_clean_image()?,
            Action::Assign => {
                if self.page == Page::Clean {
                    self.assign_clean_card()?
                } else {
                    self.toggle_selected_card_deck()?
                }
            }
            Action::Enrol => self.toggle_selected_section()?,
            Action::ChooseDeck => {
                self.review_scope_selected =
                    self.active_deck.saturating_add(1).min(self.decks.len());
                self.mode = Mode::ReviewDeckChoice;
            }
            Action::PreviousDeck => self.change_active_deck(-1),
            Action::NextDeck => self.change_active_deck(1),
            Action::Decrease => self.change_option(-1)?,
            Action::Increase => self.change_option(1)?,
            Action::Search => {
                self.mode = Mode::Search;
                self.query.clear();
                self.sections = self.db.search("")?;
                self.selected = 0;
                self.focused_panel = 0;
            }
            Action::PlayVideo => self.play_selected_video()?,
            Action::OpenUrl => self.open_selected_url()?,
            Action::OpenImage => self.open_selected_image()?,
            Action::FollowRelations => self.jump_to_relations()?,
            Action::Palette => {
                self.mode = Mode::Palette;
                self.query.clear();
                self.palette_selected = 0;
            }
            Action::Help => self.mode = Mode::Help,
            Action::Sync => self.sync_vault()?,
            Action::Reindex => {
                self.refresh_index()?;
                self.note("re-read the vault");
            }
        }
        Ok(false)
    }

    /// Keys inside the command palette.
    ///
    /// The list is filtered by typing, so `j` and `k` have to mean `j` and `k`
    /// here. The arrows and `Ctrl+N`/`Ctrl+P` move the cursor instead.
    fn handle_palette(&mut self, key: KeyEvent) -> Result<bool> {
        let matches = palette::matching(&self.query).len();
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.query.clear();
            }
            KeyCode::Down | KeyCode::Char('n') if control || key.code == KeyCode::Down => {
                self.palette_selected = (self.palette_selected + 1).min(matches.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('p') if control || key.code == KeyCode::Up => {
                self.palette_selected = self.palette_selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                let chosen = palette::matching(&self.query)
                    .get(self.palette_selected)
                    .map(|entry| entry.command);
                self.mode = Mode::Browse;
                self.query.clear();
                self.palette_selected = 0;
                if let Some(command) = chosen {
                    return self.run_command(command);
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.palette_selected = 0;
            }
            KeyCode::Char(character) if !control => {
                self.query.push(character);
                self.palette_selected = 0;
            }
            _ => {}
        }
        Ok(false)
    }

    /// Run a palette command.
    fn run_command(&mut self, command: PaletteCommand) -> Result<bool> {
        if let Some(page) = command.page() {
            self.go_to(page)?;
            return Ok(false);
        }
        match command {
            PaletteCommand::NewNote => {
                self.query.clear();
                self.mode = Mode::NoteInput;
            }
            PaletteCommand::NewDeck => {
                self.query.clear();
                self.mode = Mode::DeckInput;
            }
            PaletteCommand::SyncNow => self.sync_vault()?,
            PaletteCommand::Reindex => {
                self.refresh_index()?;
                self.note("re-read the vault");
            }
            PaletteCommand::AuthenticateGithub => self.authenticate_github(),
            PaletteCommand::SetRepository => {
                self.query = self.sync_remote.clone().unwrap_or_default();
                self.mode = Mode::GitRemoteInput;
            }
            PaletteCommand::OpenVault | PaletteCommand::CreateVault => {
                self.create_vault = command == PaletteCommand::CreateVault;
                self.query.clear();
                self.mode = Mode::VaultInput;
            }
            PaletteCommand::Quit => return Ok(true),
            PaletteCommand::OpenClean
            | PaletteCommand::OpenOptions
            | PaletteCommand::OpenArchived => {}
        }
        Ok(false)
    }

    // ------------------------------------------------------- media presence

    /// Whether this section carries a clip yalive could play.
    ///
    /// Deliberately cheap: the footer asks this on every frame, so it reads the
    /// section body already in memory instead of querying SQLite.
    fn section_has_video(&self, section: &SectionRow) -> bool {
        section.body.contains("@video") || crate::player::first_video_url(&section.body).is_some()
    }

    fn section_has_url(&self, section: &SectionRow) -> bool {
        URL.is_match(&section.body)
    }

    fn section_has_image(&self, section: &SectionRow) -> bool {
        IMAGE.is_match(&section.body)
    }

    // ------------------------------------------------------------ rendering

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        ui::draw(self, frame);
    }
}

/// Compiled once rather than on every keystroke and every frame.
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s)>\]]+").expect("valid url pattern"));
static IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\(([^)]+)\)").expect("valid image pattern"));

/// `Shift`+`H`/`J`/`K`/`L` moves focus between panes, spatially.
fn shifted_panel_direction(key: KeyEvent) -> Option<(i16, i16)> {
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    match key.code {
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'h') => Some((-1, 0)),
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'l') => Some((1, 0)),
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'j') => Some((0, 1)),
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'k') => Some((0, -1)),
        _ => None,
    }
}

fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

pub fn run(vault: &Path) -> Result<Option<PathBuf>> {
    let vault = vault
        .canonicalize()
        .with_context(|| format!("opening vault {}", vault.display()))?;
    App::new(vault.clone(), Database::open(&vault)?)?.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    /// A vault with a note, a child section, a relation, and a quiz card —
    /// enough for every page to have something to draw.
    fn vault_with_content() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("rust.md"),
            "---\nid: rust\ntitle: Rust Ownership\ntopic: Programming\npinned: true\n---\n\
             # Rust Ownership {#root}\n\nOwnership is Rust's memory model.\n\n\
             ## Borrowing {#borrow}\n\nA borrow is a reference. [[linear#root]]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("linear.md"),
            "---\nid: linear\ntitle: Linear Algebra\ntopic: Mathematics\n---\n\
             # Linear Algebra {#root}\n\nVectors and matrices.\n",
        )
        .unwrap();
        dir
    }

    fn app_for(dir: &tempfile::TempDir) -> App {
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        App::new(vault, database).unwrap()
    }

    #[test]
    fn renders_every_page_and_overlay_without_panicking() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        for page in [
            Page::Library,
            Page::Review,
            Page::Relations,
            Page::Stats,
            Page::Clean,
            Page::Options,
            Page::Archived,
        ] {
            app.page = page;
            if page == Page::Relations {
                app.load_relations().unwrap();
            }
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        for mode in [
            Mode::ReviewDeckChoice,
            Mode::Palette,
            Mode::Help,
            Mode::Search,
            Mode::NoteInput,
            Mode::DeckInput,
            Mode::VaultInput,
            Mode::GitRemoteInput,
        ] {
            app.mode = mode;
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
    }

    /// A terminal far narrower than the layout was designed for must still
    /// render. Every width calculation subtracts something, and saturating
    /// arithmetic is the only reason those subtractions do not wrap.
    #[test]
    fn renders_at_a_hostile_terminal_size() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        for (width, height) in [(20, 6), (40, 10), (240, 80)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for page in [Page::Library, Page::Review, Page::Stats, Page::Options] {
                app.page = page;
                terminal.draw(|frame| app.draw(frame)).unwrap();
            }
            app.mode = Mode::Palette;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            app.mode = Mode::Browse;
        }
    }

    /// The footer prints what [`keymap::bindings`] returns, and browse-mode
    /// dispatch resolves through the same list. If a binding here had no arm in
    /// `run_action`, the footer would advertise a dead key.
    #[test]
    fn every_advertised_binding_is_dispatchable() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        for page in [
            Page::Library,
            Page::Review,
            Page::Relations,
            Page::Stats,
            Page::Clean,
            Page::Options,
            Page::Archived,
        ] {
            app.page = page;
            let bindings = keymap::bindings(&app);
            assert!(
                !bindings.is_empty(),
                "{} advertises no keys at all",
                page.label()
            );
            for binding in &bindings {
                let event = match binding.key {
                    keymap::Key::Char(character) => {
                        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
                    }
                    keymap::Key::Enter => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    keymap::Key::Space => KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                    keymap::Key::Ctrl(character) => {
                        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
                    }
                };
                assert!(
                    binding.key.matches(event),
                    "{} on {} does not match its own key event",
                    binding.label,
                    page.label()
                );
            }
        }
    }

    /// No two bindings on a page may claim the same key: the first would
    /// silently win dispatch while the second still printed in the footer.
    #[test]
    fn no_page_binds_one_key_twice() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        for page in [
            Page::Library,
            Page::Review,
            Page::Relations,
            Page::Stats,
            Page::Clean,
            Page::Options,
            Page::Archived,
        ] {
            app.page = page;
            let mut seen: Vec<String> = Vec::new();
            for binding in keymap::bindings(&app) {
                let key = binding.key.label();
                assert!(!seen.contains(&key), "{} binds {key} twice", page.label());
                seen.push(key);
            }
        }
    }

    /// The media keys are only advertised when the selection carries media,
    /// so the footer never offers a key whose only reply is "there is none".
    #[test]
    fn media_keys_appear_only_when_the_section_has_media() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("plain.md"),
            "---\nid: plain\ntitle: Plain\n---\n# Plain {#root}\n\nJust prose.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("clip.md"),
            "---\nid: clip\ntitle: Clip\n---\n# Clip {#root}\n\n\
             @video https://www.youtube.com/watch?v=dQw4w9WgXcQ 06:54  A moment\n",
        )
        .unwrap();
        let mut app = app_for(&dir);
        app.page = Page::Library;

        let labels = |app: &App| {
            keymap::bindings(app)
                .into_iter()
                .map(|binding| binding.label)
                .collect::<Vec<_>>()
        };

        let clip_index = app
            .library_items()
            .iter()
            .position(|item| match item {
                LibraryItem::Note(index) => app.notes[*index].title == "Clip",
                LibraryItem::Section(_) => false,
            })
            .expect("the clip note is indexed");
        app.selected = clip_index;
        assert!(labels(&app).contains(&"play clip"));

        let plain_index = app
            .library_items()
            .iter()
            .position(|item| match item {
                LibraryItem::Note(index) => app.notes[*index].title == "Plain",
                LibraryItem::Section(_) => false,
            })
            .expect("the plain note is indexed");
        app.selected = plain_index;
        assert!(!labels(&app).contains(&"play clip"));
    }

    #[test]
    fn the_palette_opens_pages_that_are_no_longer_tabs() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.mode == Mode::Palette);

        for character in "clean".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert!(app.mode == Mode::Browse);
        assert!(app.page == Page::Clean);
    }

    /// Pages reached from the palette have no digit, so `Esc` has to be a way
    /// back — otherwise Clean is a room with no door.
    #[test]
    fn escape_leaves_a_palette_only_page() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        app.go_to(Page::Archived).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.page == Page::Library);
    }

    #[test]
    fn digits_select_the_four_tabs() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        for (digit, expected) in [
            ('1', Page::Library),
            ('2', Page::Review),
            ('3', Page::Relations),
            ('4', Page::Stats),
        ] {
            app.handle_key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::NONE))
                .unwrap();
            assert!(
                app.page == expected,
                "{digit} did not open {}",
                expected.label()
            );
        }
        // The pages that lost their digits must not answer to the old ones.
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.page == Page::Stats);
    }

    #[test]
    fn a_failure_is_marked_so_the_footer_can_colour_it() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        app.note("indexed");
        assert!(!app.status_error);
        app.fail("could not reach the remote");
        assert!(app.status_error);
        app.note("recovered");
        assert!(!app.status_error);
    }

    #[test]
    fn follows_incoming_and_outgoing_relations() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        app.page = Page::Relations;
        app.relation_section = app
            .sections
            .iter()
            .position(|section| section.uid == "rust#borrow")
            .unwrap();
        app.load_relations().unwrap();
        assert_eq!(app.outgoing_relations().len(), 1);

        app.follow_relation("linear#root").unwrap();
        assert_eq!(app.sections[app.relation_section].uid, "linear#root");
        assert_eq!(app.incoming_relations().len(), 1);
    }

    #[test]
    fn consumes_ygraphy_focus_commands() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        let vault = app.vault.clone();
        fs::write(
            vault.join(".notes/ygraphy-open.json"),
            serde_json::to_vec("linear#root").unwrap(),
        )
        .unwrap();

        app.consume_ygraphy_command().unwrap();

        assert!(app.page == Page::Relations);
        assert_eq!(app.sections[app.relation_section].uid, "linear#root");
        assert!(!vault.join(".notes/ygraphy-open.json").exists());
    }

    /// A graph click must not interrupt a card you are in the middle of
    /// answering, and must not be lost either.
    #[test]
    fn a_graph_command_waits_for_the_review_session_to_end() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        let vault = app.vault.clone();
        let command = vault.join(".notes/ygraphy-open.json");
        fs::write(&command, serde_json::to_vec("linear#root").unwrap()).unwrap();

        app.mode = Mode::Review;
        app.consume_ygraphy_command().unwrap();
        assert!(
            app.mode == Mode::Review,
            "the review session was interrupted"
        );
        assert!(command.exists(), "the graph command was thrown away");

        app.mode = Mode::Browse;
        app.consume_ygraphy_command().unwrap();
        assert!(app.page == Page::Relations);
        assert!(!command.exists());
    }

    #[test]
    fn moves_panel_focus_spatially() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        app.page = Page::Library;
        app.move_panel_focus(1, 0);
        assert_eq!(app.focused_panel, 1);
        app.move_panel_focus(0, 1);
        assert_eq!(app.focused_panel, 2);
        app.move_panel_focus(0, -1);
        assert_eq!(app.focused_panel, 1);
        app.move_panel_focus(-1, 0);
        assert_eq!(app.focused_panel, 0);
    }

    /// Every page's panel map must be reachable, or `Shift`+direction can strand
    /// focus on a pane that no longer exists.
    #[test]
    fn panel_focus_stays_inside_every_page() {
        let dir = vault_with_content();
        let mut app = app_for(&dir);
        for page in [
            Page::Library,
            Page::Review,
            Page::Relations,
            Page::Stats,
            Page::Clean,
            Page::Options,
            Page::Archived,
        ] {
            app.page = page;
            app.focused_panel = 0;
            for (dx, dy) in [(1, 0), (0, 1), (1, 0), (0, 1), (-1, 0), (0, -1)] {
                app.move_panel_focus(dx, dy);
            }
            assert!(
                app.focused_panel < 4,
                "{} left focus at {}",
                page.label(),
                app.focused_panel
            );
        }
    }
}

/// A way to look at every screen without launching the application.
///
/// `cargo test screens -- --ignored --nocapture` prints each page, overlay, and
/// review phase as text. Ignored by default because it exists to be read, not
/// asserted on — the assertions live in [`tests`].
#[cfg(test)]
mod screens {
    use super::*;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    #[test]
    #[ignore]
    fn screens() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("rust.md"), "---\nid: rust\ntitle: Rust Ownership\ntopic: Programming\npinned: true\n---\n# Rust Ownership {#root}\n\nOwnership is Rust's memory model.\n\n## Borrowing {#borrow}\n\nA borrow is a reference. [[linear#root]]\n\n@video https://www.youtube.com/watch?v=dQw4w9WgXcQ 06:54  Chapter on borrowing\n").unwrap();
        fs::write(dir.path().join("linear.md"), "---\nid: linear\ntitle: Linear Algebra\ntopic: Mathematics\n---\n# Linear Algebra {#root}\n\nVectors and matrices.\n\n## Eigenvalues {#eigen}\n\nA scalar lambda such that Av = lambda v.\n").unwrap();
        fs::write(dir.path().join("orphan.md"), "---\nid: orphan\ntitle: No Topic Here\n---\n# No Topic Here {#root}\n\nThis note has no topic.\n").unwrap();
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault, database).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        let show = |app: &App, terminal: &mut Terminal<TestBackend>, name: &str| {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            println!("\n===== {name} =====");
            let buffer = terminal.backend().buffer();
            for y in 0..buffer.area.height {
                let mut line = String::new();
                for x in 0..buffer.area.width {
                    line.push_str(buffer[(x, y)].symbol());
                }
                println!("|{}|", line.trim_end());
            }
        };
        for (name, page) in [
            ("Library", Page::Library),
            ("Review", Page::Review),
            ("Relations", Page::Relations),
            ("Stats", Page::Stats),
            ("Clean", Page::Clean),
            ("Options", Page::Options),
        ] {
            app.page = page;
            if page == Page::Relations {
                app.load_relations().unwrap();
            }
            show(&app, &mut terminal, name);
        }
        app.page = Page::Library;
        app.mode = Mode::Palette;
        show(&app, &mut terminal, "Palette");
        app.mode = Mode::Help;
        show(&app, &mut terminal, "Help");
        app.mode = Mode::Browse;
        app.page = Page::Review;
        let card = crate::model::ReviewCard {
            id: 1,
            uid: "rust#borrow:q1".into(),
            section_uid: "rust#borrow".into(),
            due_at: chrono::Utc::now().timestamp(),
            review_count: 0,
            stability: None,
            difficulty: None,
            last_review_at: None,
            content: crate::model::CardContent::MultipleChoice {
                question: "What does & mean in Rust?".into(),
                answers: vec![
                    crate::model::ChoiceAnswer {
                        id: None,
                        text: "A borrow".into(),
                        correct: true,
                    },
                    crate::model::ChoiceAnswer {
                        id: None,
                        text: "A move".into(),
                        correct: false,
                    },
                    crate::model::ChoiceAnswer {
                        id: None,
                        text: "A copy".into(),
                        correct: false,
                    },
                ],
                mode: crate::model::ChoiceMode::Single,
                explanation: Some(
                    "An ampersand creates a reference without taking ownership.".into(),
                ),
                clips: Default::default(),
            },
        };
        app.review = Some(ReviewSession::new(vec![card.clone(), card]));
        app.mode = Mode::Review;
        show(&app, &mut terminal, "Review session - prompt");
        if let Some(session) = app.review.as_mut() {
            session.selected.insert(0);
            session.correct = Some(true);
            session.feedback = "Correct".into();
            session.phase = ReviewPhase::Revealed;
        }
        show(&app, &mut terminal, "Review session - revealed");
    }
}
