use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use regex::Regex;

use crate::config::{Config, ReviewOrder};
use crate::db::Database;
use crate::model::{
    ArchivedItem, CardContent, CardRow, ChoiceMode, DeckRow, GapDefinition, NoteRow, RelationRow,
    ReviewCard, ReviewScope, ReviewSectionRow, SectionRow, Statistics,
};
use crate::sync;

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[derive(PartialEq)]
enum Mode {
    Browse,
    Search,
    DeckInput,
    NoteInput,
    VaultInput,
    GitRemoteInput,
    ReviewDeckChoice,
    Review,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Dashboard,
    Reviews,
    Relations,
    Statistics,
    Clean,
    Options,
    Archived,
}

#[derive(Clone, Copy)]
enum DashboardItem {
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
            page: Page::Dashboard,
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
            status: "[1] Dashboard  [2] Reviews  [3] Relations  [4] Statistics  [5] Clean  [6] Options  [7] Archived".into(),
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
                && self.handle_key(key, terminal)?
            {
                return Ok(self.next_vault.take());
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
            self.status = format!(
                "index: {} updated, {} removed, {} failed",
                summary.indexed, summary.removed, summary.failed
            );
        }
        let total = match self.page {
            Page::Dashboard => self.dashboard_items().len(),
            Page::Reviews => self.review_items().len(),
            Page::Relations => self.sections.len(),
            Page::Statistics => 1,
            Page::Clean => self.clean_items().len(),
            Page::Options => 10,
            Page::Archived => self.archived.len(),
        };
        self.selected = self.selected.min(total.saturating_sub(1));
        Ok(())
    }

    fn consume_ygraphy_command(&mut self) -> Result<()> {
        let path = self.vault.join(".notes/ygraphy-open.json");
        if !path.exists() {
            return Ok(());
        }
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                self.status = format!("could not read ygraphy command: {error}");
                return Ok(());
            }
        };
        if let Err(error) = fs::remove_file(&path) {
            self.status = format!("could not consume ygraphy command: {error}");
            return Ok(());
        }
        let uid: String = match serde_json::from_str(&source) {
            Ok(uid) => uid,
            Err(error) => {
                self.status = format!("ignored invalid ygraphy command: {error}");
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

    fn handle_key(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<bool> {
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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(false);
        }
        match self.mode {
            Mode::Browse => self.handle_browse(key, terminal),
            Mode::Search => self.handle_search(key, terminal),
            Mode::DeckInput => self.handle_deck_input(key),
            Mode::NoteInput => self.handle_note_input(key, terminal),
            Mode::VaultInput => self.handle_vault_input(key),
            Mode::GitRemoteInput => self.handle_git_remote_input(key),
            Mode::ReviewDeckChoice => self.handle_review_deck_choice(key),
            Mode::Review => self.handle_review(key),
        }
    }

    fn handle_browse(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<bool> {
        if let KeyCode::Char(page @ '1'..='7') = key.code {
            self.page = match page {
                '1' => Page::Dashboard,
                '2' => Page::Reviews,
                '3' => Page::Relations,
                '4' => Page::Statistics,
                '5' => Page::Clean,
                '6' => Page::Options,
                _ => Page::Archived,
            };
            self.selected = 0;
            self.scroll = 0;
            self.focused_panel = usize::from(self.page == Page::Relations);
            if self.page == Page::Relations {
                self.load_relations()?;
            }
            self.set_page_status();
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('R') => {
                self.refresh_index()?;
                self.status = "reloaded Markdown and SQLite views".into();
            }
            KeyCode::Char('j') | KeyCode::Down if self.page == Page::Relations => {
                self.move_relation_selection(1)?
            }
            KeyCode::Char('k') | KeyCode::Up if self.page == Page::Relations => {
                self.move_relation_selection(-1)?
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_page(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_page(-1),
            KeyCode::Char('/') if self.page == Page::Dashboard => {
                self.mode = Mode::Search;
                self.query.clear();
                self.sections = self.db.search("")?;
                self.selected = 0;
            }
            KeyCode::Enter => self.open_selected(terminal)?,
            KeyCode::Char('g') | KeyCode::Char('b') if self.page == Page::Dashboard => {
                if let Some(section) = self.selected_dashboard_section() {
                    self.relation_section = self
                        .sections
                        .iter()
                        .position(|candidate| candidate.uid == section.uid)
                        .unwrap_or(0);
                    self.page = Page::Relations;
                    self.focused_panel = 1;
                    self.load_relations()?;
                    self.set_page_status();
                }
            }
            KeyCode::Char('r') if self.page == Page::Reviews => {
                self.review_scope_selected =
                    self.active_deck.saturating_add(1).min(self.decks.len());
                self.mode = Mode::ReviewDeckChoice;
                self.status = "Choose a deck: Enter reviews due cards, f forces all cards".into();
            }
            KeyCode::Char(' ') if self.page == Page::Reviews => self.toggle_selected_section()?,
            KeyCode::Char('n') if self.page == Page::Reviews => {
                self.mode = Mode::DeckInput;
                self.query.clear();
                self.status = "New deck name: ".into();
            }
            KeyCode::Char('n') if self.page == Page::Dashboard => {
                self.mode = Mode::NoteInput;
                self.query.clear();
                self.status = "New note title: ".into();
            }
            KeyCode::Char('[') if self.page == Page::Reviews => self.change_active_deck(-1),
            KeyCode::Char(']') if self.page == Page::Reviews => self.change_active_deck(1),
            KeyCode::Char('a') if self.page == Page::Reviews => self.toggle_selected_card_deck()?,
            KeyCode::Char('a') if self.page == Page::Clean => self.assign_clean_card()?,
            KeyCode::Char('d') if self.page == Page::Clean => self.delete_clean_image()?,
            KeyCode::Char('x')
                if matches!(self.page, Page::Dashboard | Page::Reviews | Page::Clean) =>
            {
                self.archive_selected()?
            }
            KeyCode::Char('u') if self.page == Page::Archived => self.restore_selected()?,
            KeyCode::Left | KeyCode::Char('h')
                if self.page == Page::Options && self.selected < 5 =>
            {
                self.change_option(-1)?
            }
            KeyCode::Right | KeyCode::Char('l')
                if self.page == Page::Options && self.selected < 5 =>
            {
                self.change_option(1)?
            }
            KeyCode::Char(' ')
                if self.page == Page::Options && (3..=4).contains(&self.selected) =>
            {
                self.change_option(1)?
            }
            KeyCode::Char('e') if self.page == Page::Dashboard => self.open_selected(terminal)?,
            KeyCode::Char('o') if self.page == Page::Dashboard => self.open_selected_url()?,
            KeyCode::Char('i') if self.page == Page::Dashboard => self.open_selected_image()?,
            _ => {}
        }
        Ok(false)
    }

    fn handle_review_deck_choice(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                self.set_page_status();
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
        self.status = if force {
            format!("force reviewing {count} cards from {name}")
        } else {
            format!("{count} cards due in {name}")
        };
        Ok(())
    }

    fn handle_search(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.sections = self.db.sections()?;
                self.selected = 0;
            }
            KeyCode::Enter => {
                self.edit_selected(terminal)?;
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
                self.set_page_status();
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
                        self.status = format!("created deck {}", self.query);
                    }
                    Err(error) => self.status = format!("could not create deck: {error}"),
                }
                self.mode = Mode::Browse;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.status = format!("New deck name: {}", self.query);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.status = format!("New deck name: {}", self.query);
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_note_input(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.set_page_status();
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
                self.open_editor(terminal, &path, 8)?;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.status = format!("New note title: {}", self.query);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(character);
                self.status = format!("New note title: {}", self.query);
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
                self.set_page_status();
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                let path = expand_home(self.query.trim())?;
                if self.create_vault {
                    fs::create_dir_all(&path)
                        .with_context(|| format!("creating vault {}", path.display()))?;
                }
                if !path.is_dir() {
                    self.status = format!("vault directory does not exist: {}", path.display());
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
                self.set_page_status();
            }
            KeyCode::Enter if !self.query.trim().is_empty() => {
                match sync::configure_remote(&self.vault, self.query.trim()) {
                    Ok(()) => {
                        self.sync_remote = sync::remote(&self.vault);
                        self.status =
                            "Git repository saved; select Sync now to upload the vault".into();
                        self.mode = Mode::Browse;
                    }
                    Err(error) => self.status = format!("could not save repository: {error:#}"),
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
        self.status = format!("Repository URL: {}  Enter save  Esc cancel", self.query);
    }

    fn update_vault_input_status(&mut self) {
        self.status = format!(
            "{} vault path: {}  Enter confirm  Esc cancel",
            if self.create_vault { "Create" } else { "Open" },
            self.query
        );
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
        if session.phase == ReviewPhase::Revealed {
            if let KeyCode::Char(rating @ '1'..='4') = key.code {
                let rating = rating.to_digit(10).unwrap();
                let card = session.card().unwrap().clone();
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
                    session.current += 1;
                } else {
                    session.cards.remove(session.current);
                    self.status = "card was deleted; review queue refreshed".into();
                }
                session.prepare_card();
            }
            return Ok(false);
        }
        let content = session.card().unwrap().content.clone();
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
            Page::Dashboard => {
                let total = self.dashboard_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Reviews => {
                let total = self.review_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Relations => {}
            Page::Statistics => self.scroll = self.scroll.saturating_add_signed(amount as i16),
            Page::Clean => {
                let total = self.clean_items().len();
                self.selected = self
                    .selected
                    .saturating_add_signed(amount)
                    .min(total.saturating_sub(1));
            }
            Page::Options => {
                self.selected = self.selected.saturating_add_signed(amount).min(9);
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
                Page::Dashboard => &[(0, 0), (1, 0), (1, 1), (2, 1)],
                Page::Reviews | Page::Clean | Page::Options | Page::Archived => &[(0, 0), (1, 0)],
                Page::Relations => &[(0, 0), (1, 0), (2, 0)],
                Page::Statistics => &[(0, 0), (1, 0), (2, 0), (0, 1), (3, 0), (3, 1), (3, 2)],
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

    fn dashboard_items(&self) -> Vec<DashboardItem> {
        let mut items = Vec::new();
        for (note_index, note) in self.notes.iter().enumerate() {
            items.push(DashboardItem::Note(note_index));
            items.extend(
                self.sections
                    .iter()
                    .enumerate()
                    .filter(|(_, section)| section.path == note.path)
                    .map(|(section_index, _)| DashboardItem::Section(section_index)),
            );
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

    fn selected_dashboard_section(&self) -> Option<&SectionRow> {
        match self.dashboard_items().get(self.selected)? {
            DashboardItem::Section(index) => self.sections.get(*index),
            DashboardItem::Note(index) => {
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

    fn set_page_status(&mut self) {
        self.status = match self.page {
            Page::Dashboard => "[1-7] pages  Enter open  n new note  / search  Ctrl+s sync",
            Page::Reviews => {
                "[1-7] pages  r choose review deck  Space enroll  a assign  x archive  Ctrl+s sync"
            }
            Page::Relations => "[1-7] pages  j/k select  Enter follow/open  Ctrl+s sync",
            Page::Statistics => "[1-7] pages  j/k scroll  Ctrl+s sync",
            Page::Clean => {
                "[1-7] pages  Enter open  a assign  x archive  d delete image  Ctrl+s sync"
            }
            Page::Options => "[1-7] pages  j/k select  h/l change  Enter setup/sync  Ctrl+s sync",
            Page::Archived => "[1-7] pages  j/k select  u restore  Enter open  Ctrl+s sync",
        }
        .into();
    }

    fn toggle_selected_section(&mut self) -> Result<()> {
        let Some(ReviewItem::Section(index)) = self.review_items().get(self.selected).copied()
        else {
            self.status = "select a section to add or remove it from reviews".into();
            return Ok(());
        };
        if let Some(section) = self.review_sections.get(index) {
            let active = self.db.toggle_section_review(&section.uid)?;
            self.status = format!(
                "{} review: {} / {}",
                if active { "added to" } else { "removed from" },
                section.note_title,
                section.heading
            );
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
            self.status = format!("active deck: {}", self.decks[self.active_deck].name);
        }
    }

    fn toggle_selected_card_deck(&mut self) -> Result<()> {
        let Some(ReviewItem::Card(index)) = self.review_items().get(self.selected).copied() else {
            self.status = "select a card first".into();
            return Ok(());
        };
        let Some(card) = self.cards.get(index) else {
            return Ok(());
        };
        let Some(deck) = self.decks.get(self.active_deck) else {
            self.status = "create a deck first with n".into();
            return Ok(());
        };
        let added = self.db.toggle_card_deck(card.id, deck.id)?;
        self.status = format!(
            "{} {} {}",
            if added { "added" } else { "removed" },
            card.label,
            deck.name
        );
        self.refresh_views()?;
        Ok(())
    }

    fn archive_selected(&mut self) -> Result<()> {
        let status = match self.page {
            Page::Dashboard => match self.dashboard_items().get(self.selected).copied() {
                Some(DashboardItem::Note(index)) => {
                    let note = &self.notes[index];
                    self.db.archive_note(&note.path)?;
                    format!("archived note {} and its contents", note.title)
                }
                Some(DashboardItem::Section(index)) => {
                    let section = &self.sections[index];
                    self.db.archive_section(&section.uid)?;
                    format!("archived section {}", section.heading)
                }
                None => return Ok(()),
            },
            Page::Reviews => match self.review_items().get(self.selected).copied() {
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
                    self.status = "images cannot be archived; d permanently deletes them".into();
                    return Ok(());
                }
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        self.refresh_index()?;
        self.selected = self.selected.min(
            match self.page {
                Page::Dashboard => self.dashboard_items().len(),
                Page::Reviews => self.review_items().len(),
                Page::Clean => self.clean_items().len(),
                _ => 0,
            }
            .saturating_sub(1),
        );
        self.status = status;
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
        self.status = format!("restored {label}");
        Ok(())
    }

    fn assign_clean_card(&mut self) -> Result<()> {
        let Some(CleanItem::Card(index)) = self.clean_items().get(self.selected).copied() else {
            self.status = "select an unassigned card first".into();
            return Ok(());
        };
        let Some(deck) = self.decks.get(self.active_deck) else {
            self.status = "create a deck on page 2 first".into();
            return Ok(());
        };
        let card = &self.cards[index];
        self.db.toggle_card_deck(card.id, deck.id)?;
        self.status = format!("assigned {} to {}", card.label, deck.name);
        self.refresh_views()?;
        self.selected = self
            .selected
            .min(self.clean_items().len().saturating_sub(1));
        Ok(())
    }

    fn delete_clean_image(&mut self) -> Result<()> {
        let Some(CleanItem::Image(index)) = self.clean_items().get(self.selected).copied() else {
            self.status = "select an unreferenced image first".into();
            return Ok(());
        };
        let relative = self.orphan_images[index].clone();
        fs::remove_file(self.vault.join(&relative))?;
        self.status = format!("deleted {}", relative.display());
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
        self.status = "review options saved".into();
        Ok(())
    }

    fn open_selected(&mut self, terminal: &mut Tui) -> Result<()> {
        match self.page {
            Page::Dashboard => match self.dashboard_items().get(self.selected).copied() {
                Some(DashboardItem::Note(index)) => {
                    let note = self.notes[index].clone();
                    self.open_editor(terminal, &self.vault.join(note.path), 1)?;
                }
                Some(DashboardItem::Section(index)) => {
                    let section = self.sections[index].clone();
                    self.open_editor(terminal, &self.vault.join(section.path), section.start_line)?;
                }
                None => {}
            },
            Page::Reviews => match self.review_items().get(self.selected).copied() {
                Some(ReviewItem::Section(index)) => {
                    let uid = self.review_sections[index].uid.clone();
                    if let Some(section) = self.sections.iter().find(|section| section.uid == uid) {
                        let section = section.clone();
                        self.open_editor(
                            terminal,
                            &self.vault.join(section.path),
                            section.start_line,
                        )?;
                    }
                }
                Some(ReviewItem::Deck(index)) => {
                    self.active_deck = index;
                    self.status = format!("active deck: {}", self.decks[index].name);
                }
                Some(ReviewItem::Card(index)) => {
                    let id = self.cards[index].id;
                    if let Some(card) = self.db.review_card(id)? {
                        self.review = Some(ReviewSession::new(vec![card]));
                        self.mode = Mode::Review;
                        self.status = "reviewing selected card".into();
                    } else {
                        self.refresh_index()?;
                        self.status = "card was deleted; list refreshed".into();
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
                        self.open_editor(
                            terminal,
                            &self.vault.join(section.path),
                            section.start_line,
                        )?;
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
            Page::Statistics => {}
            Page::Clean => match self.clean_items().get(self.selected).copied() {
                Some(CleanItem::Note(index)) => {
                    let note = self.notes[index].clone();
                    self.open_editor(terminal, &self.vault.join(note.path), 1)?;
                }
                Some(CleanItem::Card(index)) => {
                    let id = self.cards[index].id;
                    if let Some(card) = self.db.review_card(id)? {
                        self.review = Some(ReviewSession::new(vec![card]));
                        self.mode = Mode::Review;
                        self.status = "reviewing unassigned card".into();
                    } else {
                        self.refresh_index()?;
                        self.status = "card was deleted; list refreshed".into();
                    }
                }
                Some(CleanItem::Image(index)) => {
                    let image = self.vault.join(&self.orphan_images[index]);
                    open::that(&image)?;
                    self.status = format!("opened {}", image.display());
                }
                None => {}
            },
            Page::Options => match self.selected {
                5 => self.authenticate_github(terminal)?,
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
                            self.open_editor(terminal, &self.vault.join(path), 1)?;
                        }
                        ArchivedItem::Section {
                            path, start_line, ..
                        } => {
                            self.open_editor(terminal, &self.vault.join(path), start_line)?;
                        }
                        ArchivedItem::Quiz { .. } | ArchivedItem::Deck { .. } => {
                            self.status = "restore this item with u before opening it".into();
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn follow_relation(&mut self, uid: &str) -> Result<()> {
        let Some(index) = self.sections.iter().position(|section| section.uid == uid) else {
            self.status = format!("relation target not found: {uid}");
            return Ok(());
        };
        self.relation_section = index;
        self.incoming_selected = 0;
        self.outgoing_selected = 0;
        self.focused_panel = 1;
        self.load_relations()?;
        self.status = format!("selected {}", self.sections[index].heading);
        Ok(())
    }

    fn edit_selected(&mut self, terminal: &mut Tui) -> Result<()> {
        let Some(section) = self.sections.get(self.selected).cloned() else {
            return Ok(());
        };
        self.open_editor(terminal, &self.vault.join(section.path), section.start_line)
    }

    fn open_editor(&mut self, terminal: &mut Tui, path: &Path, line: usize) -> Result<()> {
        restore_terminal(terminal)?;
        let editor = self.config.editor.clone().unwrap_or_else(|| {
            env::var("VISUAL")
                .or_else(|_| env::var("EDITOR"))
                .unwrap_or_else(|_| "nvim".into())
        });
        let mut parts = editor.split_whitespace();
        let program = parts.next().unwrap_or("nvim");
        let status = Command::new(program)
            .args(parts)
            .arg(format!("+{line}"))
            .arg(path)
            .status();
        *terminal = setup_terminal()?;
        match status {
            Ok(_) => {
                self.last_index = Instant::now() - Duration::from_secs(2);
                self.refresh_index()?;
            }
            Err(error) => self.status = format!("editor failed: {error}"),
        }
        Ok(())
    }

    fn authenticate_github(&mut self, terminal: &mut Tui) -> Result<()> {
        restore_terminal(terminal)?;
        let login = Command::new("gh").args(["auth", "login"]).status();
        let setup = match login {
            Ok(status) if status.success() => {
                Command::new("gh").args(["auth", "setup-git"]).status()
            }
            Ok(status) => Ok(status),
            Err(error) => Err(error),
        };
        *terminal = setup_terminal()?;
        self.status = match setup {
            Ok(status) if status.success() => "GitHub authentication configured securely".into(),
            Ok(_) => "GitHub authentication was cancelled or failed".into(),
            Err(error) => format!("could not run GitHub CLI (`gh`): {error}"),
        };
        Ok(())
    }

    fn sync_vault(&mut self) -> Result<()> {
        self.status = "syncing vault with GitHub...".into();
        match sync::sync(&self.vault, None) {
            Ok(summary) => {
                self.sync_remote = Some(summary.remote);
                self.refresh_index()?;
                self.status = format!("vault synced on branch {}", summary.branch);
            }
            Err(error) => self.status = format!("sync failed: {error:#}"),
        }
        Ok(())
    }

    fn open_selected_url(&mut self) -> Result<()> {
        if let Some(section) = self.selected_dashboard_section() {
            let re = Regex::new(r"https?://[^\s)>\]]+")?;
            if let Some(url) = re.find(&section.body) {
                open::that(url.as_str())?;
                self.status = format!("opened {}", url.as_str());
            } else {
                self.status = "no URL in this section".into();
            }
        }
        Ok(())
    }

    fn open_selected_image(&mut self) -> Result<()> {
        if let Some(section) = self.selected_dashboard_section() {
            let re = Regex::new(r"!\[[^\]]*\]\(([^)]+)\)")?;
            if let Some(capture) = re.captures(&section.body) {
                let note_dir = self
                    .vault
                    .join(&section.path)
                    .parent()
                    .unwrap()
                    .to_path_buf();
                let image = note_dir.join(capture[1].trim());
                open::that(&image)?;
                self.status = format!("opened {}", image.display());
            } else {
                self.status = "no image in this section".into();
            }
        }
        Ok(())
    }

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(frame.area());
        let tabs = Line::from(vec![
            page_tab(" 1 Dashboard ", self.page == Page::Dashboard),
            Span::raw("  "),
            page_tab(" 2 Reviews ", self.page == Page::Reviews),
            Span::raw("  "),
            page_tab(" 3 Relations ", self.page == Page::Relations),
            Span::raw("  "),
            page_tab(" 4 Statistics ", self.page == Page::Statistics),
            Span::raw("  "),
            page_tab(" 5 Clean ", self.page == Page::Clean),
            Span::raw("  "),
            page_tab(" 6 Options ", self.page == Page::Options),
            Span::raw("  "),
            page_tab(" 7 Archived ", self.page == Page::Archived),
        ]);
        frame.render_widget(Paragraph::new(tabs), areas[0]);
        match self.mode {
            Mode::Review => self.draw_review(frame, areas[1]),
            Mode::ReviewDeckChoice => self.draw_review_deck_choice(frame, areas[1]),
            Mode::Search => self.draw_browse(frame, areas[1]),
            _ => match self.page {
                Page::Dashboard => self.draw_dashboard(frame, areas[1]),
                Page::Reviews => self.draw_reviews_page(frame, areas[1]),
                Page::Relations => self.draw_relations(frame, areas[1]),
                Page::Statistics => self.draw_statistics(frame, areas[1]),
                Page::Clean => self.draw_clean(frame, areas[1]),
                Page::Options => self.draw_options(frame, areas[1]),
                Page::Archived => self.draw_archived(frame, areas[1]),
            },
        }
        frame.render_widget(
            Paragraph::new(self.status.as_str()).style(Style::default().fg(Color::DarkGray)),
            areas[2],
        );
    }

    fn draw_dashboard(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(39), Constraint::Percentage(61)])
            .split(area);
        let dashboard_items = self.dashboard_items();
        let mut last_topic = None;
        let mut items = Vec::new();
        let mut selected_row = None;
        for (item_index, item) in dashboard_items.iter().enumerate() {
            let item = match item {
                DashboardItem::Note(index) => {
                    let note = &self.notes[*index];
                    let topic = note.topic.as_deref().unwrap_or("No topic");
                    if last_topic != Some(topic) {
                        items.push(ListItem::new(section_title(&topic.to_uppercase())));
                        last_topic = Some(topic);
                    }
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if note.pinned { "* " } else { "  " },
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::styled(
                            note.title.clone(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]))
                }
                DashboardItem::Section(index) => ListItem::new(Line::from(vec![
                    Span::styled("    |- ", Style::default().fg(Color::DarkGray)),
                    Span::raw(self.sections[*index].heading.clone()),
                ])),
            };
            if item_index == self.selected {
                selected_row = Some(items.len());
            }
            items.push(item);
        }
        let mut state = ListState::default().with_selected(selected_row);
        frame.render_stateful_widget(
            List::new(items)
                .block(focused_block(" Library ", self.focused_panel == 0))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[0],
            &mut state,
        );

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
            .split(columns[1]);
        let (title, detail) = match dashboard_items.get(self.selected) {
            Some(DashboardItem::Note(index)) => {
                let note = &self.notes[*index];
                let sections = self
                    .sections
                    .iter()
                    .filter(|section| section.path == note.path);
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("TOPIC   ", label_style()),
                        Span::raw(note.topic.as_deref().unwrap_or("No topic")),
                    ]),
                    Line::from(vec![
                        Span::styled("PINNED  ", label_style()),
                        Span::raw(if note.pinned { "yes" } else { "no" }),
                    ]),
                    Line::from(vec![
                        Span::styled("CREATED ", label_style()),
                        Span::raw(short_date(note.created_at)),
                    ]),
                    Line::from(vec![
                        Span::styled("EDITED  ", label_style()),
                        Span::raw(short_date(note.modified_at)),
                    ]),
                    Line::from(vec![
                        Span::styled("PATH    ", label_style()),
                        Span::raw(note.path.display().to_string()),
                    ]),
                    Line::raw(""),
                    section_title("SECTIONS"),
                ];
                lines.extend(sections.map(|section| Line::raw(format!("  {}", section.heading))));
                (format!(" {} ", note.title), Text::from(lines))
            }
            Some(DashboardItem::Section(index)) => {
                let section = &self.sections[*index];
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("NOTE  ", label_style()),
                        Span::raw(section.note_title.clone()),
                    ]),
                    Line::from(vec![
                        Span::styled("LINE  ", label_style()),
                        Span::raw(section.start_line.to_string()),
                    ]),
                    Line::raw(""),
                ];
                lines.extend(
                    display_markdown(&section.body)
                        .lines()
                        .map(|line| Line::raw(line.to_string())),
                );
                (format!(" {} ", section.heading), Text::from(lines))
            }
            None => (
                " Preview ".into(),
                Text::from("No notes indexed. Press n to create one."),
            ),
        };
        frame.render_widget(
            Paragraph::new(detail)
                .block(focused_block(title, self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            right[0],
        );

        let quick = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(right[1]);
        let pinned = self
            .notes
            .iter()
            .filter(|note| note.pinned)
            .map(|note| {
                Line::raw(format!(
                    "* {:<24} {}",
                    truncate(&note.title, 24),
                    note.topic.as_deref().unwrap_or("No topic")
                ))
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(if pinned.is_empty() {
                vec![dim_line("No pinned notes")]
            } else {
                pinned
            })
            .block(focused_block(" Pinned ", self.focused_panel == 2)),
            quick[0],
        );
        let week_ago = chrono::Utc::now().timestamp() - 7 * 86_400;
        let recent = self
            .notes
            .iter()
            .filter(|note| note.modified_at >= week_ago)
            .map(|note| {
                Line::raw(format!(
                    "{:<24} {}",
                    truncate(&note.title, 24),
                    short_date(note.modified_at)
                ))
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(if recent.is_empty() {
                vec![dim_line("No edits this week")]
            } else {
                recent
            })
            .block(focused_block(" Edited this week ", self.focused_panel == 3)),
            quick[1],
        );
    }

    fn draw_reviews_page(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        let review_items = self.review_items();
        let mut seen = [false; 3];
        let mut items = Vec::new();
        let mut selected_row = None;
        for (item_index, item) in review_items.iter().enumerate() {
            let (group, line) = match item {
                ReviewItem::Section(index) => {
                    let section = &self.review_sections[*index];
                    (
                        0,
                        Line::from(format!(
                            "  [{}] {} / {}",
                            if section.enrolled { "x" } else { " " },
                            section.note_title,
                            section.heading
                        )),
                    )
                }
                ReviewItem::Deck(index) => {
                    let deck = &self.decks[*index];
                    (
                        1,
                        Line::styled(
                            format!(
                                "  {} {}  {} cards",
                                if *index == self.active_deck { "*" } else { " " },
                                deck.name,
                                deck.card_count
                            ),
                            if *index == self.active_deck {
                                Style::default().fg(Color::Yellow)
                            } else {
                                Style::default()
                            },
                        ),
                    )
                }
                ReviewItem::Card(index) => {
                    (2, Line::from(format!("  {}", self.cards[*index].label)))
                }
            };
            if !seen[group] {
                items.push(ListItem::new(section_title(
                    ["SECTIONS", "DECKS", "CARDS"][group],
                )));
                seen[group] = true;
            }
            if item_index == self.selected {
                selected_row = Some(items.len());
            }
            items.push(ListItem::new(line));
        }
        let mut state = ListState::default().with_selected(selected_row);
        frame.render_stateful_widget(
            List::new(items)
                .block(focused_block(" Review organizer ", self.focused_panel == 0))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[0],
            &mut state,
        );

        let active = self
            .decks
            .get(self.active_deck)
            .map_or("No deck", |deck| deck.name.as_str());
        let (title, mut lines) = match review_items.get(self.selected) {
            Some(ReviewItem::Section(index)) => {
                let section = &self.review_sections[*index];
                let body = self
                    .sections
                    .iter()
                    .find(|candidate| candidate.uid == section.uid)
                    .map(|candidate| display_markdown(&candidate.body))
                    .unwrap_or_default();
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("NOTE    ", label_style()),
                        Span::raw(section.note_title.clone()),
                    ]),
                    Line::from(vec![
                        Span::styled("REVIEW  ", label_style()),
                        Span::raw(if section.enrolled {
                            "enrolled"
                        } else {
                            "not enrolled"
                        }),
                    ]),
                    Line::raw(""),
                ];
                lines.extend(body.lines().map(|line| Line::raw(line.to_string())));
                (format!(" Section: {} ", section.heading), lines)
            }
            Some(ReviewItem::Deck(index)) => {
                let deck = &self.decks[*index];
                let cards = self
                    .cards
                    .iter()
                    .filter(|card| card.decks.contains(&deck.id));
                let mut lines = vec![
                    Line::from(format!("{} cards", deck.card_count)),
                    Line::raw(""),
                ];
                lines.extend(cards.map(|card| Line::raw(format!("  {}", card.label))));
                (format!(" Deck: {} ", deck.name), lines)
            }
            Some(ReviewItem::Card(index)) => {
                let card = &self.cards[*index];
                let assigned = self
                    .decks
                    .iter()
                    .filter(|deck| card.decks.contains(&deck.id))
                    .map(|deck| deck.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    " Card ".into(),
                    vec![
                        Line::raw(card.label.clone()),
                        Line::raw(""),
                        Line::from(vec![
                            Span::styled("DECKS   ", label_style()),
                            Span::raw(if assigned.is_empty() {
                                "Unassigned".to_string()
                            } else {
                                assigned
                            }),
                        ]),
                        Line::from(vec![
                            Span::styled("ACTIVE  ", label_style()),
                            Span::raw(active),
                        ]),
                        Line::raw(""),
                        Line::styled(
                            "Enter reviews this card now.",
                            Style::default().fg(Color::Green),
                        ),
                        Line::raw("Press a to add/remove it from the active deck."),
                    ],
                )
            }
            None => (
                " Review setup ".into(),
                vec![Line::raw("No review items yet.")],
            ),
        };
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Enter open/review   Space enroll   n new deck   [/] active deck   a assign",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .block(focused_block(title, self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }

    fn draw_review_deck_choice(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let deckless = self
            .cards
            .iter()
            .filter(|card| card.decks.is_empty())
            .count();
        let mut items = vec![ListItem::new(format!("No deck  {deckless} cards"))];
        items.extend(
            self.decks
                .iter()
                .map(|deck| ListItem::new(format!("{}  {} cards", deck.name, deck.card_count))),
        );
        let mut state = ListState::default().with_selected(Some(self.review_scope_selected));
        let height = (items.len() as u16).saturating_add(6).min(area.height);
        frame.render_stateful_widget(
            List::new(items)
                .block(review_block(" Choose a deck "))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            centered_card(area, 64, height),
            &mut state,
        );
    }

    fn draw_clean(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        let clean_items = self.clean_items();
        let mut seen = [false; 3];
        let mut items = Vec::new();
        let mut selected_row = None;
        for (item_index, item) in clean_items.iter().enumerate() {
            let (group, line) = match item {
                CleanItem::Note(index) => {
                    (0, Line::raw(format!("  ! {}", self.notes[*index].title)))
                }
                CleanItem::Card(index) => {
                    (1, Line::raw(format!("  ! {}", self.cards[*index].label)))
                }
                CleanItem::Image(index) => (
                    2,
                    Line::raw(format!("  ! {}", self.orphan_images[*index].display())),
                ),
            };
            if !seen[group] {
                items.push(ListItem::new(section_title(
                    [
                        "NOTES WITHOUT TOPICS",
                        "CARDS WITHOUT DECKS",
                        "UNUSED IMAGES",
                    ][group],
                )));
                seen[group] = true;
            }
            if item_index == self.selected {
                selected_row = Some(items.len());
            }
            items.push(ListItem::new(line));
        }
        let mut state = ListState::default().with_selected(selected_row);
        frame.render_stateful_widget(
            List::new(items)
                .block(focused_block(" Cleanup queue ", self.focused_panel == 0))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[0],
            &mut state,
        );

        let active_deck = self
            .decks
            .get(self.active_deck)
            .map_or("No deck", |deck| deck.name.as_str());
        let (title, lines) = match clean_items.get(self.selected) {
            Some(CleanItem::Note(index)) => (
                format!(" Note: {} ", self.notes[*index].title),
                vec![
                    Line::styled("Missing topic", Style::default().fg(Color::Yellow)),
                    Line::raw(""),
                    Line::raw("Open this note and add a topic to its front matter:"),
                    Line::styled("topic: Your topic", Style::default().fg(Color::Cyan)),
                    Line::raw(""),
                    dim_line("Enter open in editor"),
                ],
            ),
            Some(CleanItem::Card(index)) => (
                " Unassigned card ".into(),
                vec![
                    Line::raw(self.cards[*index].label.clone()),
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("ACTIVE DECK  ", label_style()),
                        Span::raw(active_deck),
                    ]),
                    Line::raw(""),
                    Line::styled(
                        "Press a to assign this card.",
                        Style::default().fg(Color::Green),
                    ),
                    dim_line("Choose the active deck on page 2 with [ and ]."),
                ],
            ),
            Some(CleanItem::Image(index)) => {
                let path = &self.orphan_images[*index];
                let size = fs::metadata(self.vault.join(path))
                    .map(|metadata| format!("{} bytes", metadata.len()))
                    .unwrap_or_else(|_| "unknown".into());
                (
                    " Unreferenced image ".into(),
                    vec![
                        Line::raw(path.display().to_string()),
                        Line::raw(""),
                        Line::from(vec![Span::styled("SIZE  ", label_style()), Span::raw(size)]),
                        Line::raw(""),
                        Line::raw("No Markdown image link in the vault points to this file."),
                        Line::styled(
                            "Enter opens it. d permanently deletes it.",
                            Style::default().fg(Color::Yellow),
                        ),
                    ],
                )
            }
            None => (
                " Vault clean ".into(),
                vec![
                    Line::styled(
                        "Everything has a home.",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(""),
                    Line::raw("All notes have topics, all cards belong to decks,"),
                    Line::raw("and every image is referenced by Markdown."),
                ],
            ),
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(focused_block(title, self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }

    fn draw_archived(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        let items = self
            .archived
            .iter()
            .map(|item| {
                let (kind, label) = match item {
                    ArchivedItem::Note { title, .. } => ("NOTE", title.as_str()),
                    ArchivedItem::Section { heading, .. } => ("SECTION", heading.as_str()),
                    ArchivedItem::Quiz { label, .. } => ("QUIZ", label.as_str()),
                    ArchivedItem::Deck { name, .. } => ("DECK", name.as_str()),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {kind:<8}"), Style::default().fg(Color::DarkGray)),
                    Span::raw(label.to_string()),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default()
            .with_selected((!self.archived.is_empty()).then_some(self.selected));
        frame.render_stateful_widget(
            List::new(if items.is_empty() {
                vec![ListItem::new(dim_line("Nothing is archived"))]
            } else {
                items
            })
            .block(focused_block(" Archived items ", self.focused_panel == 0))
            .highlight_style(selected_style())
            .highlight_symbol("> "),
            columns[0],
            &mut state,
        );

        let (title, lines) = self.archived.get(self.selected).map_or_else(
            || {
                (
                    " Archive ".to_string(),
                    vec![
                        Line::styled("The archive is empty.", Style::default().fg(Color::Green)),
                        Line::raw(""),
                        Line::raw("Press x on Dashboard, Reviews, or Clean to archive an item."),
                    ],
                )
            },
            |item| match item {
                ArchivedItem::Note {
                    title,
                    path,
                    section_count,
                    quiz_count,
                    ..
                } => (
                    format!(" Note: {title} "),
                    vec![
                        Line::raw(path.display().to_string()),
                        Line::raw(""),
                        Line::raw(format!("{section_count} sections  {quiz_count} quizzes")),
                        Line::raw(""),
                        Line::raw("Its sections and quizzes are hidden with the note."),
                        dim_line("u restore note and contents   Enter open source"),
                    ],
                ),
                ArchivedItem::Section {
                    note_title,
                    heading,
                    path,
                    quiz_count,
                    ..
                } => (
                    format!(" Section: {heading} "),
                    vec![
                        Line::raw(format!("{note_title}  /  {}", path.display())),
                        Line::raw(""),
                        Line::raw(format!("{quiz_count} quizzes hidden with this section")),
                        Line::raw(""),
                        dim_line("u restore section and quizzes   Enter open source"),
                    ],
                ),
                ArchivedItem::Quiz {
                    label, card_count, ..
                } => (
                    " Archived quiz ".into(),
                    vec![
                        Line::raw(label.clone()),
                        Line::raw(""),
                        Line::raw(format!("{card_count} card variants retained")),
                        Line::raw(""),
                        dim_line("u restore quiz"),
                    ],
                ),
                ArchivedItem::Deck {
                    name, quiz_count, ..
                } => (
                    format!(" Deck: {name} "),
                    vec![
                        Line::raw(format!("{quiz_count} quizzes assigned")),
                        Line::raw(""),
                        Line::raw("Quizzes exclusive to this deck are hidden from active views."),
                        Line::raw("Quizzes shared with an active deck remain active."),
                        Line::raw(""),
                        dim_line("u restore deck and its exclusive quizzes"),
                    ],
                ),
            },
        );
        frame.render_widget(
            Paragraph::new(lines)
                .block(focused_block(title, self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }

    fn draw_options(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(area);
        let order = match self.config.review_order {
            ReviewOrder::Due => "Due first",
            ReviewOrder::Random => "Random",
        };
        let rows = [
            (
                "Desired retention",
                format!("{:.0}%", self.config.desired_retention * 100.0),
            ),
            ("New cards / day", self.config.new_cards_per_day.to_string()),
            (
                "Maximum reviews / day",
                self.config.max_reviews_per_day.to_string(),
            ),
            ("Review order", order.into()),
            (
                "Bury sibling cards",
                if self.config.bury_siblings {
                    "On"
                } else {
                    "Off"
                }
                .into(),
            ),
            ("GitHub authentication", "Enter".into()),
            (
                "Repository URL",
                self.sync_remote
                    .as_deref()
                    .map(|remote| truncate(remote, 30))
                    .unwrap_or_else(|| "Not configured".into()),
            ),
            (
                "Sync now",
                if self.sync_remote.is_some() {
                    "Enter"
                } else {
                    "Set repository first"
                }
                .into(),
            ),
            ("Open another vault", "Enter".into()),
            ("Create new vault", "Enter".into()),
        ];
        let items = rows
            .iter()
            .map(|(label, value)| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{label:<28}")),
                    Span::styled(
                        value.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(focused_block(" Review options ", self.focused_panel == 0))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[0],
            &mut state,
        );
        let descriptions = [
            (
                "Desired retention",
                "FSRS targets this recall probability when choosing the next interval. Higher values produce shorter intervals and more reviews.",
            ),
            (
                "New cards per day",
                "Limits cards that have never been reviewed. Cards introduced earlier today count toward this limit.",
            ),
            (
                "Maximum reviews per day",
                "Caps the complete daily workload. Reviews already completed today count toward this limit.",
            ),
            (
                "Review order",
                "Due first clears the oldest scheduled cards first. Random shuffles the available queue.",
            ),
            (
                "Bury siblings",
                "Shows at most one due card from each section per session, reducing answer leakage between related cards.",
            ),
            (
                "GitHub authentication",
                "Securely signs in through GitHub CLI using a browser or device code, then configures Git's credential helper. SSH users can skip this step. Yalive never sees or stores a token.",
            ),
            (
                "Repository URL",
                "The empty or existing GitHub repository for this vault. Use https://github.com/owner/repo.git after authentication, or git@github.com:owner/repo.git for SSH. URLs containing tokens are rejected.",
            ),
            (
                "Sync now",
                "Commits local vault changes, fetches and integrates remote changes, then pushes. Conflicts stop safely without overwriting either device. The SQLite index is never uploaded.",
            ),
            (
                "Open another vault",
                "Closes the current vault and opens an existing vault directory. The selected vault becomes the default next time yalive starts.",
            ),
            (
                "Create new vault",
                "Creates the directory when needed, initializes its .notes index, and remembers it as the default vault.",
            ),
        ];
        let (title, description) = descriptions[self.selected.min(descriptions.len() - 1)];
        let control_hint = if self.selected >= 5 {
            "Enter to continue"
        } else {
            "h/l or Left/Right to change"
        };
        let details = vec![
            section_title(title),
            Line::raw(""),
            Line::raw(description),
            Line::raw(""),
            Line::styled(control_hint, Style::default().fg(Color::Yellow)),
            Line::raw(""),
            section_title("STORAGE"),
            Line::raw(".notes/config.toml"),
            Line::raw("Git credentials: system credential helper"),
            Line::raw(""),
            Line::from(vec![
                Span::styled("EDITOR    ", label_style()),
                Span::raw(
                    self.config
                        .editor
                        .as_deref()
                        .unwrap_or("$VISUAL / $EDITOR / nvim"),
                ),
            ]),
            Line::from(vec![
                Span::styled("REINDEX   ", label_style()),
                Span::raw(format!("{} ms", self.config.reindex_interval_ms)),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(details)
                .block(focused_block(" Option guide ", self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }

    fn draw_statistics(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let stats = &self.statistics;
        let accuracy = |value: Option<f64>| {
            value.map_or("--".into(), |value| format!("{:.0}%", value * 100.0))
        };
        let response = stats.average_response_ms.map_or("--".into(), |value| {
            format!("{:.1}s", value as f64 / 1000.0)
        });
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(10)])
            .split(columns[0]);
        let pulse = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ])
            .split(left[0]);
        metric_box(
            frame,
            pulse[0],
            " DUE NOW ",
            stats.due_now.to_string(),
            "clear today's queue",
            Color::Yellow,
            self.focused_panel == 0,
        );
        metric_box(
            frame,
            pulse[1],
            " ACCURACY ",
            accuracy(stats.accuracy_week),
            "last 7 days",
            Color::Green,
            self.focused_panel == 1,
        );
        metric_box(
            frame,
            pulse[2],
            " STREAK ",
            format!("{} days", stats.streak_days),
            &format!("{} today", stats.reviewed_today),
            Color::Cyan,
            self.focused_panel == 2,
        );
        let mut activity = vec![
            Line::from(format!(
                "7-day reviews {}   avg response {}",
                stats.reviewed_week, response
            )),
            Line::raw(""),
        ];
        let activity_max = stats
            .daily_reviews
            .iter()
            .map(|(_, count)| *count)
            .max()
            .unwrap_or(1)
            .max(1);
        for (day, count) in &stats.daily_reviews {
            activity.push(Line::from(format!(
                " {}  {:<24} {}",
                day_label(*day),
                bar(*count, activity_max, 24),
                count
            )));
        }
        frame.render_widget(
            Paragraph::new(activity)
                .block(focused_block(" 14-day activity ", self.focused_panel == 3))
                .scroll((self.scroll, 0)),
            left[1],
        );

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(42),
                Constraint::Percentage(27),
                Constraint::Percentage(31),
            ])
            .split(columns[1]);
        let mut forecast = Vec::new();
        let forecast_max = stats
            .due_forecast
            .iter()
            .map(|(_, count)| *count)
            .max()
            .unwrap_or(1)
            .max(1);
        for (day, count) in &stats.due_forecast {
            forecast.push(Line::from(format!(
                " {}  {:<24} {} due",
                day_label(*day),
                bar(*count, forecast_max, 24),
                count
            )));
        }
        frame.render_widget(
            Paragraph::new(forecast).block(focused_block(
                " Workload: next 7 days ",
                self.focused_panel == 4,
            )),
            right[0],
        );

        let labels = ["Again", "Hard ", "Good ", "Easy "];
        let ratings_max = stats
            .rating_counts
            .iter()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
        let mut ratings = vec![Line::from(format!(
            "30-day accuracy {}",
            accuracy(stats.accuracy_month)
        ))];
        for (label, count) in labels.iter().zip(stats.rating_counts) {
            ratings.push(Line::from(format!(
                " {label}  {:<18} {count}",
                bar(count, ratings_max, 24)
            )));
        }
        frame.render_widget(
            Paragraph::new(ratings).block(focused_block(
                " Retention signals ",
                self.focused_panel == 5,
            )),
            right[1],
        );

        let mut health = vec![section_title("NEEDS ATTENTION")];
        if stats.weak_notes.is_empty() {
            health.push(dim_line("Not enough review history yet"));
        }
        for (title, reviews, score) in &stats.weak_notes {
            health.push(Line::from(format!(
                " {:<28} {:>3} reviews  {:>3.0}%",
                truncate(title, 28),
                reviews,
                score * 100.0
            )));
        }
        health.extend([
            Line::raw(""),
            section_title("LIBRARY"),
            Line::raw(format!(
                "{} notes  {} topics  {} active cards",
                stats.note_count, stats.topic_count, stats.card_count
            )),
            Line::styled(
                format!("{} notes still need a topic", stats.untopiced_count),
                if stats.untopiced_count > 0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]);
        frame.render_widget(
            Paragraph::new(health).block(focused_block(" Focus ", self.focused_panel == 6)),
            right[2],
        );
    }

    fn draw_browse(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
            .split(area);
        let items: Vec<_> = self
            .sections
            .iter()
            .map(|section| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{}  ", section.note_title),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(&section.heading),
                ]))
            })
            .collect();
        let title = if self.mode == Mode::Search {
            format!(" Search: {} ", self.query)
        } else {
            " Sections ".into()
        };
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(focused_block(title, self.focused_panel == 0))
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> "),
            columns[0],
            &mut state,
        );
        let (title, body) = self.sections.get(self.selected).map_or_else(
            || (" Section ".to_string(), "No sections indexed".to_string()),
            |section| {
                (
                    format!(" {} / {} ", section.note_title, section.heading),
                    display_markdown(&section.body),
                )
            },
        );
        frame.render_widget(
            Paragraph::new(body)
                .block(focused_block(title, self.focused_panel == 1))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
    }

    fn draw_relations(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(40),
                Constraint::Percentage(30),
            ])
            .split(area);
        let incoming = self.incoming_relations();
        let outgoing = self.outgoing_relations();
        let relation_items = |relations: &[&RelationRow], empty: &str| {
            if relations.is_empty() {
                vec![ListItem::new(dim_line(empty))]
            } else {
                relations
                    .iter()
                    .map(|relation| {
                        ListItem::new(vec![
                            Line::styled(
                                relation.relation_type.to_uppercase(),
                                Style::default().fg(Color::Cyan),
                            ),
                            Line::raw(
                                relation
                                    .target_heading
                                    .as_deref()
                                    .unwrap_or(&relation.target_uid)
                                    .to_string(),
                            ),
                            dim_line(&relation.target_uid),
                        ])
                    })
                    .collect()
            }
        };
        let mut incoming_state = ListState::default()
            .with_selected((!incoming.is_empty()).then_some(self.incoming_selected));
        frame.render_stateful_widget(
            List::new(relation_items(&incoming, "No incoming sections"))
                .block(focused_block(" Incoming ", self.focused_panel == 0))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[0],
            &mut incoming_state,
        );

        let middle = self
            .sections
            .iter()
            .map(|section| {
                ListItem::new(vec![
                    Line::styled(
                        section.heading.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        format!("{}  {}", section.note_title, section.uid),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        let mut middle_state = ListState::default()
            .with_selected((!self.sections.is_empty()).then_some(self.relation_section));
        frame.render_stateful_widget(
            List::new(middle)
                .block(focused_block(" Selected section ", self.focused_panel == 1))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[1],
            &mut middle_state,
        );

        let mut outgoing_state = ListState::default()
            .with_selected((!outgoing.is_empty()).then_some(self.outgoing_selected));
        frame.render_stateful_widget(
            List::new(relation_items(&outgoing, "No outgoing sections"))
                .block(focused_block(" Outgoing ", self.focused_panel == 2))
                .highlight_style(selected_style())
                .highlight_symbol("> "),
            columns[2],
            &mut outgoing_state,
        );
    }

    fn draw_review(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(session) = &self.review else {
            return;
        };
        let Some(card) = session.card() else {
            frame.render_widget(
                Paragraph::new("Review complete. Press Enter to return.")
                    .alignment(Alignment::Center)
                    .block(review_block(" Review complete ")),
                centered_card(area, 64, 9),
            );
            return;
        };
        let title = format!(
            " Review {} of {} ",
            session.current + 1,
            session.cards.len(),
        );
        let metadata = Line::from(vec![
            Span::styled(card.section_uid.clone(), Style::default().fg(Color::Cyan)),
            Span::styled("  /  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if card.review_count == 0 {
                    "new".to_string()
                } else {
                    format!("seen {}", card.review_count)
                },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("  /  due ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                chrono::DateTime::from_timestamp(card.due_at, 0)
                    .map(|date| date.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "now".into()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let mut content = review_text(session, card);
        content.lines.insert(0, Line::raw(""));
        content.lines.insert(0, metadata);
        let height = (content.lines.len() as u16).saturating_add(4).max(12);
        frame.render_widget(
            Paragraph::new(content)
                .block(review_block(title))
                .wrap(Wrap { trim: false }),
            centered_card(area, 100, height),
        );
    }
}

fn archived_item_label(item: &ArchivedItem) -> String {
    match item {
        ArchivedItem::Note { title, .. } => format!("note {title}"),
        ArchivedItem::Section { heading, .. } => format!("section {heading}"),
        ArchivedItem::Quiz { label, .. } => format!("quiz {label}"),
        ArchivedItem::Deck { name, .. } => format!("deck {name}"),
    }
}

fn review_text(session: &ReviewSession, card: &ReviewCard) -> Text<'static> {
    let mut lines = Vec::new();
    match &card.content {
        CardContent::Section { title, body } => {
            lines.push(Line::styled(
                title.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::raw(""));
            if session.phase == ReviewPhase::Revealed {
                lines.extend(
                    display_markdown(body)
                        .lines()
                        .map(|line| Line::raw(line.to_string())),
                );
            } else {
                lines.push(Line::styled(
                    "Recall the section, then reveal it.",
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "[Space] Reveal",
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        CardContent::Cloze { prompt, cloze } => {
            lines.extend(render_cloze(
                prompt,
                *cloze,
                session.phase == ReviewPhase::Revealed,
            ));
            lines.push(Line::raw(""));
            if session.phase == ReviewPhase::Answering {
                lines.push(Line::styled(
                    "[Space] Reveal",
                    Style::default().fg(Color::Yellow),
                ));
            }
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
                let mark = if session.selected.contains(&index) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let cursor = if position == session.choice_cursor {
                    ">"
                } else {
                    " "
                };
                let style = if session.phase == ReviewPhase::Revealed && answer.correct {
                    Style::default().fg(Color::Green)
                } else if session.phase == ReviewPhase::Revealed
                    && session.selected.contains(&index)
                {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(
                    format!("{cursor} {mark} {}", answer.text),
                    style,
                ));
            }
            if session.phase == ReviewPhase::Revealed
                && let Some(explanation) = explanation
            {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Explanation",
                    Style::default().fg(Color::Cyan),
                ));
                lines.extend(explanation.lines().map(|line| Line::raw(line.to_string())));
            }
        }
        CardContent::CodeGap {
            language,
            prompt,
            code,
            gaps,
        } => {
            if let Some(prompt) = prompt {
                lines.extend(prompt.lines().map(|line| Line::raw(line.to_string())));
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(
                format!(" {} ", language.to_uppercase()),
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ));
            lines.extend(render_code(code, session, gaps));
            if session.phase == ReviewPhase::Answering {
                let current = session
                    .gap_names
                    .get(session.gap_cursor)
                    .map_or("", String::as_str);
                let value = session.gap_values.get(current).map_or("", String::as_str);
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {current} "),
                        Style::default().fg(Color::Black).bg(Color::Yellow),
                    ),
                    Span::raw(format!(
                        "  {}",
                        if value.is_empty() {
                            "type your answer"
                        } else {
                            value
                        }
                    )),
                ]));
                lines.push(dim_line("Tab / Shift+Tab change gap   Enter check answer"));
            }
        }
    }
    if session.phase == ReviewPhase::Revealed {
        if !session.feedback.is_empty() {
            let color = if session.correct == Some(false) {
                Color::Red
            } else {
                Color::Green
            };
            lines.push(Line::styled(
                session.feedback.clone(),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "[1] Again   [2] Hard   [3] Good   [4] Easy",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Text::from(lines)
}

fn page_tab(label: &'static str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            label,
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(label, Style::default().fg(Color::DarkGray))
    }
}

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

fn focused_block(title: impl Into<String>, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .title(title.into())
        .title_style(style)
        .borders(Borders::ALL)
        .border_style(style)
}

fn selected_style() -> Style {
    Style::default()
        .bg(Color::Rgb(28, 40, 48))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn label_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

fn metric_box(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    value: String,
    caption: &str,
    color: Color,
    focused: bool,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                value,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Line::styled(caption.to_string(), Style::default().fg(Color::DarkGray)),
        ])
        .block(focused_block(title, focused)),
        area,
    );
}

fn section_title(title: &str) -> Line<'static> {
    Line::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn dim_line(value: &str) -> Line<'static> {
    Line::styled(value.to_string(), Style::default().fg(Color::DarkGray))
}

fn short_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn day_label(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.format("%a %d").to_string())
        .unwrap_or_else(|| "-------".into())
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    value
        .chars()
        .take(width.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }
    slug.trim_end_matches('-').to_string()
}

fn bar(value: usize, maximum: usize, width: usize) -> String {
    let filled = if value == 0 {
        0
    } else {
        (value * width).div_ceil(maximum)
    };
    format!(
        "{}{}",
        "#".repeat(filled),
        ".".repeat(width.saturating_sub(filled))
    )
}

fn render_cloze(prompt: &str, target: u32, revealed: bool) -> Vec<Line<'static>> {
    let marker = Regex::new(r"\{\{c(\d+)::([^}:]+)(?:::([^}]+))?\}\}").unwrap();
    let rendered = marker.replace_all(prompt, |capture: &regex::Captures<'_>| {
        let number: u32 = capture[1].parse().unwrap_or_default();
        if number == target && !revealed {
            capture
                .get(3)
                .map_or("[...]", |hint| hint.as_str())
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
    let marker = Regex::new(r"\{\{gap:([a-zA-Z0-9_-]+)\}\}").unwrap();
    code.lines()
        .enumerate()
        .map(|(index, line)| {
            let mut spans = vec![Span::styled(
                format!("{:>3} | ", index + 1),
                Style::default().fg(Color::DarkGray),
            )];
            let mut end = 0;
            for capture in marker.captures_iter(line) {
                let whole = capture.get(0).unwrap();
                spans.push(Span::raw(line[end..whole.start()].to_string()));
                let name = &capture[1];
                let (value, style) = if session.phase == ReviewPhase::Revealed {
                    let submitted = session.gap_values.get(name).map_or("", String::as_str);
                    let value = if matches_gap(submitted, &gaps[name]) {
                        submitted.to_string()
                    } else {
                        canonical_answer(&gaps[name])
                    };
                    (
                        value,
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
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
                    let style = if active {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    (value, style)
                };
                spans.push(Span::styled(value, style));
                end = whole.end();
            }
            spans.push(Span::raw(line[end..].to_string()));
            Line::from(spans)
        })
        .collect()
}

fn canonical_answer(gap: &GapDefinition) -> String {
    gap.answer
        .clone()
        .or_else(|| {
            gap.answers
                .as_ref()
                .and_then(|answers| answers.first().cloned())
        })
        .unwrap_or_else(|| "<regex answer>".into())
}

fn matches_gap(submitted: &str, gap: &GapDefinition) -> bool {
    if let Some(regex) = &gap.regex
        && Regex::new(regex).is_ok_and(|regex| regex.is_match(submitted))
    {
        return true;
    }
    let normalize = |value: &str| {
        let mut value = if gap.r#match.trim {
            value.trim().to_string()
        } else {
            value.to_string()
        };
        if gap.r#match.normalize_whitespace {
            value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        if !gap.r#match.case_sensitive {
            value = value.to_lowercase();
        }
        value
    };
    let submitted = normalize(submitted);
    gap.answer
        .iter()
        .chain(gap.answers.iter().flatten())
        .any(|answer| normalize(answer) == submitted)
}

fn display_markdown(body: &str) -> String {
    let quiz = Regex::new(r"(?s)```quiz\s.*?```").unwrap();
    let images = Regex::new(r"!\[([^\]]*)\]\([^)]+\)").unwrap();
    let without_quizzes = quiz.replace_all(body, "[Quiz card]");
    images
        .replace_all(&without_quizzes, |capture: &regex::Captures<'_>| {
            format!("[Image: {}] (press i to open)", &capture[1])
        })
        .to_string()
}

fn find_orphan_images(vault: &Path) -> Result<Vec<PathBuf>> {
    let image_extensions = ["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"];
    let mut images = Vec::new();
    let mut markdown = Vec::new();
    for entry in walkdir::WalkDir::new(vault)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".notes")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if extension == "md" {
            markdown.push(path.to_path_buf());
        } else if image_extensions.contains(&extension.as_str()) {
            images.push(path.canonicalize()?);
        }
    }

    let markdown_image = Regex::new(r#"!\[[^\]]*\]\((?:<([^>]+)>|([^\)]+))\)"#)?;
    let wiki_image = Regex::new(r"!\[\[([^\]|#]+)")?;
    let mut referenced = HashSet::new();
    let mut wiki_names = HashSet::new();
    for path in markdown {
        let source = fs::read_to_string(&path)?;
        let directory = path.parent().unwrap_or(vault);
        for capture in markdown_image.captures_iter(&source) {
            let raw_target = capture
                .get(1)
                .or_else(|| capture.get(2))
                .map(|value| value.as_str().trim())
                .unwrap_or("");
            let raw_target = raw_target
                .split_once(" \"")
                .map_or(raw_target, |(path, _)| path);
            let target = target_without_fragment(raw_target);
            if target.starts_with("http://") || target.starts_with("https://") {
                continue;
            }
            let target = Path::new(target);
            let resolved = if target.is_absolute() {
                target.to_path_buf()
            } else {
                directory.join(target)
            };
            if let Ok(resolved) = resolved.canonicalize() {
                referenced.insert(resolved);
            }
        }
        for capture in wiki_image.captures_iter(&source) {
            let target = target_without_fragment(capture[1].trim());
            let resolved = directory.join(target);
            if let Ok(resolved) = resolved.canonicalize() {
                referenced.insert(resolved);
            } else if let Some(name) = Path::new(target).file_name() {
                wiki_names.insert(name.to_os_string());
            }
        }
    }
    for image in &images {
        if image
            .file_name()
            .is_some_and(|name| wiki_names.contains(name))
        {
            referenced.insert(image.clone());
        }
    }
    let mut orphaned = images
        .into_iter()
        .filter(|path| !referenced.contains(path))
        .filter_map(|path| path.strip_prefix(vault).ok().map(Path::to_path_buf))
        .collect::<Vec<_>>();
    orphaned.sort();
    Ok(orphaned)
}

fn target_without_fragment(target: &str) -> &str {
    target.split(['#', '?']).next().unwrap_or(target)
}

fn expand_home(value: &str) -> Result<PathBuf> {
    let path = if value == "~" || value.starts_with("~/") {
        let home = directories::UserDirs::new()
            .map(|directories| directories.home_dir().to_path_buf())
            .context("could not determine home directory")?;
        if value == "~" {
            home
        } else {
            home.join(&value[2..])
        }
    } else {
        PathBuf::from(value)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn review_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .title(title.into())
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Blue))
        .padding(Padding::new(2, 2, 1, 1))
}

fn centered_card(area: Rect, desired_width: u16, desired_height: u16) -> Rect {
    let width = desired_width.min(area.width.saturating_sub(2)).max(1);
    let height = desired_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
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
    use crate::model::MatchOptions;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    #[test]
    fn matches_normalized_and_regex_gaps() {
        let normalized = GapDefinition {
            answer: Some("a + b".into()),
            answers: None,
            regex: None,
            r#match: MatchOptions {
                trim: true,
                normalize_whitespace: true,
                case_sensitive: true,
            },
        };
        assert!(matches_gap(" a  +  b ", &normalized));
        let regex = GapDefinition {
            answer: None,
            answers: None,
            regex: Some(r"^values(?:\.iter\(\))?$".into()),
            r#match: MatchOptions::default(),
        };
        assert!(matches_gap("values.iter()", &regex));
    }

    #[test]
    fn renders_every_page_at_standard_terminal_size() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("welcome.md"),
            "---\nid: welcome\ntitle: Welcome\ntopic: General\npinned: true\n---\n# Welcome {#root}\n\nStart here.\n",
        )
        .unwrap();
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault, database).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        for page in [
            Page::Dashboard,
            Page::Reviews,
            Page::Relations,
            Page::Statistics,
            Page::Clean,
            Page::Options,
            Page::Archived,
        ] {
            app.page = page;
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        app.mode = Mode::ReviewDeckChoice;
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn follows_incoming_and_outgoing_relations() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("alpha.md"),
            "---\nid: alpha\ntitle: Alpha\n---\n# Alpha {#root}\n\n[[beta#root]]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.md"),
            "---\nid: beta\ntitle: Beta\n---\n# Beta {#root}\n",
        )
        .unwrap();
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault, database).unwrap();
        app.page = Page::Relations;
        app.relation_section = app
            .sections
            .iter()
            .position(|section| section.uid == "alpha#root")
            .unwrap();
        app.load_relations().unwrap();
        assert_eq!(app.outgoing_relations().len(), 1);

        app.follow_relation("beta#root").unwrap();
        assert_eq!(app.sections[app.relation_section].uid, "beta#root");
        assert_eq!(app.incoming_relations().len(), 1);
    }

    #[test]
    fn consumes_ygraphy_focus_commands() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("welcome.md"),
            "---\nid: welcome\ntitle: Welcome\n---\n# Welcome {#root}\n",
        )
        .unwrap();
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault.clone(), database).unwrap();
        fs::write(
            vault.join(".notes/ygraphy-open.json"),
            serde_json::to_vec("welcome#root").unwrap(),
        )
        .unwrap();

        app.consume_ygraphy_command().unwrap();

        assert!(app.page == Page::Relations);
        assert_eq!(app.sections[app.relation_section].uid, "welcome#root");
        assert!(!vault.join(".notes/ygraphy-open.json").exists());
    }

    #[test]
    fn moves_panel_focus_spatially() {
        let dir = tempdir().unwrap();
        let vault = dir.path().canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault, database).unwrap();

        app.move_panel_focus(1, 0);
        assert_eq!(app.focused_panel, 1);
        app.move_panel_focus(0, 1);
        assert_eq!(app.focused_panel, 2);
        app.move_panel_focus(1, 0);
        assert_eq!(app.focused_panel, 3);
        app.move_panel_focus(0, -1);
        assert_eq!(app.focused_panel, 1);
        app.move_panel_focus(-1, 0);
        assert_eq!(app.focused_panel, 0);

        assert_eq!(
            shifted_panel_direction(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            Some((-1, 0))
        );
        assert_eq!(
            shifted_panel_direction(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::SHIFT)),
            Some((1, 0))
        );
        assert_eq!(
            shifted_panel_direction(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn creates_vault_from_options_input() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("current");
        fs::create_dir(&current).unwrap();
        let vault = current.canonicalize().unwrap();
        let database = Database::open(&vault).unwrap();
        let mut app = App::new(vault, database).unwrap();
        let target = dir.path().join("new-vault");
        app.mode = Mode::VaultInput;
        app.create_vault = true;
        app.query = target.display().to_string();

        assert!(
            app.handle_vault_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap()
        );
        assert_eq!(app.next_vault, Some(target.canonicalize().unwrap()));
    }

    #[test]
    fn finds_only_unreferenced_images() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("assets")).unwrap();
        fs::write(dir.path().join("assets/used.png"), b"used").unwrap();
        fs::write(dir.path().join("assets/unused.jpg"), b"unused").unwrap();
        fs::write(
            dir.path().join("note.md"),
            "# Note\n\n![Used](assets/used.png)\n",
        )
        .unwrap();

        assert_eq!(
            find_orphan_images(dir.path()).unwrap(),
            vec![PathBuf::from("assets/unused.jpg")]
        );
    }

    #[test]
    fn creates_safe_note_slugs() {
        assert_eq!(
            slugify("Rust: Ownership & Borrowing"),
            "rust-ownership-borrowing"
        );
    }
}
