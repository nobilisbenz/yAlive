use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::{Local, TimeZone, Utc};
use fsrs::{FSRS, MemoryState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::config::ReviewOrder;
use crate::model::{
    CardContent, CardRow, DeckRow, Diagnostic, NoteRow, ParsedNote, RelationRow, ReviewCard,
    ReviewSectionRow, SectionRow, Statistics,
};
use crate::parser::{markdown_files, parse_note};

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open(vault: &Path) -> Result<Self> {
        let notes_dir = vault.join(".notes");
        fs::create_dir_all(&notes_dir)?;
        let connection = Connection::open(notes_dir.join("index.sqlite"))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                note_id TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                content_hash TEXT NOT NULL,
                modified_at INTEGER NOT NULL,
                topic TEXT,
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS sections (
                id INTEGER PRIMARY KEY,
                section_uid TEXT NOT NULL UNIQUE,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                parent_uid TEXT,
                heading TEXT NOT NULL,
                heading_level INTEGER NOT NULL,
                start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL,
                start_line INTEGER NOT NULL,
                position INTEGER NOT NULL,
                body TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relations (
                source_section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
                target_section_uid TEXT NOT NULL,
                relation_type TEXT NOT NULL DEFAULT 'related',
                context TEXT,
                PRIMARY KEY (source_section_id, target_section_uid, relation_type)
             );
             CREATE TABLE IF NOT EXISTS cards (
                id INTEGER PRIMARY KEY,
                card_uid TEXT NOT NULL UNIQUE,
                section_uid TEXT NOT NULL,
                quiz_id TEXT NOT NULL,
                card_type TEXT NOT NULL,
                variant_key TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                definition TEXT NOT NULL,
                suspended INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS review_state (
                card_id INTEGER PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
                due_at INTEGER NOT NULL,
                stability REAL,
                difficulty REAL,
                last_review_at INTEGER,
                scheduled_days INTEGER NOT NULL DEFAULT 0,
                review_count INTEGER NOT NULL DEFAULT 0,
                lapse_count INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS review_log (
                id INTEGER PRIMARY KEY,
                card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
                card_uid TEXT NOT NULL,
                reviewed_at INTEGER NOT NULL,
                rating INTEGER NOT NULL,
                answer_correct INTEGER,
                response_ms INTEGER,
                submitted_value TEXT,
                elapsed_days INTEGER NOT NULL,
                scheduled_days INTEGER NOT NULL,
                stability_before REAL,
                stability_after REAL,
                difficulty_before REAL,
                difficulty_after REAL
             );
             CREATE TABLE IF NOT EXISTS decks (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS card_decks (
                card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
                deck_id INTEGER NOT NULL REFERENCES decks(id) ON DELETE CASCADE,
                PRIMARY KEY(card_id, deck_id)
             );
             CREATE TABLE IF NOT EXISTS diagnostics (
                path TEXT NOT NULL,
                line INTEGER NOT NULL,
                message TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS section_search USING fts5(
                section_uid UNINDEXED, note_title, heading, body, tags
             );",
        )?;
        add_column(&connection, "files", "topic", "TEXT")?;
        add_column(&connection, "files", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(
            &connection,
            "files",
            "created_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        connection.execute(
            "UPDATE files SET created_at=modified_at WHERE created_at=0",
            [],
        )?;
        Ok(Self { connection })
    }

    pub fn index_vault(&mut self, vault: &Path) -> Result<IndexSummary> {
        let paths = markdown_files(vault);
        let present: HashSet<String> = paths
            .iter()
            .filter_map(|path| path.strip_prefix(vault).ok())
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        let mut summary = IndexSummary::default();
        for path in paths {
            let relative = path.strip_prefix(vault)?.to_string_lossy().to_string();
            let source = fs::read(&path)?;
            let hash = blake3::hash(&source).to_hex().to_string();
            let existing: Option<String> = self
                .connection
                .query_row(
                    "SELECT content_hash FROM files WHERE path = ?1",
                    [&relative],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.as_deref() == Some(&hash) {
                summary.unchanged += 1;
                continue;
            }
            match parse_note(&path, vault) {
                Ok(note) => {
                    summary.diagnostics += note.diagnostics.len();
                    self.replace_note(&note)?;
                    summary.indexed += 1;
                }
                Err(error) => {
                    self.replace_diagnostics(
                        Path::new(&relative),
                        &[Diagnostic {
                            path: PathBuf::from(&relative),
                            line: 1,
                            message: error.to_string(),
                        }],
                    )?;
                    summary.failed += 1;
                }
            }
        }
        let stored: Vec<(i64, String)> = {
            let mut statement = self.connection.prepare("SELECT id, path FROM files")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (id, path) in stored {
            if !present.contains(&path) {
                self.connection
                    .execute("DELETE FROM files WHERE id = ?1", [id])?;
                self.connection
                    .execute("DELETE FROM diagnostics WHERE path = ?1", [&path])?;
                summary.removed += 1;
            }
        }
        self.refresh_broken_link_diagnostics()?;
        Ok(summary)
    }

    fn refresh_broken_link_diagnostics(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM diagnostics WHERE message LIKE 'broken link `%'
             ESCAPE '\\'",
            [],
        )?;
        transaction.execute(
            "INSERT INTO diagnostics(path, line, message)
             SELECT f.path, source.start_line, 'broken link `' || r.target_section_uid || '`'
             FROM relations r
             JOIN sections source ON source.id=r.source_section_id
             JOIN files f ON f.id=source.file_id
             LEFT JOIN sections target ON target.section_uid=r.target_section_uid
             WHERE target.id IS NULL",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn replace_note(&mut self, note: &ParsedNote) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let path = note.path.to_string_lossy();
        transaction.execute(
            "INSERT INTO files(path, note_id, title, tags, content_hash, modified_at, topic, pinned, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(note_id) DO UPDATE SET
                path=excluded.path, title=excluded.title, tags=excluded.tags,
                content_hash=excluded.content_hash, modified_at=excluded.modified_at,
                topic=excluded.topic, pinned=excluded.pinned, created_at=excluded.created_at",
            params![
                path,
                note.note_id,
                note.title,
                serde_json::to_string(&note.tags)?,
                note.content_hash,
                note.modified_at,
                note.topic,
                note.pinned,
                note.created_at
            ],
        )?;
        let file_id: i64 = transaction.query_row(
            "SELECT id FROM files WHERE note_id = ?1",
            [&note.note_id],
            |row| row.get(0),
        )?;
        let section_uids: HashSet<_> = note
            .sections
            .iter()
            .map(|section| section.uid.as_str())
            .collect();
        for section in &note.sections {
            transaction.execute(
                "INSERT INTO sections(section_uid, file_id, parent_uid, heading, heading_level,
                    start_byte, end_byte, start_line, position, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(section_uid) DO UPDATE SET
                    file_id=excluded.file_id, parent_uid=excluded.parent_uid,
                    heading=excluded.heading, heading_level=excluded.heading_level,
                    start_byte=excluded.start_byte, end_byte=excluded.end_byte,
                    start_line=excluded.start_line, position=excluded.position, body=excluded.body",
                params![
                    section.uid,
                    file_id,
                    section.parent_uid,
                    section.heading,
                    section.level,
                    section.start_byte,
                    section.end_byte,
                    section.start_line,
                    section.position,
                    section.body
                ],
            )?;
            let section_id: i64 = transaction.query_row(
                "SELECT id FROM sections WHERE section_uid = ?1",
                [&section.uid],
                |row| row.get(0),
            )?;
            transaction.execute(
                "DELETE FROM relations WHERE source_section_id = ?1",
                [section_id],
            )?;
            for relation in &section.relations {
                transaction.execute(
                    "INSERT OR REPLACE INTO relations(source_section_id, target_section_uid, relation_type, context)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![section_id, relation.target_uid, relation.relation_type, relation.context],
                )?;
            }
            transaction.execute("DELETE FROM section_search WHERE rowid = ?1", [section_id])?;
            transaction.execute(
                "INSERT INTO section_search(rowid, section_uid, note_title, heading, body, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    section_id,
                    section.uid,
                    note.title,
                    section.heading,
                    section.body,
                    note.tags.join(" ")
                ],
            )?;
            let card_uids: HashSet<_> =
                section.cards.iter().map(|card| card.uid.as_str()).collect();
            for card in &section.cards {
                transaction.execute(
                    "INSERT INTO cards(card_uid, section_uid, quiz_id, card_type, variant_key, content_hash, definition)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(card_uid) DO UPDATE SET
                       section_uid=excluded.section_uid, quiz_id=excluded.quiz_id,
                       card_type=excluded.card_type, variant_key=excluded.variant_key,
                       content_hash=excluded.content_hash, definition=excluded.definition",
                    params![
                        card.uid,
                        card.section_uid,
                        card.quiz_id,
                        card.card_type,
                        card.variant_key,
                        card.content_hash,
                        serde_json::to_string(&card.content)?
                    ],
                )?;
                transaction.execute(
                    "INSERT OR IGNORE INTO review_state(card_id, due_at)
                     SELECT id, ?2 FROM cards WHERE card_uid = ?1",
                    params![card.uid, Utc::now().timestamp()],
                )?;
            }
            let section_review = serde_json::to_string(&CardContent::Section {
                title: section.heading.clone(),
                body: section.body.clone(),
            })?;
            transaction.execute(
                "UPDATE cards SET definition=?2, content_hash=?3
                 WHERE section_uid=?1 AND card_type='section-review'",
                params![
                    section.uid,
                    section_review,
                    blake3::hash(section_review.as_bytes()).to_hex().to_string()
                ],
            )?;
            delete_stale_cards(&transaction, &section.uid, &card_uids)?;
        }
        delete_stale_sections(&transaction, file_id, &section_uids)?;
        transaction.execute("DELETE FROM diagnostics WHERE path = ?1", [&*path])?;
        for diagnostic in &note.diagnostics {
            transaction.execute(
                "INSERT INTO diagnostics(path, line, message) VALUES (?1, ?2, ?3)",
                params![
                    diagnostic.path.to_string_lossy(),
                    diagnostic.line,
                    diagnostic.message
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn replace_diagnostics(&self, path: &Path, diagnostics: &[Diagnostic]) -> Result<()> {
        let path = path.to_string_lossy();
        self.connection
            .execute("DELETE FROM diagnostics WHERE path = ?1", [&*path])?;
        for diagnostic in diagnostics {
            self.connection.execute(
                "INSERT INTO diagnostics(path, line, message) VALUES (?1, ?2, ?3)",
                params![path, diagnostic.line, diagnostic.message],
            )?;
        }
        Ok(())
    }

    pub fn sections(&self) -> Result<Vec<SectionRow>> {
        self.query_sections(
            "SELECT s.section_uid, f.title, s.heading, s.body, f.path, s.start_line
             FROM sections s JOIN files f ON f.id=s.file_id
             ORDER BY lower(f.title), s.position",
            [],
        )
    }

    pub fn notes(&self) -> Result<Vec<NoteRow>> {
        let mut statement = self.connection.prepare(
            "SELECT title, topic, pinned, created_at, modified_at, path
             FROM files ORDER BY lower(COALESCE(topic, '')), lower(title)",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(NoteRow {
                    title: row.get(0)?,
                    topic: row.get(1)?,
                    pinned: row.get::<_, i64>(2)? != 0,
                    created_at: row.get(3)?,
                    modified_at: row.get(4)?,
                    path: PathBuf::from(row.get::<_, String>(5)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn review_sections(&self) -> Result<Vec<ReviewSectionRow>> {
        let mut statement = self.connection.prepare(
            "SELECT s.section_uid, f.title, s.heading,
                    EXISTS(SELECT 1 FROM cards c WHERE c.section_uid=s.section_uid
                           AND c.card_type='section-review' AND c.suspended=0)
             FROM sections s JOIN files f ON f.id=s.file_id
             ORDER BY lower(f.title), s.position",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(ReviewSectionRow {
                    uid: row.get(0)?,
                    note_title: row.get(1)?,
                    heading: row.get(2)?,
                    enrolled: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn toggle_section_review(&mut self, section_uid: &str) -> Result<bool> {
        let existing: Option<(i64, bool)> = self
            .connection
            .query_row(
                "SELECT id, suspended=0 FROM cards WHERE section_uid=?1 AND card_type='section-review'",
                [section_uid],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        if let Some((id, active)) = existing {
            self.connection.execute(
                "UPDATE cards SET suspended=?2 WHERE id=?1",
                params![id, active],
            )?;
            return Ok(!active);
        }
        let (heading, body): (String, String) = self.connection.query_row(
            "SELECT heading, body FROM sections WHERE section_uid=?1",
            [section_uid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let definition = serde_json::to_string(&CardContent::Section {
            title: heading,
            body,
        })?;
        let uid = format!("{section_uid}/section-review:main");
        self.connection.execute(
            "INSERT INTO cards(card_uid, section_uid, quiz_id, card_type, variant_key, content_hash, definition)
             VALUES (?1, ?2, 'section-review', 'section-review', 'main', ?3, ?4)",
            params![
                uid,
                section_uid,
                blake3::hash(definition.as_bytes()).to_hex().to_string(),
                definition
            ],
        )?;
        self.connection.execute(
            "INSERT INTO review_state(card_id, due_at) SELECT id, ?2 FROM cards WHERE card_uid=?1",
            params![uid, Utc::now().timestamp()],
        )?;
        Ok(true)
    }

    pub fn decks(&self) -> Result<Vec<DeckRow>> {
        let mut statement = self.connection.prepare(
            "SELECT d.id, d.name, COUNT(cd.card_id) FROM decks d
             LEFT JOIN card_decks cd ON cd.deck_id=d.id GROUP BY d.id ORDER BY lower(d.name)",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(DeckRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    card_count: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_deck(&self, name: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO decks(name, created_at) VALUES (?1, ?2)",
            params![name.trim(), Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn delete_deck(&self, id: i64) -> Result<()> {
        self.connection
            .execute("DELETE FROM decks WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn card_rows(&self) -> Result<Vec<CardRow>> {
        let mut statement = self.connection.prepare(
            "SELECT c.id, f.title || ' / ' || s.heading || ' / ' || c.quiz_id,
                    COALESCE(group_concat(cd.deck_id), '')
             FROM cards c JOIN sections s ON s.section_uid=c.section_uid
             JOIN files f ON f.id=s.file_id LEFT JOIN card_decks cd ON cd.card_id=c.id
             WHERE c.suspended=0 GROUP BY c.id ORDER BY lower(f.title), s.position, c.id",
        )?;
        Ok(statement
            .query_map([], |row| {
                let decks: String = row.get(2)?;
                Ok(CardRow {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    decks: decks
                        .split(',')
                        .filter_map(|value| value.parse().ok())
                        .collect(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn review_card(&self, card_id: i64) -> Result<Option<ReviewCard>> {
        Ok(self
            .connection
            .query_row(
                "SELECT c.id, c.card_uid, c.section_uid, c.definition, rs.due_at,
                    rs.stability, rs.difficulty, rs.last_review_at, rs.review_count
             FROM cards c JOIN review_state rs ON rs.card_id=c.id WHERE c.id=?1",
                [card_id],
                review_card_from_row,
            )
            .optional()?)
    }

    pub fn toggle_card_deck(&self, card_id: i64, deck_id: i64) -> Result<bool> {
        let removed = self.connection.execute(
            "DELETE FROM card_decks WHERE card_id=?1 AND deck_id=?2",
            params![card_id, deck_id],
        )?;
        if removed > 0 {
            return Ok(false);
        }
        self.connection.execute(
            "INSERT INTO card_decks(card_id, deck_id) VALUES (?1, ?2)",
            params![card_id, deck_id],
        )?;
        Ok(true)
    }

    pub fn statistics(&self) -> Result<Statistics> {
        let now = Utc::now().timestamp();
        let today = Local::now().date_naive();
        let day_start = Local
            .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        let week_start = day_start - 6 * 86_400;
        let month_start = day_start - 29 * 86_400;
        let (note_count, topic_count, untopiced_count) = self.connection.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT topic),
                    COALESCE(SUM(CASE WHEN topic IS NULL OR trim(topic)='' THEN 1 ELSE 0 END), 0)
             FROM files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let card_count = count(
            &self.connection,
            "SELECT COUNT(*) FROM cards WHERE suspended=0",
            [],
        )?;
        let due_now = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_state rs JOIN cards c ON c.id=rs.card_id
             WHERE c.suspended=0 AND rs.due_at<=?1",
            [now],
        )?;
        let reviewed_today = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_log WHERE reviewed_at>=?1",
            [day_start],
        )?;
        let reviewed_week = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_log WHERE reviewed_at>=?1",
            [week_start],
        )?;
        let accuracy = |start| -> Result<Option<f64>> {
            Ok(self.connection.query_row(
                "SELECT AVG(CASE WHEN COALESCE(answer_correct, rating>1) THEN 1.0 ELSE 0.0 END)
                 FROM review_log WHERE reviewed_at>=?1",
                [start],
                |row| row.get(0),
            )?)
        };
        let average_response_ms = self.connection.query_row(
            "SELECT CAST(AVG(response_ms) AS INTEGER) FROM review_log WHERE reviewed_at>=?1",
            [week_start],
            |row| row.get(0),
        )?;
        let mut rating_counts = [0; 4];
        let mut statement = self.connection.prepare(
            "SELECT rating, COUNT(*) FROM review_log WHERE reviewed_at>=?1 GROUP BY rating",
        )?;
        for row in statement.query_map([month_start], |row| {
            Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?))
        })? {
            let (rating, amount) = row?;
            if (1..=4).contains(&rating) {
                rating_counts[rating - 1] = amount;
            }
        }
        let mut daily_reviews = Vec::new();
        for offset in (0..14).rev() {
            let start = day_start - i64::from(offset) * 86_400;
            daily_reviews.push((
                start,
                count(
                    &self.connection,
                    "SELECT COUNT(*) FROM review_log WHERE reviewed_at>=?1 AND reviewed_at<?2",
                    params![start, start + 86_400],
                )?,
            ));
        }
        let mut streak_days = 0;
        for offset in 0..365 {
            let start = day_start - i64::from(offset) * 86_400;
            if count(
                &self.connection,
                "SELECT COUNT(*) FROM review_log WHERE reviewed_at>=?1 AND reviewed_at<?2",
                params![start, start + 86_400],
            )? == 0
            {
                break;
            }
            streak_days += 1;
        }
        let mut due_forecast = Vec::new();
        for offset in 0..7 {
            let start = day_start + i64::from(offset) * 86_400;
            due_forecast.push((
                start,
                count(
                    &self.connection,
                    "SELECT COUNT(*) FROM review_state rs JOIN cards c ON c.id=rs.card_id
                     WHERE c.suspended=0 AND rs.due_at>=?1 AND rs.due_at<?2",
                    params![start, start + 86_400],
                )?,
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT f.title, COUNT(*),
                    AVG(CASE WHEN COALESCE(l.answer_correct, l.rating>1) THEN 1.0 ELSE 0.0 END) score
             FROM review_log l JOIN cards c ON c.id=l.card_id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE l.reviewed_at>=?1 GROUP BY f.id HAVING COUNT(*)>=2
             ORDER BY score, COUNT(*) DESC LIMIT 5",
        )?;
        let weak_notes = statement
            .query_map([month_start], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Statistics {
            note_count,
            topic_count,
            untopiced_count,
            card_count,
            due_now,
            reviewed_today,
            reviewed_week,
            accuracy_week: accuracy(week_start)?,
            accuracy_month: accuracy(month_start)?,
            average_response_ms,
            streak_days,
            rating_counts,
            daily_reviews,
            due_forecast,
            weak_notes,
        })
    }

    pub fn search(&self, query: &str) -> Result<Vec<SectionRow>> {
        if query.trim().is_empty() {
            return self.sections();
        }
        let fts_query = query
            .split_whitespace()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut statement = self.connection.prepare(
            "SELECT s.section_uid, f.title, s.heading, s.body, f.path, s.start_line
             FROM section_search q
             JOIN sections s ON s.id=q.rowid JOIN files f ON f.id=s.file_id
             WHERE section_search MATCH ?1 ORDER BY bm25(section_search, 0, 2, 5, 1, 1) LIMIT 100",
        )?;
        Ok(statement
            .query_map([fts_query], section_from_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn query_sections<P>(&self, sql: &str, params: P) -> Result<Vec<SectionRow>>
    where
        P: rusqlite::Params,
    {
        let mut statement = self.connection.prepare(sql)?;
        Ok(statement
            .query_map(params, section_from_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn relations(&self, section_uid: &str) -> Result<Vec<RelationRow>> {
        let mut statement = self.connection.prepare(
            "SELECT r.relation_type, r.target_section_uid, target.heading, 0
             FROM relations r JOIN sections source ON source.id=r.source_section_id
             LEFT JOIN sections target ON target.section_uid=r.target_section_uid
             WHERE source.section_uid=?1
             UNION ALL
             SELECT r.relation_type, source.section_uid, source.heading, 1
             FROM relations r JOIN sections source ON source.id=r.source_section_id
             WHERE r.target_section_uid=?1",
        )?;
        Ok(statement
            .query_map([section_uid], |row| {
                Ok(RelationRow {
                    relation_type: row.get(0)?,
                    target_uid: row.get(1)?,
                    target_heading: row.get(2)?,
                    incoming: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn due_cards(
        &self,
        new_cards_per_day: usize,
        max_reviews_per_day: usize,
        review_order: ReviewOrder,
        bury_siblings: bool,
    ) -> Result<Vec<ReviewCard>> {
        let now = Utc::now().timestamp();
        let today = Local::now().date_naive();
        let day_start = Local
            .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        let reviewed_today = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_log WHERE reviewed_at>=?1",
            [day_start],
        )?;
        let introduced_today = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_state WHERE review_count=1 AND last_review_at>=?1",
            [day_start],
        )?;
        let remaining = max_reviews_per_day.saturating_sub(reviewed_today);
        let remaining_new = new_cards_per_day.saturating_sub(introduced_today);
        let mut statement = self.connection.prepare(
            "SELECT c.id, c.card_uid, c.section_uid, c.definition, rs.due_at,
                    rs.stability, rs.difficulty, rs.last_review_at, rs.review_count
             FROM cards c JOIN review_state rs ON rs.card_id=c.id
             WHERE c.suspended=0 AND rs.due_at <= ?1 ORDER BY rs.due_at, c.id",
        )?;
        let mut cards = statement
            .query_map([now], |row| {
                let definition: String = row.get(3)?;
                let content: CardContent = serde_json::from_str(&definition).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        definition.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(ReviewCard {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    section_uid: row.get(2)?,
                    content,
                    due_at: row.get(4)?,
                    stability: row.get(5)?,
                    difficulty: row.get(6)?,
                    last_review_at: row.get(7)?,
                    review_count: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if review_order == ReviewOrder::Random {
            use rand::seq::SliceRandom;
            cards.shuffle(&mut rand::rng());
        }
        let mut new_cards = 0;
        let mut sections = HashSet::new();
        cards.retain(|card| {
            if card.review_count == 0 {
                if new_cards >= remaining_new {
                    return false;
                }
                new_cards += 1;
            }
            !bury_siblings || sections.insert(card.section_uid.clone())
        });
        cards.truncate(remaining);
        Ok(cards)
    }

    pub fn record_review(
        &mut self,
        card: &ReviewCard,
        rating: u32,
        correct: Option<bool>,
        response_ms: i64,
        submitted: Option<&str>,
        desired_retention: f32,
    ) -> Result<Option<u32>> {
        if !(1..=4).contains(&rating) {
            return Err(anyhow!("rating must be between 1 and 4"));
        }
        let now = Utc::now().timestamp();
        let elapsed_days = card
            .last_review_at
            .map(|last| ((now - last) / 86_400).max(0) as u32)
            .unwrap_or(0);
        let current = match (card.stability, card.difficulty) {
            (Some(stability), Some(difficulty)) => Some(MemoryState {
                stability,
                difficulty,
            }),
            _ => None,
        };
        let fsrs = FSRS::new(Some(&[]))?;
        let states = fsrs.next_states(current, desired_retention, elapsed_days)?;
        let next = match rating {
            1 => states.again,
            2 => states.hard,
            3 => states.good,
            4 => states.easy,
            _ => unreachable!(),
        };
        let scheduled_days = next.interval.round().max(1.0) as u32;
        let due_at = now + i64::from(scheduled_days) * 86_400;
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE review_state SET due_at=?2, stability=?3, difficulty=?4,
                last_review_at=?5, scheduled_days=?6, review_count=review_count+1,
                lapse_count=lapse_count + CASE WHEN ?7=1 THEN 1 ELSE 0 END WHERE card_id=?1",
            params![
                card.id,
                due_at,
                next.memory.stability,
                next.memory.difficulty,
                now,
                scheduled_days,
                rating
            ],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO review_log(card_id, card_uid, reviewed_at, rating, answer_correct,
                response_ms, submitted_value, elapsed_days, scheduled_days, stability_before,
                stability_after, difficulty_before, difficulty_after)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                card.id,
                card.uid,
                now,
                rating,
                correct.map(i64::from),
                response_ms,
                submitted,
                elapsed_days,
                scheduled_days,
                card.stability,
                next.memory.stability,
                card.difficulty,
                next.memory.difficulty
            ],
        )?;
        transaction.commit()?;
        Ok(Some(scheduled_days))
    }

    pub fn diagnostics(&self) -> Result<Vec<Diagnostic>> {
        let mut statement = self
            .connection
            .prepare("SELECT path, line, message FROM diagnostics ORDER BY path, line")?;
        Ok(statement
            .query_map([], |row| {
                Ok(Diagnostic {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    line: row.get(1)?,
                    message: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn export_reviews(&self, path: &Path) -> Result<usize> {
        let mut statement = self.connection.prepare(
            "SELECT card_uid, reviewed_at, rating, answer_correct, response_ms,
                    elapsed_days, scheduled_days, stability_before, stability_after,
                    difficulty_before, difficulty_after FROM review_log ORDER BY reviewed_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(serde_json::json!({
                "card_uid": row.get::<_, String>(0)?,
                "reviewed_at": row.get::<_, i64>(1)?,
                "rating": row.get::<_, i64>(2)?,
                "answer_correct": row.get::<_, Option<i64>>(3)?.map(|v| v != 0),
                "response_ms": row.get::<_, i64>(4)?,
                "elapsed_days": row.get::<_, i64>(5)?,
                "scheduled_days": row.get::<_, i64>(6)?,
                "stability_before": row.get::<_, Option<f64>>(7)?,
                "stability_after": row.get::<_, Option<f64>>(8)?,
                "difficulty_before": row.get::<_, Option<f64>>(9)?,
                "difficulty_after": row.get::<_, Option<f64>>(10)?,
            }))
        })?;
        let values = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let output = values
            .iter()
            .map(serde_json::to_string)
            .collect::<serde_json::Result<Vec<_>>>()?
            .join("\n");
        fs::write(
            path,
            if output.is_empty() {
                output
            } else {
                output + "\n"
            },
        )
        .with_context(|| format!("writing {}", path.display()))?;
        Ok(values.len())
    }
}

fn section_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SectionRow> {
    Ok(SectionRow {
        uid: row.get(0)?,
        note_title: row.get(1)?,
        heading: row.get(2)?,
        body: row.get(3)?,
        path: PathBuf::from(row.get::<_, String>(4)?),
        start_line: row.get(5)?,
    })
}

fn review_card_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewCard> {
    let definition: String = row.get(3)?;
    let content: CardContent = serde_json::from_str(&definition).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            definition.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(ReviewCard {
        id: row.get(0)?,
        uid: row.get(1)?,
        section_uid: row.get(2)?,
        content,
        due_at: row.get(4)?,
        stability: row.get(5)?,
        difficulty: row.get(6)?,
        last_review_at: row.get(7)?,
        review_count: row.get(8)?,
    })
}

fn delete_stale_cards(
    transaction: &Transaction<'_>,
    section_uid: &str,
    keep: &HashSet<&str>,
) -> Result<()> {
    let existing: Vec<(i64, String)> = {
        let mut statement =
            transaction.prepare("SELECT id, card_uid FROM cards WHERE section_uid=?1")?;
        statement
            .query_map([section_uid], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, uid) in existing {
        if !keep.contains(uid.as_str()) {
            transaction.execute(
                "DELETE FROM cards WHERE id=?1 AND card_type!='section-review'",
                [id],
            )?;
        }
    }
    Ok(())
}

fn add_column(connection: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn count<P: rusqlite::Params>(connection: &Connection, sql: &str, params: P) -> Result<usize> {
    Ok(connection.query_row(sql, params, |row| row.get(0))?)
}

fn delete_stale_sections(
    transaction: &Transaction<'_>,
    file_id: i64,
    keep: &HashSet<&str>,
) -> Result<()> {
    let existing: Vec<(i64, String)> = {
        let mut statement =
            transaction.prepare("SELECT id, section_uid FROM sections WHERE file_id=?1")?;
        statement
            .query_map([file_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, uid) in existing {
        if !keep.contains(uid.as_str()) {
            transaction.execute("DELETE FROM cards WHERE section_uid=?1", [&uid])?;
            transaction.execute("DELETE FROM section_search WHERE rowid=?1", [id])?;
            transaction.execute("DELETE FROM sections WHERE id=?1", [id])?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct IndexSummary {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub failed: usize,
    pub diagnostics: usize,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn indexes_searches_and_preserves_card_state() {
        let dir = tempdir().unwrap();
        let source = r#"---
id: test
title: Test Note
topic: Rust
pinned: true
---
# Root {#root}
## Memory safety {#memory}
Unique mutable access.
```quiz
id: rule
type: cloze
prompt: One {{c1::writer}}.
```
"#;
        fs::write(dir.path().join("test.md"), source).unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        assert_eq!(db.index_vault(dir.path()).unwrap().indexed, 1);
        let note = db.notes().unwrap().remove(0);
        assert_eq!(note.topic.as_deref(), Some("Rust"));
        assert!(note.pinned);
        assert_eq!(db.search("mutable").unwrap().len(), 1);
        assert!(db.toggle_section_review("test#memory").unwrap());
        assert!(
            db.review_sections()
                .unwrap()
                .into_iter()
                .any(|section| section.uid == "test#memory" && section.enrolled)
        );
        db.create_deck("Rust").unwrap();
        let deck = db.decks().unwrap().remove(0);
        let card_row = db.card_rows().unwrap().remove(0);
        assert!(db.toggle_card_deck(card_row.id, deck.id).unwrap());
        assert!(db.card_rows().unwrap()[0].decks.contains(&deck.id));
        let card = db
            .due_cards(20, 200, ReviewOrder::Due, false)
            .unwrap()
            .remove(0);
        assert!(
            db.record_review(&card, 3, Some(true), 100, None, 0.9)
                .unwrap()
                .is_some()
        );
        let statistics = db.statistics().unwrap();
        assert_eq!(statistics.reviewed_today, 1);
        assert_eq!(statistics.note_count, 1);
        fs::write(
            dir.path().join("test.md"),
            source.replace("Unique", "Exclusive"),
        )
        .unwrap();
        db.index_vault(dir.path()).unwrap();
        let due = db.due_cards(20, 200, ReviewOrder::Due, false).unwrap();
        assert!(due.iter().all(|due_card| due_card.uid != card.uid));
        assert!(due.iter().any(|due_card| matches!(
            &due_card.content,
            CardContent::Section { body, .. } if body.contains("Exclusive")
        )));
    }

    #[test]
    fn deleted_cards_are_reported_as_stale_instead_of_failing() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("test.md"),
            "# Test {#root}\n\n```quiz\nid: missing\ntype: cloze\nprompt: '{{c1::answer}}'\n```\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        let card = db
            .due_cards(20, 200, ReviewOrder::Due, false)
            .unwrap()
            .remove(0);

        db.connection
            .execute("DELETE FROM cards WHERE id=?1", [card.id])
            .unwrap();

        assert!(db.review_card(card.id).unwrap().is_none());
        assert!(
            db.record_review(&card, 3, Some(true), 100, None, 0.9)
                .unwrap()
                .is_none()
        );
    }
}
