use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::{Local, TimeZone, Utc};
use fsrs::{FSRS, MemoryState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::config::ReviewOrder;
use crate::model::{
    ActionRow, ArchivedItem, ContradictionPair, CardContent, CardRow, DeckRow, Diagnostic, GraphData, GraphLink, GraphNote,
    GraphSection, MobileCard, MobileDeck, MobileSnapshot, NoteRow, ParsedNote, RelationRow,
    ReviewCard, ReviewEvent, ReviewScope, ReviewSectionRow, SectionRow, Statistics,
};
use crate::parser::{markdown_files, parse_note};
use crate::search::Bm25Weights;

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open(vault: &Path) -> Result<Self> {
        let notes_dir = vault.join(".notes");
        fs::create_dir_all(&notes_dir)?;
        let connection = Connection::open(notes_dir.join("index.sqlite"))?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        connection.execute_batch(
            // PRAGMAs are per-connection, not per-database, so every one of these has to
            // be set here rather than once at creation.
            //
            // `synchronous = NORMAL` is the correct pairing with WAL: FULL costs an fsync
            // per commit, and the durability it buys back is only against OS crashes —
            // WAL already survives a process crash. On a reindex that writes a row per
            // section, the difference is the whole runtime.
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA cache_size   = -32000;
             PRAGMA temp_store   = MEMORY;
             PRAGMA mmap_size    = 268435456;
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
                heading_path TEXT NOT NULL DEFAULT '',
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
             CREATE TABLE IF NOT EXISTS synced_review_events (
                event_id TEXT PRIMARY KEY,
                device_id TEXT NOT NULL,
                imported_at INTEGER NOT NULL
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
             -- `@action` lines, parsed in trusted code from what the author wrote.
             -- A language model never writes a row here (spec §3.3, §48): it may mention
             -- an action in prose, but the buttons come from this table. Note there is no
             -- column that could hold a command — only a typed target.
             CREATE TABLE IF NOT EXISTS actions (
                id INTEGER PRIMARY KEY,
                section_id INTEGER NOT NULL REFERENCES sections(id) ON DELETE CASCADE,
                kind TEXT NOT NULL,
                target TEXT NOT NULL,
                line INTEGER,
                timestamp_seconds INTEGER,
                position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS diagnostics (
                path TEXT NOT NULL,
                line INTEGER NOT NULL,
                message TEXT NOT NULL
             );
             -- `section_search` is not here: it is external-content FTS5 and its shape
             -- can change, so `migrate_search` owns creating and rebuilding it.
             --
             -- Everything below rides on primary keys otherwise.
             --
             -- `relations` has PK (source_section_id, target_section_uid, relation_type),
             -- so forward traversal is covered and **reverse traversal is a full table
             -- scan**. Backlinks are half of graph expansion, so that scan would land
             -- once per hop per query in the interactive path.
             CREATE INDEX IF NOT EXISTS relations_target ON relations(target_section_uid);
             CREATE INDEX IF NOT EXISTS sections_file    ON sections(file_id);
             CREATE INDEX IF NOT EXISTS sections_parent  ON sections(parent_uid);
             CREATE INDEX IF NOT EXISTS cards_section    ON cards(section_uid);
             CREATE INDEX IF NOT EXISTS actions_section  ON actions(section_id);",
        )?;
        add_column(&connection, "files", "topic", "TEXT")?;
        add_column(&connection, "files", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(
            &connection,
            "files",
            "created_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column(&connection, "files", "archived_at", "INTEGER")?;
        add_column(&connection, "files", "status", "TEXT")?;
        add_column(&connection, "sections", "archived_at", "INTEGER")?;
        // Denormalised from the parser's heading stack rather than derived with a
        // recursive CTE at read time: it is shown on every retrieved source, so it is
        // read far more often than it is written, and the write already has the stack
        // in hand.
        add_column(&connection, "sections", "heading_path", "TEXT NOT NULL DEFAULT ''")?;
        add_column(&connection, "cards", "archived_at", "INTEGER")?;
        add_column(&connection, "decks", "archived_at", "INTEGER")?;
        connection.execute(
            "UPDATE files SET created_at=modified_at WHERE created_at=0",
            [],
        )?;
        // After `add_column`, because the search index reads `sections.heading_path`.
        migrate_search(&connection)?;
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
            "INSERT INTO files(path, note_id, title, tags, content_hash, modified_at, topic, pinned, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(note_id) DO UPDATE SET
                path=excluded.path, title=excluded.title, tags=excluded.tags,
                content_hash=excluded.content_hash, modified_at=excluded.modified_at,
                topic=excluded.topic, pinned=excluded.pinned, created_at=excluded.created_at,
                status=excluded.status",
            params![
                path,
                note.note_id,
                note.title,
                serde_json::to_string(&note.tags)?,
                note.content_hash,
                note.modified_at,
                note.topic,
                note.pinned,
                note.created_at,
                note.status
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
                "INSERT INTO sections(section_uid, file_id, parent_uid, heading, heading_path,
                    heading_level, start_byte, end_byte, start_line, position, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(section_uid) DO UPDATE SET
                    file_id=excluded.file_id, parent_uid=excluded.parent_uid,
                    heading=excluded.heading, heading_path=excluded.heading_path,
                    heading_level=excluded.heading_level,
                    start_byte=excluded.start_byte, end_byte=excluded.end_byte,
                    start_line=excluded.start_line, position=excluded.position, body=excluded.body",
                params![
                    section.uid,
                    file_id,
                    section.parent_uid,
                    section.heading,
                    section.heading_path,
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
            transaction.execute("DELETE FROM actions WHERE section_id = ?1", [section_id])?;
            for (position, action) in section.actions.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO actions(section_id, kind, target, line, timestamp_seconds, position)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        section_id,
                        action.kind,
                        action.target,
                        action.line,
                        action.timestamp_seconds,
                        position as i64
                    ],
                )?;
            }
            for relation in &section.relations {
                transaction.execute(
                    "INSERT OR REPLACE INTO relations(source_section_id, target_section_uid, relation_type, context)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![section_id, relation.target_uid, relation.relation_type, relation.context],
                )?;
            }
            // The search index is maintained by trigger (see `SEARCH_SCHEMA`), so the
            // section upsert above has already updated it.
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

    /// Fetch specific sections by `section_uid`.
    ///
    /// Graph expansion works in node indices and knows only a section's uid and heading, so
    /// anything it reaches has to be resolved to a full row before it can be shown or
    /// packed into a prompt. Returned in no particular order — the caller has its own
    /// ranking and would only have to undo one imposed here.
    pub fn sections_by_uids(&self, uids: &[String]) -> Result<Vec<SectionRow>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        // `IN` with a generated placeholder list rather than one query per uid: expansion
        // routinely reaches tens of sections and this is on the interactive path.
        let placeholders = std::iter::repeat_n("?", uids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT s.section_uid, f.title, s.heading, s.heading_path, s.body, f.path,
                    s.start_line, f.status
             FROM sections s JOIN files f ON f.id=s.file_id
             WHERE s.section_uid IN ({placeholders})
               AND s.archived_at IS NULL AND f.archived_at IS NULL"
        );
        self.query_sections(&sql, rusqlite::params_from_iter(uids))
    }

    /// Pairs joined by `contradicts::` where **neither side has been resolved**.
    ///
    /// A `contradicts::` edge is a judgement: two sections of your own vault disagree. That
    /// is fine once one of them is marked `obsolete`, `archived`, or superseded by the
    /// other — the disagreement has an answer, and ranking already demotes the loser.
    ///
    /// What this finds is the case with *no* answer: both sides current, both retrievable,
    /// and nothing to choose between them. That is precisely what makes an assistant answer
    /// confidently and wrongly, because whichever one BM25 happens to prefer becomes the
    /// truth. Reported as vault health rather than fixed automatically — only the author
    /// knows which one is right.
    ///
    /// Second use: an unresolved contradiction is an excellent flashcard.
    pub fn unresolved_contradictions(&self) -> Result<Vec<ContradictionPair>> {
        let mut statement = self.connection.prepare(
            "SELECT src.section_uid, src.heading, srcf.path, tgt.section_uid, tgt.heading, tgtf.path
             FROM relations r
             JOIN sections src  ON src.id = r.source_section_id
             JOIN files    srcf ON srcf.id = src.file_id
             JOIN sections tgt  ON tgt.section_uid = r.target_section_uid
             JOIN files    tgtf ON tgtf.id = tgt.file_id
             WHERE r.relation_type = 'contradicts'
               AND src.archived_at IS NULL AND tgt.archived_at IS NULL
               AND srcf.archived_at IS NULL AND tgtf.archived_at IS NULL
               AND COALESCE(srcf.status, 'current') NOT IN ('obsolete', 'archived')
               AND COALESCE(tgtf.status, 'current') NOT IN ('obsolete', 'archived')
               -- A `supersedes` edge either way *is* the resolution.
               AND NOT EXISTS (
                   SELECT 1 FROM relations s
                   WHERE s.relation_type = 'supersedes'
                     AND ((s.source_section_id = src.id
                           AND s.target_section_uid = tgt.section_uid)
                       OR (s.source_section_id = tgt.id
                           AND s.target_section_uid = src.section_uid))
               )",
        )?;

        let rows = statement
            .query_map([], |row| {
                Ok(ContradictionPair {
                    left_uid: row.get(0)?,
                    left_heading: row.get(1)?,
                    left_path: PathBuf::from(row.get::<_, String>(2)?),
                    right_uid: row.get(3)?,
                    right_heading: row.get(4)?,
                    right_path: PathBuf::from(row.get::<_, String>(5)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // `a contradicts b` and `b contradicts a` are one disagreement, not two.
        let mut seen = HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|pair| {
                let key = if pair.left_uid <= pair.right_uid {
                    (pair.left_uid.clone(), pair.right_uid.clone())
                } else {
                    (pair.right_uid.clone(), pair.left_uid.clone())
                };
                seen.insert(key)
            })
            .collect())
    }

    /// The `@action` rows declared on the given sections.
    ///
    /// Returned with their `section_uid` so the caller can order them by the rank of the
    /// section that declared them — the top source's actions are the ones `Alt+1` should
    /// reach.
    pub fn actions_for(&self, section_uids: &[String]) -> Result<Vec<ActionRow>> {
        if section_uids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", section_uids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT s.section_uid, a.kind, a.target, a.line, a.timestamp_seconds
             FROM actions a JOIN sections s ON s.id = a.section_id
             WHERE s.section_uid IN ({placeholders}) AND s.archived_at IS NULL
             ORDER BY a.section_id, a.position"
        );
        let mut statement = self.connection.prepare(&sql)?;
        Ok(statement
            .query_map(rusqlite::params_from_iter(section_uids), |row| {
                Ok(ActionRow {
                    section_uid: row.get(0)?,
                    kind: row.get(1)?,
                    target: row.get(2)?,
                    line: row.get(3)?,
                    timestamp_seconds: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// How much is indexed, for `brainctl status` and the daemon's status report.
    ///
    /// Counts exclude archived rows, so it reports what is actually searchable rather than
    /// what is stored.
    pub fn counts(&self) -> Result<Counts> {
        Ok(Counts {
            documents: count(
                &self.connection,
                "SELECT count(*) FROM files WHERE archived_at IS NULL",
                [],
            )?,
            sections: count(
                &self.connection,
                "SELECT count(*) FROM sections s JOIN files f ON f.id=s.file_id
                 WHERE s.archived_at IS NULL AND f.archived_at IS NULL",
                [],
            )?,
            relations: count(&self.connection, "SELECT count(*) FROM relations", [])?,
        })
    }

    pub fn sections(&self) -> Result<Vec<SectionRow>> {
        self.query_sections(
            "SELECT s.section_uid, f.title, s.heading, s.heading_path, s.body, f.path, s.start_line, f.status
             FROM sections s JOIN files f ON f.id=s.file_id
             WHERE f.archived_at IS NULL AND s.archived_at IS NULL
             ORDER BY lower(f.title), s.position",
            [],
        )
    }

    pub fn notes(&self) -> Result<Vec<NoteRow>> {
        let mut statement = self.connection.prepare(
            "SELECT title, topic, pinned, created_at, modified_at, path
             FROM files WHERE archived_at IS NULL
             ORDER BY lower(COALESCE(topic, '')), lower(title)",
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
             WHERE f.archived_at IS NULL AND s.archived_at IS NULL
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
            "SELECT d.id, d.name, COUNT(CASE WHEN c.archived_at IS NULL
                    AND s.archived_at IS NULL AND f.archived_at IS NULL THEN 1 END)
             FROM decks d LEFT JOIN card_decks cd ON cd.deck_id=d.id
             LEFT JOIN cards c ON c.id=cd.card_id
             LEFT JOIN sections s ON s.section_uid=c.section_uid
             LEFT JOIN files f ON f.id=s.file_id
             WHERE d.archived_at IS NULL GROUP BY d.id ORDER BY lower(d.name)",
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

    pub fn archive_note(&self, path: &Path) -> Result<()> {
        self.connection.execute(
            "UPDATE files SET archived_at=?2 WHERE path=?1",
            params![path.to_string_lossy(), Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn archive_section(&self, uid: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE sections SET archived_at=?2 WHERE section_uid=?1",
            params![uid, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn archive_quiz(&self, card_id: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE cards SET archived_at=?2
             WHERE (section_uid, quiz_id)=(SELECT section_uid, quiz_id FROM cards WHERE id=?1)",
            params![card_id, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn archive_deck(&self, id: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE decks SET archived_at=?2 WHERE id=?1",
            params![id, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn archived_items(&self) -> Result<Vec<ArchivedItem>> {
        let mut items = Vec::new();
        let mut statement = self.connection.prepare(
            "SELECT f.note_id, f.title, f.path, COUNT(DISTINCT s.id),
                    COUNT(DISTINCT CASE WHEN c.card_type!='section-review'
                                       THEN c.section_uid || char(0) || c.quiz_id END)
             FROM files f LEFT JOIN sections s ON s.file_id=f.id
             LEFT JOIN cards c ON c.section_uid=s.section_uid
             WHERE f.archived_at IS NOT NULL GROUP BY f.id ORDER BY f.archived_at DESC",
        )?;
        items.extend(
            statement
                .query_map([], |row| {
                    Ok(ArchivedItem::Note {
                        note_id: row.get(0)?,
                        title: row.get(1)?,
                        path: PathBuf::from(row.get::<_, String>(2)?),
                        section_count: row.get(3)?,
                        quiz_count: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        let mut statement = self.connection.prepare(
            "SELECT s.section_uid, f.title, s.heading, f.path, s.start_line,
                    COUNT(DISTINCT CASE WHEN c.card_type!='section-review' THEN c.quiz_id END)
             FROM sections s JOIN files f ON f.id=s.file_id
             LEFT JOIN cards c ON c.section_uid=s.section_uid
             WHERE s.archived_at IS NOT NULL AND f.archived_at IS NULL
             GROUP BY s.id ORDER BY s.archived_at DESC",
        )?;
        items.extend(
            statement
                .query_map([], |row| {
                    Ok(ArchivedItem::Section {
                        uid: row.get(0)?,
                        note_title: row.get(1)?,
                        heading: row.get(2)?,
                        path: PathBuf::from(row.get::<_, String>(3)?),
                        start_line: row.get(4)?,
                        quiz_count: row.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        let mut statement = self.connection.prepare(
            "SELECT c.section_uid, c.quiz_id,
                    f.title || ' / ' || s.heading || ' / ' || c.quiz_id, COUNT(*)
             FROM cards c JOIN sections s ON s.section_uid=c.section_uid
             JOIN files f ON f.id=s.file_id
             WHERE c.archived_at IS NOT NULL AND c.card_type!='section-review'
               AND s.archived_at IS NULL AND f.archived_at IS NULL
             GROUP BY c.section_uid, c.quiz_id ORDER BY MAX(c.archived_at) DESC",
        )?;
        items.extend(
            statement
                .query_map([], |row| {
                    Ok(ArchivedItem::Quiz {
                        section_uid: row.get(0)?,
                        quiz_id: row.get(1)?,
                        label: row.get(2)?,
                        card_count: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        let mut statement = self.connection.prepare(
            "SELECT d.id, d.name, COUNT(DISTINCT c.section_uid || char(0) || c.quiz_id)
             FROM decks d LEFT JOIN card_decks cd ON cd.deck_id=d.id
             LEFT JOIN cards c ON c.id=cd.card_id AND c.card_type!='section-review'
             WHERE d.archived_at IS NOT NULL GROUP BY d.id ORDER BY d.archived_at DESC",
        )?;
        items.extend(
            statement
                .query_map([], |row| {
                    Ok(ArchivedItem::Deck {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        quiz_count: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        Ok(items)
    }

    pub fn restore(&self, item: &ArchivedItem) -> Result<()> {
        match item {
            ArchivedItem::Note { note_id, .. } => {
                self.connection.execute(
                    "UPDATE files SET archived_at=NULL WHERE note_id=?1",
                    [note_id],
                )?;
            }
            ArchivedItem::Section { uid, .. } => {
                self.connection.execute(
                    "UPDATE sections SET archived_at=NULL WHERE section_uid=?1",
                    [uid],
                )?;
            }
            ArchivedItem::Quiz {
                section_uid,
                quiz_id,
                ..
            } => {
                self.connection.execute(
                    "UPDATE cards SET archived_at=NULL WHERE section_uid=?1 AND quiz_id=?2",
                    params![section_uid, quiz_id],
                )?;
            }
            ArchivedItem::Deck { id, .. } => {
                self.connection
                    .execute("UPDATE decks SET archived_at=NULL WHERE id=?1", [id])?;
            }
        }
        Ok(())
    }

    pub fn card_rows(&self) -> Result<Vec<CardRow>> {
        let mut statement = self.connection.prepare(
            "SELECT c.id, f.title || ' / ' || s.heading || ' / ' || c.quiz_id,
                    COALESCE(group_concat(CASE WHEN d.archived_at IS NULL THEN d.id END), ''),
                    c.section_uid, c.card_type
             FROM cards c JOIN sections s ON s.section_uid=c.section_uid
             JOIN files f ON f.id=s.file_id LEFT JOIN card_decks cd ON cd.card_id=c.id
             LEFT JOIN decks d ON d.id=cd.deck_id
             WHERE c.suspended=0 AND c.archived_at IS NULL AND s.archived_at IS NULL
               AND f.archived_at IS NULL
               AND (NOT EXISTS(SELECT 1 FROM card_decks any_cd WHERE any_cd.card_id=c.id)
                    OR EXISTS(SELECT 1 FROM card_decks active_cd JOIN decks active_d
                              ON active_d.id=active_cd.deck_id
                              WHERE active_cd.card_id=c.id AND active_d.archived_at IS NULL))
             GROUP BY c.id ORDER BY lower(f.title), s.position, c.id",
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
                    section_uid: row.get(3)?,
                    card_type: row.get(4)?,
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
             FROM files WHERE archived_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let card_count = count(
            &self.connection,
            "SELECT COUNT(*) FROM cards c JOIN sections s ON s.section_uid=c.section_uid
             JOIN files f ON f.id=s.file_id WHERE c.suspended=0 AND c.archived_at IS NULL
             AND s.archived_at IS NULL AND f.archived_at IS NULL
             AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                  OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                            WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
            [],
        )?;
        let due_now = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_state rs JOIN cards c ON c.id=rs.card_id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE c.suspended=0 AND c.archived_at IS NULL AND s.archived_at IS NULL
               AND f.archived_at IS NULL AND rs.due_at<=?1
               AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                    OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                              WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
            [now],
        )?;
        let reviewed_today = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_log l JOIN cards c ON c.id=l.card_id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE l.reviewed_at>=?1 AND c.archived_at IS NULL AND s.archived_at IS NULL
               AND f.archived_at IS NULL
               AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                    OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                              WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
            [day_start],
        )?;
        let reviewed_week = count(
            &self.connection,
            "SELECT COUNT(*) FROM review_log l JOIN cards c ON c.id=l.card_id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE l.reviewed_at>=?1 AND c.archived_at IS NULL AND s.archived_at IS NULL
               AND f.archived_at IS NULL
               AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                    OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                              WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
            [week_start],
        )?;
        let accuracy = |start| -> Result<Option<f64>> {
            Ok(self.connection.query_row(
                "SELECT AVG(CASE WHEN COALESCE(answer_correct, rating>1) THEN 1.0 ELSE 0.0 END)
                 FROM review_log l JOIN cards c ON c.id=l.card_id
                 JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
                 WHERE reviewed_at>=?1 AND c.archived_at IS NULL AND s.archived_at IS NULL
                   AND f.archived_at IS NULL
                   AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                        OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                                  WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
                [start],
                |row| row.get(0),
            )?)
        };
        let average_response_ms = self.connection.query_row(
            "SELECT CAST(AVG(response_ms) AS INTEGER) FROM review_log l
             JOIN cards c ON c.id=l.card_id JOIN sections s ON s.section_uid=c.section_uid
             JOIN files f ON f.id=s.file_id WHERE reviewed_at>=?1 AND c.archived_at IS NULL
             AND s.archived_at IS NULL AND f.archived_at IS NULL
             AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                  OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                            WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
            [week_start],
            |row| row.get(0),
        )?;
        let mut rating_counts = [0; 4];
        let mut statement = self.connection.prepare(
            "SELECT rating, COUNT(*) FROM review_log l JOIN cards c ON c.id=l.card_id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE reviewed_at>=?1 AND c.archived_at IS NULL AND s.archived_at IS NULL
             AND f.archived_at IS NULL
             AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                  OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                            WHERE cd.card_id=c.id AND d.archived_at IS NULL))
             GROUP BY rating",
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
                    "SELECT COUNT(*) FROM review_log l JOIN cards c ON c.id=l.card_id
                     JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
                     WHERE reviewed_at>=?1 AND reviewed_at<?2 AND c.archived_at IS NULL
                     AND s.archived_at IS NULL AND f.archived_at IS NULL
                     AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                          OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                                    WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
                    params![start, start + 86_400],
                )?,
            ));
        }
        let mut streak_days = 0;
        for offset in 0..365 {
            let start = day_start - i64::from(offset) * 86_400;
            if count(
                &self.connection,
                "SELECT COUNT(*) FROM review_log l JOIN cards c ON c.id=l.card_id
                 JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
                 WHERE reviewed_at>=?1 AND reviewed_at<?2 AND c.archived_at IS NULL
                 AND s.archived_at IS NULL AND f.archived_at IS NULL
                 AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                      OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                                WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
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
                     JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
                     WHERE c.suspended=0 AND c.archived_at IS NULL AND s.archived_at IS NULL
                     AND f.archived_at IS NULL AND rs.due_at>=?1 AND rs.due_at<?2
                     AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                          OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                                    WHERE cd.card_id=c.id AND d.archived_at IS NULL))",
                    params![start, start + 86_400],
                )?,
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT f.title, COUNT(*),
                    AVG(CASE WHEN COALESCE(l.answer_correct, l.rating>1) THEN 1.0 ELSE 0.0 END) score
             FROM review_log l JOIN cards c ON c.id=l.card_id
              JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
              WHERE l.reviewed_at>=?1 AND c.archived_at IS NULL AND s.archived_at IS NULL
                AND f.archived_at IS NULL
                AND (NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                     OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                               WHERE cd.card_id=c.id AND d.archived_at IS NULL))
              GROUP BY f.id HAVING COUNT(*)>=2
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

    /// Free-text search over sections, ranked by BM25.
    ///
    /// An empty query returns every section, which is what the TUI's unfiltered list wants.
    /// A query with no searchable token in it at all (`"--"`, `"'''"`) returns nothing —
    /// deliberately not everything, and deliberately not an error.
    pub fn search(&self, query: &str) -> Result<Vec<SectionRow>> {
        if query.trim().is_empty() {
            return self.sections();
        }
        let Some(expression) = crate::search::expression(query) else {
            return Ok(Vec::new());
        };
        self.search_expression(&expression, Bm25Weights::default(), 100)
    }

    /// Search with an already-built FTS5 expression and explicit ranking weights.
    ///
    /// The primitive behind [`Database::search`], and what `yy` calls: it sweeps the
    /// weights from config against its retrieval benchmark, so they cannot be literals
    /// here. Build `expression` with [`crate::search::expression`] — passing user text
    /// straight in is the crash this API exists to prevent.
    ///
    /// `bm25()` returns a **negative** score where more negative is better, so `ORDER BY
    /// score ASC` is correct and is not a sign error.
    pub fn search_expression(
        &self,
        expression: &str,
        weights: Bm25Weights,
        limit: usize,
    ) -> Result<Vec<SectionRow>> {
        let [note_title, heading, heading_path, body, tags] = weights.as_array();
        let mut statement = self.connection.prepare_cached(
            "SELECT s.section_uid, f.title, s.heading, s.heading_path, s.body, f.path, s.start_line, f.status
             FROM section_search q
             JOIN sections s ON s.id=q.rowid JOIN files f ON f.id=s.file_id
             WHERE section_search MATCH ?1 AND s.archived_at IS NULL AND f.archived_at IS NULL
             ORDER BY bm25(section_search, ?2, ?3, ?4, ?5, ?6) LIMIT ?7",
        )?;
        Ok(statement
            .query_map(
                params![
                    expression,
                    note_title,
                    heading,
                    heading_path,
                    body,
                    tags,
                    limit as i64
                ],
                section_from_row,
            )?
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
             JOIN files source_file ON source_file.id=source.file_id
             LEFT JOIN files target_file ON target_file.id=target.file_id
             WHERE source.section_uid=?1 AND r.relation_type != 'ingoing'
               AND source.archived_at IS NULL
               AND source_file.archived_at IS NULL
               AND (target.id IS NULL OR (target.archived_at IS NULL
                                          AND target_file.archived_at IS NULL))
             UNION ALL
             SELECT r.relation_type, source.section_uid, source.heading, 1
             FROM relations r JOIN sections source ON source.id=r.source_section_id
             JOIN files source_file ON source_file.id=source.file_id
             JOIN sections target ON target.section_uid=r.target_section_uid
             JOIN files target_file ON target_file.id=target.file_id
             WHERE r.target_section_uid=?1 AND r.relation_type != 'ingoing'
               AND source.archived_at IS NULL
                AND source_file.archived_at IS NULL AND target.archived_at IS NULL
                AND target_file.archived_at IS NULL
             UNION ALL
             SELECT r.relation_type, source.section_uid, source.heading, 0
             FROM relations r JOIN sections source ON source.id=r.source_section_id
             JOIN files source_file ON source_file.id=source.file_id
             JOIN sections target ON target.section_uid=r.target_section_uid
             JOIN files target_file ON target_file.id=target.file_id
             WHERE r.target_section_uid=?1 AND r.relation_type = 'ingoing'
               AND source.archived_at IS NULL AND source_file.archived_at IS NULL
               AND target.archived_at IS NULL AND target_file.archived_at IS NULL
             UNION ALL
             SELECT r.relation_type, r.target_section_uid, target.heading, 1
             FROM relations r JOIN sections source ON source.id=r.source_section_id
             LEFT JOIN sections target ON target.section_uid=r.target_section_uid
             JOIN files source_file ON source_file.id=source.file_id
             LEFT JOIN files target_file ON target_file.id=target.file_id
             WHERE source.section_uid=?1 AND r.relation_type = 'ingoing'
               AND source.archived_at IS NULL AND source_file.archived_at IS NULL
               AND (target.id IS NULL OR (target.archived_at IS NULL
                                          AND target_file.archived_at IS NULL))",
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

    pub fn graph(&self) -> Result<GraphData> {
        let notes = {
            let mut statement = self.connection.prepare(
                "SELECT note_id, title, topic, path FROM files
                 WHERE archived_at IS NULL ORDER BY lower(title)",
            )?;
            statement
                .query_map([], |row| {
                    Ok(GraphNote {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        topic: row.get(2)?,
                        path: PathBuf::from(row.get::<_, String>(3)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let sections = {
            let mut statement = self.connection.prepare(
                "SELECT s.section_uid, f.note_id, s.heading, s.parent_uid,
                        s.heading_level, s.start_line
                 FROM sections s JOIN files f ON f.id=s.file_id
                 WHERE s.archived_at IS NULL AND f.archived_at IS NULL
                 ORDER BY lower(f.title), s.position",
            )?;
            statement
                .query_map([], |row| {
                    Ok(GraphSection {
                        uid: row.get(0)?,
                        note_id: row.get(1)?,
                        heading: row.get(2)?,
                        parent_uid: row.get(3)?,
                        level: row.get(4)?,
                        start_line: row.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let links = {
            let mut statement = self.connection.prepare(
                "SELECT CASE WHEN r.relation_type = 'ingoing'
                             THEN target.section_uid ELSE source.section_uid END,
                        CASE WHEN r.relation_type = 'ingoing'
                             THEN source.section_uid ELSE target.section_uid END,
                        r.relation_type
                 FROM relations r
                 JOIN sections source ON source.id=r.source_section_id
                 JOIN files source_file ON source_file.id=source.file_id
                 JOIN sections target ON target.section_uid=r.target_section_uid
                 JOIN files target_file ON target_file.id=target.file_id
                 WHERE source.archived_at IS NULL AND target.archived_at IS NULL
                   AND source_file.archived_at IS NULL AND target_file.archived_at IS NULL
                 ORDER BY source.section_uid, target.section_uid, r.relation_type",
            )?;
            statement
                .query_map([], |row| {
                    Ok(GraphLink {
                        source: row.get(0)?,
                        target: row.get(1)?,
                        relation_type: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(GraphData {
            notes,
            sections,
            links,
        })
    }

    pub fn due_cards(
        &self,
        new_cards_per_day: usize,
        max_reviews_per_day: usize,
        review_order: ReviewOrder,
        bury_siblings: bool,
    ) -> Result<Vec<ReviewCard>> {
        self.review_cards(
            ReviewScope::All,
            false,
            new_cards_per_day,
            max_reviews_per_day,
            review_order,
            bury_siblings,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn review_cards(
        &self,
        scope: ReviewScope,
        force: bool,
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
        let (scope_kind, deck_id) = match scope {
            ReviewScope::All => (0, -1),
            ReviewScope::Deckless => (1, -1),
            ReviewScope::Deck(deck_id) => (2, deck_id),
        };
        let mut statement = self.connection.prepare(
            "SELECT c.id, c.card_uid, c.section_uid, c.definition, rs.due_at,
                    rs.stability, rs.difficulty, rs.last_review_at, rs.review_count
             FROM cards c JOIN review_state rs ON rs.card_id=c.id
             JOIN sections s ON s.section_uid=c.section_uid JOIN files f ON f.id=s.file_id
             WHERE c.suspended=0 AND c.archived_at IS NULL AND s.archived_at IS NULL
               AND f.archived_at IS NULL AND (?4 OR rs.due_at <= ?1)
               AND ((?2=0 AND (
                        NOT EXISTS(SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id)
                        OR EXISTS(SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                                  WHERE cd.card_id=c.id AND d.archived_at IS NULL)))
                    OR (?2=1 AND NOT EXISTS(
                        SELECT 1 FROM card_decks cd WHERE cd.card_id=c.id))
                    OR (?2=2 AND EXISTS(
                        SELECT 1 FROM card_decks cd JOIN decks d ON d.id=cd.deck_id
                        WHERE cd.card_id=c.id AND cd.deck_id=?3 AND d.archived_at IS NULL)))
              ORDER BY rs.due_at, c.id",
        )?;
        let mut cards = statement
            .query_map(params![now, scope_kind, deck_id, force], |row| {
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
        if force {
            return Ok(cards);
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
        self.record_review_at(
            card,
            rating,
            correct,
            response_ms,
            submitted,
            desired_retention,
            Utc::now().timestamp(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_review_at(
        &mut self,
        card: &ReviewCard,
        rating: u32,
        correct: Option<bool>,
        response_ms: i64,
        submitted: Option<&str>,
        desired_retention: f32,
        reviewed_at: i64,
    ) -> Result<Option<u32>> {
        if !(1..=4).contains(&rating) {
            return Err(anyhow!("rating must be between 1 and 4"));
        }
        let now = reviewed_at;
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

    pub fn import_review_events(
        &mut self,
        events: &[ReviewEvent],
        desired_retention: f32,
    ) -> Result<usize> {
        let mut events = events.to_vec();
        events.sort_by(|left, right| {
            (left.reviewed_at, &left.event_id).cmp(&(right.reviewed_at, &right.event_id))
        });
        let mut imported = 0;
        for event in events {
            if !(1..=4).contains(&event.rating)
                || event.event_id.trim().is_empty()
                || event.device_id.trim().is_empty()
            {
                continue;
            }
            let seen = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM synced_review_events WHERE event_id=?1)",
                [&event.event_id],
                |row| row.get::<_, bool>(0),
            )?;
            if seen {
                continue;
            }
            let card = self
                .connection
                .query_row(
                    "SELECT c.id, c.card_uid, c.section_uid, c.definition, rs.due_at,
                    rs.stability, rs.difficulty, rs.last_review_at, rs.review_count
                 FROM cards c JOIN review_state rs ON rs.card_id=c.id WHERE c.card_uid=?1",
                    [&event.card_uid],
                    review_card_from_row,
                )
                .optional()?;
            let Some(card) = card else {
                continue;
            };
            if card
                .last_review_at
                .is_some_and(|last| last >= event.reviewed_at)
            {
                self.connection.execute(
                    "INSERT INTO review_log(card_id, card_uid, reviewed_at, rating,
                        answer_correct, response_ms, elapsed_days, scheduled_days,
                        stability_before, stability_after, difficulty_before, difficulty_after)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?7, ?8, ?8)",
                    params![
                        card.id,
                        card.uid,
                        event.reviewed_at,
                        event.rating,
                        event.answer_correct.map(i64::from),
                        event.response_ms.max(0),
                        card.stability,
                        card.difficulty,
                    ],
                )?;
                self.connection.execute(
                    "INSERT INTO synced_review_events(event_id, device_id, imported_at)
                     VALUES (?1, ?2, ?3)",
                    params![event.event_id, event.device_id, Utc::now().timestamp()],
                )?;
                imported += 1;
                continue;
            }
            if self
                .record_review_at(
                    &card,
                    event.rating,
                    event.answer_correct,
                    event.response_ms.max(0),
                    None,
                    desired_retention,
                    event.reviewed_at,
                )?
                .is_some()
            {
                self.connection.execute(
                    "INSERT INTO synced_review_events(event_id, device_id, imported_at)
                     VALUES (?1, ?2, ?3)",
                    params![event.event_id, event.device_id, Utc::now().timestamp()],
                )?;
                imported += 1;
            }
        }
        Ok(imported)
    }

    pub fn mobile_snapshot(
        &self,
        new_cards_per_day: usize,
        max_reviews_per_day: usize,
        review_order: ReviewOrder,
        bury_siblings: bool,
    ) -> Result<MobileSnapshot> {
        let generated_at = Utc::now().timestamp();
        let decks = self.decks()?;
        let rows = self.card_rows()?;
        let deck_ids = rows
            .into_iter()
            .map(|row| (row.id, row.decks))
            .collect::<std::collections::HashMap<_, _>>();
        let due_without_deck = self
            .review_cards(
                ReviewScope::Deckless,
                false,
                new_cards_per_day,
                max_reviews_per_day,
                review_order,
                bury_siblings,
            )?
            .into_iter()
            .map(|card| card.id)
            .collect::<HashSet<_>>();
        let mut due_deck_ids = std::collections::HashMap::<i64, Vec<i64>>::new();
        for deck in &decks {
            for card in self.review_cards(
                ReviewScope::Deck(deck.id),
                false,
                new_cards_per_day,
                max_reviews_per_day,
                review_order,
                bury_siblings,
            )? {
                due_deck_ids.entry(card.id).or_default().push(deck.id);
            }
        }
        let cards = self
            .review_cards(
                ReviewScope::All,
                true,
                new_cards_per_day,
                max_reviews_per_day,
                ReviewOrder::Due,
                false,
            )?
            .into_iter()
            .map(|card| MobileCard {
                due_without_deck: due_without_deck.contains(&card.id),
                due_deck_ids: due_deck_ids.remove(&card.id).unwrap_or_default(),
                deck_ids: deck_ids.get(&card.id).cloned().unwrap_or_default(),
                card,
            })
            .collect();
        Ok(MobileSnapshot {
            protocol_version: 2,
            generated_at,
            decks: decks
                .into_iter()
                .map(|deck| MobileDeck {
                    id: deck.id,
                    name: deck.name,
                })
                .collect(),
            cards,
            statistics: self.statistics()?,
        })
    }

    pub fn diagnostics(&self) -> Result<Vec<Diagnostic>> {
        let mut statement = self.connection.prepare(
            "SELECT d.path, d.line, d.message FROM diagnostics d
             WHERE NOT EXISTS(SELECT 1 FROM files f
                              WHERE f.path=d.path AND f.archived_at IS NOT NULL)
             ORDER BY d.path, d.line",
        )?;
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
        heading_path: row.get(3)?,
        body: row.get(4)?,
        path: PathBuf::from(row.get::<_, String>(5)?),
        start_line: row.get(6)?,
        status: row.get(7)?,
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

/// Bump when [`SEARCH_SCHEMA`] changes. Any bump rebuilds the index from `sections`,
/// which is cheap because the content is never stored in the FTS table to begin with.
const SEARCH_SCHEMA_VERSION: i64 = 1;

/// The full-text index: external content, kept in sync by triggers.
///
/// Two properties are worth stating because both were previously wrong.
///
/// **External content.** The FTS table stores only the inverted index and reads column
/// values back through the `section_content` view. Before this, `section_search` was a
/// standalone table storing a second copy of every section `body`, hand-synced from two
/// separate call sites — the arrangement where one code path eventually forgets and search
/// serves stale rows for weeks with nothing to notice it.
///
/// **`tokenchars '_-.'`.** Without it the tokenizer splits `calculate_pivot` into
/// `calculate` and `pivot`, and `sprite.rs` into `sprite` and `rs`. On a vault that
/// contains code and filenames that is a large precision loss for one tokenizer option.
///
/// The delete triggers are the subtle part. An FTS5 `'delete'` command has to be given the
/// *exact* values that were originally indexed, or the tokens it fails to match are left
/// behind pointing at a rowid that no longer exists. Two orderings threaten that, and each
/// has a trigger here holding it:
///
/// - **Deleting a note.** `ON DELETE CASCADE` removes the `files` row *before* the child
///   `sections` rows, so by the time the section delete trigger runs its subquery for the
///   title, the title is gone and the delete command supplies `NULL`. `files_bd` deletes
///   the sections first, while the title is still readable. Note that `integrity-check`
///   does **not** catch this — the residue is only visible by matching a title token.
/// - **Retitling a note.** `replace_note` updates `files` before `sections`, so a section
///   trigger firing afterwards would delete using the *new* title against an index built
///   with the old one. `section_search_files_au` re-indexes the whole note at the moment
///   the title changes, using `old.title` for the delete half.
const SEARCH_SCHEMA: &str = "
    CREATE VIEW section_content AS
      SELECT s.id AS rowid, f.title AS note_title, s.heading AS heading,
             s.heading_path AS heading_path, s.body AS body, f.tags AS tags
      FROM sections s JOIN files f ON f.id = s.file_id;

    CREATE VIRTUAL TABLE section_search USING fts5(
        note_title, heading, heading_path, body, tags,
        content='section_content', content_rowid='rowid',
        tokenize=\"unicode61 remove_diacritics 2 tokenchars '_-.'\"
    );

    CREATE TRIGGER files_bd BEFORE DELETE ON files BEGIN
      DELETE FROM sections WHERE file_id = old.id;
    END;

    CREATE TRIGGER section_search_ai AFTER INSERT ON sections BEGIN
      INSERT INTO section_search(rowid, note_title, heading, heading_path, body, tags)
      VALUES (new.id, (SELECT title FROM files WHERE id = new.file_id), new.heading,
              new.heading_path, new.body, (SELECT tags FROM files WHERE id = new.file_id));
    END;

    CREATE TRIGGER section_search_ad AFTER DELETE ON sections BEGIN
      INSERT INTO section_search(section_search, rowid, note_title, heading, heading_path,
                                 body, tags)
      VALUES ('delete', old.id, (SELECT title FROM files WHERE id = old.file_id),
              old.heading, old.heading_path, old.body,
              (SELECT tags FROM files WHERE id = old.file_id));
    END;

    CREATE TRIGGER section_search_au AFTER UPDATE ON sections BEGIN
      INSERT INTO section_search(section_search, rowid, note_title, heading, heading_path,
                                 body, tags)
      VALUES ('delete', old.id, (SELECT title FROM files WHERE id = old.file_id),
              old.heading, old.heading_path, old.body,
              (SELECT tags FROM files WHERE id = old.file_id));
      INSERT INTO section_search(rowid, note_title, heading, heading_path, body, tags)
      VALUES (new.id, (SELECT title FROM files WHERE id = new.file_id), new.heading,
              new.heading_path, new.body, (SELECT tags FROM files WHERE id = new.file_id));
    END;

    CREATE TRIGGER section_search_files_au AFTER UPDATE OF title, tags ON files
    WHEN old.title IS NOT new.title OR old.tags IS NOT new.tags BEGIN
      INSERT INTO section_search(section_search, rowid, note_title, heading, heading_path,
                                 body, tags)
        SELECT 'delete', s.id, old.title, s.heading, s.heading_path, s.body, old.tags
        FROM sections s WHERE s.file_id = old.id;
      INSERT INTO section_search(rowid, note_title, heading, heading_path, body, tags)
        SELECT s.id, new.title, s.heading, s.heading_path, s.body, new.tags
        FROM sections s WHERE s.file_id = new.id;
    END;
";

/// Create the search index, or rebuild it when its shape has changed.
///
/// Rebuilding rather than migrating in place is deliberate: the FTS table is derived data,
/// so the cheapest correct migration is always to throw it away and re-derive it.
fn migrate_search(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == SEARCH_SCHEMA_VERSION {
        return Ok(());
    }

    connection.execute_batch(
        "DROP TRIGGER IF EXISTS section_search_ai;
         DROP TRIGGER IF EXISTS section_search_ad;
         DROP TRIGGER IF EXISTS section_search_au;
         DROP TRIGGER IF EXISTS section_search_files_au;
         DROP TRIGGER IF EXISTS files_bd;
         DROP TABLE   IF EXISTS section_search;
         DROP VIEW    IF EXISTS section_content;",
    )?;
    connection.execute_batch(SEARCH_SCHEMA)?;

    // Re-derive from the content view in one statement. `rebuild` would do the same, but
    // spelling it out keeps this working if the table ever becomes contentless.
    connection.execute_batch(
        "INSERT INTO section_search(rowid, note_title, heading, heading_path, body, tags)
           SELECT rowid, note_title, heading, heading_path, body, tags FROM section_content;
         INSERT INTO section_search(section_search) VALUES('optimize');",
    )?;
    connection.execute_batch(&format!("PRAGMA user_version = {SEARCH_SCHEMA_VERSION}"))?;
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
            // `section_search_ad` removes the index entry.
            transaction.execute("DELETE FROM sections WHERE id=?1", [id])?;
        }
    }
    Ok(())
}

/// What the index currently holds, excluding archived rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub documents: usize,
    pub sections: usize,
    pub relations: usize,
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

    /// Ask FTS5 itself whether the index still matches its content, and separately look
    /// for orphaned tokens.
    ///
    /// The second half is not redundant. `integrity-check` compares the index against the
    /// content view, so it passes when a row is missing from *both* — which is exactly what
    /// a cascade delete used to produce: the section gone from the view, its title tokens
    /// still in the index pointing at a dead rowid. Matching a term and checking the rowid
    /// still resolves is the only thing that catches it.
    fn assert_search_index_is_consistent(db: &Database, orphan_probe: &[&str]) {
        db.connection
            .execute_batch("INSERT INTO section_search(section_search) VALUES('integrity-check')")
            .expect("FTS5 index disagrees with the content view");

        for term in orphan_probe {
            let expression = crate::search::expression(term).expect("probe term is searchable");
            let orphans: i64 = db
                .connection
                .query_row(
                    "SELECT count(*) FROM section_search q
                     LEFT JOIN sections s ON s.id = q.rowid
                     WHERE section_search MATCH ?1 AND s.id IS NULL",
                    [&expression],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(orphans, 0, "{term:?} still indexed against a deleted section");
        }
    }

    /// The search index survives the whole lifecycle of a note.
    ///
    /// Every step here is a case where a hand-synced or naively-triggered index goes stale
    /// silently and stays stale for weeks, because nothing in normal use reports it.
    #[test]
    fn the_search_index_stays_in_sync_across_edits_retitles_and_deletes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("obs.md");
        fs::write(
            &path,
            "---\nid: obs\ntitle: ObsTitle\ntags: [video]\n---\n\
             # Root {#root}\n## Cursor follow {#follow}\nSmoothtoken the crop.\n\
             ## Old approach {#old}\nJittertoken.\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["Smoothtoken"]);
        assert_eq!(db.search("Smoothtoken").unwrap().len(), 1);

        // Edit a body: the old term must stop matching and the new one must start.
        fs::write(
            &path,
            "---\nid: obs\ntitle: ObsTitle\ntags: [video]\n---\n\
             # Root {#root}\n## Cursor follow {#follow}\nReplacedtoken the crop.\n\
             ## Old approach {#old}\nJittertoken.\n",
        )
        .unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["Smoothtoken", "Replacedtoken"]);
        assert!(db.search("Smoothtoken").unwrap().is_empty(), "stale body");
        assert_eq!(db.search("Replacedtoken").unwrap().len(), 1);

        // Retitle. The title is indexed per section but stored on `files`, and the update
        // order means a naive trigger deletes using the new title against an old index.
        fs::write(
            &path,
            "---\nid: obs\ntitle: RetitledNote\ntags: [video]\n---\n\
             # Root {#root}\n## Cursor follow {#follow}\nReplacedtoken the crop.\n\
             ## Old approach {#old}\nJittertoken.\n",
        )
        .unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["ObsTitle", "RetitledNote"]);
        assert!(db.search("ObsTitle").unwrap().is_empty(), "stale title");
        assert_eq!(db.search("RetitledNote").unwrap().len(), 3);

        // Drop one section from the note.
        fs::write(
            &path,
            "---\nid: obs\ntitle: RetitledNote\ntags: [video]\n---\n\
             # Root {#root}\n## Cursor follow {#follow}\nReplacedtoken the crop.\n",
        )
        .unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["Jittertoken"]);
        assert!(db.search("Jittertoken").unwrap().is_empty());

        // Delete the note entirely. The `files` row goes first under `ON DELETE CASCADE`,
        // which is what leaves title tokens behind without the `files_bd` guard.
        fs::remove_file(&path).unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["RetitledNote", "Replacedtoken"]);
        assert!(db.search("RetitledNote").unwrap().is_empty());
    }

    /// Front-matter `status:` reaches the rows retrieval ranks.
    ///
    /// Without this the `[search.status_weight]` block in the config is inert, and a
    /// workflow marked `obsolete` ranks exactly as high as the note that replaced it.
    #[test]
    fn a_notes_status_travels_with_every_section_it_owns() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("old.md"),
            "---\nid: old\ntitle: Old\nstatus: obsolete\n---\n# Root {#root}\nSharedterm here.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("new.md"),
            "---\nid: new\ntitle: New\n---\n# Root {#root}\nSharedterm here too.\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        let hits = db.search("Sharedterm").unwrap();
        assert_eq!(hits.len(), 2);
        let old = hits.iter().find(|h| h.uid == "old#root").unwrap();
        let new = hits.iter().find(|h| h.uid == "new#root").unwrap();
        assert_eq!(old.status.as_deref(), Some("obsolete"));
        assert_eq!(new.status, None, "an unmarked note has no status, not a default");
    }

    /// Only genuinely unresolved disagreements are reported.
    ///
    /// A `contradicts::` edge is normal and healthy once one side is marked obsolete or
    /// superseded — that is the author having decided. Reporting those as problems would
    /// make the check noise, and a noisy check gets ignored.
    #[test]
    fn contradictions_are_reported_only_while_nothing_marks_the_winner() {
        let dir = tempdir().unwrap();
        let write = |name: &str, body: &str| fs::write(dir.path().join(name), body).unwrap();

        // Unresolved: both current.
        write(
            "a.md",
            "---\nid: a\ntitle: A\n---\n# A {#root}\ncontradicts:: [[b#root]]\nUse rsync.\n",
        );
        write("b.md", "---\nid: b\ntitle: B\n---\n# B {#root}\nUse restic.\n");

        // Resolved by status: the old one says so itself.
        write(
            "c.md",
            "---\nid: c\ntitle: C\nstatus: obsolete\n---\n# C {#root}\ncontradicts:: [[d#root]]\nOld.\n",
        );
        write("d.md", "---\nid: d\ntitle: D\n---\n# D {#root}\nNew.\n");

        // Resolved by a `supersedes` edge.
        write(
            "e.md",
            "---\nid: e\ntitle: E\n---\n# E {#root}\ncontradicts:: [[f#root]]\nsupersedes:: [[f#root]]\nReplacement.\n",
        );
        write("f.md", "---\nid: f\ntitle: F\n---\n# F {#root}\nReplaced.\n");

        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        let unresolved = db.unresolved_contradictions().unwrap();
        let uids: Vec<String> = unresolved
            .iter()
            .map(|pair| format!("{}|{}", pair.left_uid, pair.right_uid))
            .collect();

        assert_eq!(unresolved.len(), 1, "reported: {uids:?}");
        assert!(uids[0].contains("a#root") && uids[0].contains("b#root"));
    }

    #[test]
    fn a_mutual_contradiction_is_one_disagreement_not_two() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("a.md"),
            "---\nid: a\ntitle: A\n---\n# A {#root}\ncontradicts:: [[b#root]]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("b.md"),
            "---\nid: b\ntitle: B\n---\n# B {#root}\ncontradicts:: [[a#root]]\n",
        )
        .unwrap();

        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        assert_eq!(db.unresolved_contradictions().unwrap().len(), 1);
    }

    /// `tokenchars '_-.'` is one tokenizer option with a large effect on a vault holding
    /// code: without it these are two tokens each and precision collapses.
    #[test]
    fn identifiers_and_filenames_stay_single_tokens() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("code.md"),
            "---\nid: code\ntitle: Code\n---\n# Root {#root}\n\
             Call calculate_pivot from sprite.rs, not the cursor-follow helper.\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        for term in ["calculate_pivot", "sprite.rs", "cursor-follow"] {
            assert_eq!(db.search(term).unwrap().len(), 1, "{term} did not match");
        }
        // The point of the option: the halves are not independently searchable, so a query
        // for `pivot` does not drag in every section mentioning a pivot.
        assert!(db.search("pivot").unwrap().is_empty());
    }

    /// No user input can make FTS5 raise, checked against a real SQLite.
    ///
    /// A raise here is a crash on the interactive path, so string-level assertions about
    /// the escaping are not enough — the parser has to be the judge.
    #[test]
    fn no_query_can_make_the_search_index_raise() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("n.md"),
            "---\nid: n\ntitle: N\n---\n# Root {#root}\nBody.\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        for query in crate::search::tests::hostile_queries() {
            for mode in [crate::search::Mode::All, crate::search::Mode::Any] {
                let Some(expression) = crate::search::expression_with(&query, mode) else {
                    continue;
                };
                let result = db.search_expression(&expression, Bm25Weights::default(), 10);
                assert!(
                    result.is_ok(),
                    "{query:?} in {mode:?} built {expression:?}, which raised {:?}",
                    result.err()
                );
            }
        }
    }

    /// An existing vault indexed under the old standalone schema comes back searchable.
    #[test]
    fn reopening_rebuilds_the_index_when_the_search_schema_changes() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("n.md"),
            "---\nid: n\ntitle: N\n---\n# Root {#root}\nUniquetoken body.\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        drop(db);

        // Simulate a database written before this schema version: force the rebuild path
        // without reindexing any file.
        let connection = Connection::open(dir.path().join(".notes/index.sqlite")).unwrap();
        connection.execute_batch("PRAGMA user_version = 0").unwrap();
        drop(connection);

        let db = Database::open(dir.path()).unwrap();
        assert_search_index_is_consistent(&db, &["Uniquetoken"]);
        assert_eq!(
            db.search("Uniquetoken").unwrap().len(),
            1,
            "the index was not rebuilt on open"
        );
    }

    #[test]
    fn builds_graph_with_note_section_and_typed_relation_data() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("alpha.md"),
            "---\nid: alpha\ntitle: Alpha\ntopic: Systems\n---\n# Alpha {#root}\n\noutgoing:: [[beta#root]]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.md"),
            "---\nid: beta\ntitle: Beta\ntopic: Systems\n---\n# Beta {#root}\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        let graph = db.graph().unwrap();
        assert_eq!(graph.notes.len(), 2);
        assert_eq!(graph.sections.len(), 2);
        assert!(
            graph
                .notes
                .iter()
                .all(|note| note.topic.as_deref() == Some("Systems"))
        );
        assert!(graph.links.iter().any(|link| {
            link.source == "alpha#root"
                && link.target == "beta#root"
                && link.relation_type == "outgoing"
        }));
    }

    #[test]
    fn ingoing_relations_point_back_to_the_declaring_section() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("alpha.md"),
            "---\nid: alpha\ntitle: Alpha\n---\n# Alpha {#root}\n\ningoing:: [[beta#root]]\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.md"),
            "---\nid: beta\ntitle: Beta\n---\n# Beta {#root}\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        let graph = db.graph().unwrap();
        assert!(graph.links.iter().any(|link| {
            link.source == "beta#root"
                && link.target == "alpha#root"
                && link.relation_type == "ingoing"
        }));
        let alpha = db.relations("alpha#root").unwrap();
        assert!(alpha.iter().any(|relation| {
            relation.incoming
                && relation.target_uid == "beta#root"
                && relation.relation_type == "ingoing"
        }));
        let beta = db.relations("beta#root").unwrap();
        assert!(beta.iter().any(|relation| {
            !relation.incoming
                && relation.target_uid == "alpha#root"
                && relation.relation_type == "ingoing"
        }));
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

    #[test]
    fn imports_mobile_reviews_once() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("test.md"),
            "# Test {#root}\n\n```quiz\nid: q1\ntype: cloze\nprompt: '{{c1::answer}}'\n```\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        let card = db
            .due_cards(20, 200, ReviewOrder::Due, false)
            .unwrap()
            .remove(0);
        let event = ReviewEvent {
            event_id: "phone-1-event-1".into(),
            device_id: "phone-1".into(),
            card_uid: card.uid,
            reviewed_at: Utc::now().timestamp(),
            rating: 3,
            answer_correct: Some(true),
            response_ms: 500,
        };

        assert_eq!(
            db.import_review_events(std::slice::from_ref(&event), 0.9)
                .unwrap(),
            1
        );
        assert_eq!(db.import_review_events(&[event], 0.9).unwrap(), 0);
        assert_eq!(db.statistics().unwrap().reviewed_today, 1);
    }

    #[test]
    fn scopes_reviews_by_deck_and_can_force_non_due_cards() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("test.md"),
            "# Test {#root}\n\n```quiz\nid: assigned\ntype: cloze\nprompt: '{{c1::deck}}'\n```\n\n```quiz\nid: loose\ntype: cloze\nprompt: '{{c1::alone}}'\n```\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        db.create_deck("Exam").unwrap();
        let deck = db.decks().unwrap().remove(0);
        let rows = db.card_rows().unwrap();
        let assigned = rows
            .iter()
            .find(|card| card.label.ends_with("assigned"))
            .unwrap();
        db.toggle_card_deck(assigned.id, deck.id).unwrap();

        let deck_cards = db
            .review_cards(
                ReviewScope::Deck(deck.id),
                false,
                20,
                200,
                ReviewOrder::Due,
                false,
            )
            .unwrap();
        let deckless_cards = db
            .review_cards(
                ReviewScope::Deckless,
                false,
                20,
                200,
                ReviewOrder::Due,
                false,
            )
            .unwrap();
        assert_eq!(deck_cards.len(), 1);
        assert_eq!(deck_cards[0].id, assigned.id);
        assert_eq!(deckless_cards.len(), 1);
        assert_ne!(deckless_cards[0].id, assigned.id);

        db.record_review(&deck_cards[0], 3, Some(true), 100, None, 0.9)
            .unwrap();
        assert!(
            db.review_cards(
                ReviewScope::Deck(deck.id),
                false,
                20,
                200,
                ReviewOrder::Due,
                false,
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            db.review_cards(
                ReviewScope::Deck(deck.id),
                true,
                20,
                200,
                ReviewOrder::Due,
                false,
            )
            .unwrap()
            .len(),
            1
        );

        let snapshot = db
            .mobile_snapshot(20, 200, ReviewOrder::Due, false)
            .unwrap();
        assert_eq!(snapshot.protocol_version, 2);
        assert_eq!(snapshot.decks.len(), 1);
        let assigned = snapshot
            .cards
            .iter()
            .find(|card| card.card.id == assigned.id)
            .unwrap();
        assert_eq!(assigned.deck_ids, vec![deck.id]);
        assert!(assigned.due_deck_ids.is_empty());
        assert!(snapshot.cards.iter().any(|card| card.deck_ids.is_empty()));
    }

    #[test]
    fn clips_survive_the_round_trip_into_the_mobile_snapshot() {
        // The seam that matters: a clip written in a note has to reach the
        // phone through the index and the snapshot without being flattened.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("rust.md"),
            "---\nid: rust\ntitle: Rust\n---\n# Rust {#root}\n\n\
             @video https://youtu.be/dQw4w9WgXcQ 06:54  Chapter on borrowing\n\n\
             ```quiz\nid: q1\ntype: cloze\nprompt: A {{c1::mutable}} borrow is exclusive.\n```\n\n\
             ## Elision {#elide}\n\n\
             ```quiz\nid: q2\ntype: cloze\nprompt: Elision {{c1::infers}} lifetimes.\n\
             clip: https://youtu.be/ABCdefGHIjk 12:03-12:40  Elision rules\n```\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        let snapshot = db
            .mobile_snapshot(20, 200, ReviewOrder::Due, false)
            .unwrap();

        let clips_for = |uid: &str| -> crate::model::CardClips {
            let card = snapshot
                .cards
                .iter()
                .find(|card| card.card.uid.contains(uid))
                .unwrap_or_else(|| panic!("no card for {uid} in the snapshot"));
            match &card.card.content {
                CardContent::Cloze { clips, .. } => clips.clone(),
                other => panic!("expected a cloze card, got {other:?}"),
            }
        };

        // Inherited from the section's `@video`.
        let inherited = clips_for("q1").answer.expect("q1 should carry a clip");
        assert_eq!(inherited.video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(inherited.start, 414);

        // Explicit, with a range and a label.
        let explicit = clips_for("q2").answer.expect("q2 should carry a clip");
        assert_eq!(explicit.video_id.as_deref(), Some("ABCdefGHIjk"));
        assert_eq!(explicit.start, 723);
        assert_eq!(explicit.end, Some(760));
        assert_eq!(explicit.label.as_deref(), Some("Elision rules"));

        // And it has to survive the JSON the phone actually downloads.
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"ABCdefGHIjk\""), "clip missing from snapshot JSON");
        assert!(json.contains("Elision rules"));
    }

    #[test]
    fn archives_notes_sections_and_quizzes_across_reindexing() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("test.md"),
            "---\nid: test\ntitle: Test\n---\n# Root {#root}\n\n```quiz\nid: q1\ntype: cloze\nprompt: '{{c1::answer}}'\n```\n\n## Child {#child}\nBody\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();

        db.archive_section("test#child").unwrap();
        assert!(
            db.sections()
                .unwrap()
                .iter()
                .all(|row| row.uid != "test#child")
        );
        let child = db
            .archived_items()
            .unwrap()
            .into_iter()
            .find(|item| matches!(item, ArchivedItem::Section { uid, .. } if uid == "test#child"))
            .unwrap();
        db.restore(&child).unwrap();
        assert!(
            db.sections()
                .unwrap()
                .iter()
                .any(|row| row.uid == "test#child")
        );

        let card = db.card_rows().unwrap().remove(0);
        db.archive_quiz(card.id).unwrap();
        assert!(db.card_rows().unwrap().is_empty());
        assert_eq!(db.statistics().unwrap().card_count, 0);
        let quiz = db
            .archived_items()
            .unwrap()
            .into_iter()
            .find(|item| matches!(item, ArchivedItem::Quiz { quiz_id, .. } if quiz_id == "q1"))
            .unwrap();
        db.restore(&quiz).unwrap();
        assert_eq!(db.statistics().unwrap().card_count, 1);

        db.archive_note(Path::new("test.md")).unwrap();
        db.index_vault(dir.path()).unwrap();
        assert!(db.notes().unwrap().is_empty());
        assert!(db.sections().unwrap().is_empty());
        assert!(db.card_rows().unwrap().is_empty());
        assert_eq!(db.statistics().unwrap().note_count, 0);
        let note = db.archived_items().unwrap().remove(0);
        assert!(matches!(note, ArchivedItem::Note { .. }));
        db.restore(&note).unwrap();
        assert_eq!(db.notes().unwrap().len(), 1);
        assert_eq!(db.card_rows().unwrap().len(), 1);
    }

    #[test]
    fn archived_decks_hide_exclusive_quizzes_until_restored() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("test.md"),
            "# Test {#root}\n\n```quiz\nid: q1\ntype: cloze\nprompt: '{{c1::answer}}'\n```\n",
        )
        .unwrap();
        let mut db = Database::open(dir.path()).unwrap();
        db.index_vault(dir.path()).unwrap();
        db.create_deck("Old").unwrap();
        let deck = db.decks().unwrap().remove(0);
        let card = db.card_rows().unwrap().remove(0);
        db.toggle_card_deck(card.id, deck.id).unwrap();

        db.archive_deck(deck.id).unwrap();
        assert!(db.decks().unwrap().is_empty());
        assert!(db.card_rows().unwrap().is_empty());
        assert!(
            db.due_cards(20, 200, ReviewOrder::Due, false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.statistics().unwrap().card_count, 0);
        let archived = db.archived_items().unwrap();
        let deck = archived
            .iter()
            .find(|item| matches!(item, ArchivedItem::Deck { .. }))
            .unwrap();
        db.restore(deck).unwrap();
        assert_eq!(db.decks().unwrap().len(), 1);
        assert_eq!(db.card_rows().unwrap().len(), 1);
        assert_eq!(
            db.due_cards(20, 200, ReviewOrder::Due, false)
                .unwrap()
                .len(),
            1
        );
    }
}
