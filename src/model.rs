use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct ParsedNote {
    pub path: PathBuf,
    pub note_id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub topic: Option<String>,
    pub pinned: bool,
    /// Front-matter `status:` — `current` | `draft` | `archived` | `obsolete`.
    pub status: Option<String>,
    pub created_at: i64,
    pub content_hash: String,
    pub modified_at: i64,
    pub sections: Vec<ParsedSection>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct ParsedSection {
    pub uid: String,
    pub parent_uid: Option<String>,
    pub heading: String,
    /// This heading and its ancestors, joined by
    /// [`crate::parser::HEADING_PATH_SEPARATOR`].
    pub heading_path: String,
    pub level: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub position: usize,
    pub body: String,
    pub relations: Vec<Relation>,
    pub cards: Vec<CardDefinition>,
    pub actions: Vec<ParsedAction>,
}

/// An `@action` line: a jump the author declared on a section.
///
/// Parsed here, in trusted code, from what the author wrote. **A language model never
/// contributes one** (spec §3.3, §48) — it may mention an action in prose, but the buttons
/// come from these rows. That separation is the whole security model for actions, and it is
/// why this type carries a target rather than a command: there is no representable way to
/// say "run this".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedAction {
    /// `file` | `video` | `app` | `project` | `url`.
    pub kind: String,
    /// Path, URL, or desktop id — already `~`-expanded and made absolute for paths.
    pub target: String,
    /// From `@file PATH:LINE`.
    pub line: Option<u32>,
    /// From `@video URL HH:MM:SS`, or lifted out of a URL that already carried `t=`.
    pub timestamp_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Relation {
    pub target_uid: String,
    pub relation_type: String,
    pub context: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub line: usize,
    pub message: String,
}

/// A moment in a video attached to a card.
///
/// Written as `clip: URL 06:54-07:20` in a quiz block, or inherited from the
/// section's own `@video` line. Carried as a structured field rather than as
/// markup so the renderer builds the embed itself — a card must never be able
/// to inject an iframe into the reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClipRef {
    /// The canonical watch URL, with any `t=` stripped out into `start`.
    pub url: String,
    /// The YouTube id, when the URL is a YouTube one. Lets a renderer build an
    /// embed without re-parsing.
    pub video_id: Option<String>,
    pub start: u64,
    /// Absent means "play to the end".
    pub end: Option<u64>,
    pub label: Option<String>,
}

/// Where a clip sits on a card, which decides what the card actually tests.
///
/// A clip shown beside the question is the stimulus — "you just watched this,
/// what happens next?". A clip shown on reveal is the evidence — "here is where
/// it was explained". Keeping them apart protects the recall: a clip rendered
/// with the prompt when it belongs to the answer lets you read the answer off
/// the video before rating yourself, and the interval that follows is a lie.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CardClips {
    /// Shown with the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<ClipRef>,
    /// Shown only after the answer is revealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<ClipRef>,
}

impl CardClips {
    pub fn is_empty(&self) -> bool {
        self.prompt.is_none() && self.answer.is_none()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QuizDefinition {
    Cloze {
        id: String,
        prompt: String,
        #[serde(default)]
        clip: Option<String>,
        #[serde(default)]
        prompt_clip: Option<String>,
    },
    MultipleChoice {
        id: String,
        #[serde(default)]
        mode: ChoiceMode,
        question: String,
        answers: Vec<ChoiceAnswer>,
        explanation: Option<String>,
        #[serde(default)]
        clip: Option<String>,
        #[serde(default)]
        prompt_clip: Option<String>,
    },
    CodeGap {
        id: String,
        #[serde(default)]
        language: String,
        prompt: Option<String>,
        code: String,
        gaps: HashMap<String, GapDefinition>,
        #[serde(default)]
        clip: Option<String>,
        #[serde(default)]
        prompt_clip: Option<String>,
    },
}

impl QuizDefinition {
    pub fn id(&self) -> &str {
        match self {
            Self::Cloze { id, .. } | Self::MultipleChoice { id, .. } | Self::CodeGap { id, .. } => {
                id
            }
        }
    }

    /// The raw `clip:` / `prompt_clip:` strings, before parsing.
    pub fn clip_sources(&self) -> (Option<&str>, Option<&str>) {
        let (answer, prompt) = match self {
            Self::Cloze { clip, prompt_clip, .. }
            | Self::MultipleChoice { clip, prompt_clip, .. }
            | Self::CodeGap { clip, prompt_clip, .. } => (clip, prompt_clip),
        };
        (answer.as_deref(), prompt.as_deref())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceMode {
    #[default]
    Single,
    Multiple,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChoiceAnswer {
    pub id: Option<String>,
    pub text: String,
    pub correct: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapDefinition {
    pub answer: Option<String>,
    pub answers: Option<Vec<String>>,
    pub regex: Option<String>,
    #[serde(default)]
    pub r#match: MatchOptions,
}

impl<'de> Deserialize<'de> for GapDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum GapSyntax {
            Answer(String),
            Answers(Vec<String>),
            Detailed {
                answer: Option<String>,
                answers: Option<Vec<String>>,
                regex: Option<String>,
                #[serde(default)]
                r#match: MatchOptions,
            },
        }

        Ok(match GapSyntax::deserialize(deserializer)? {
            GapSyntax::Answer(answer) => Self {
                answer: Some(answer),
                answers: None,
                regex: None,
                r#match: MatchOptions::default(),
            },
            GapSyntax::Answers(answers) => Self {
                answer: None,
                answers: Some(answers),
                regex: None,
                r#match: MatchOptions::default(),
            },
            GapSyntax::Detailed {
                answer,
                answers,
                regex,
                r#match,
            } => Self {
                answer,
                answers,
                regex,
                r#match,
            },
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatchOptions {
    #[serde(default = "yes")]
    pub trim: bool,
    #[serde(default)]
    pub normalize_whitespace: bool,
    #[serde(default = "yes")]
    pub case_sensitive: bool,
}

fn yes() -> bool {
    true
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            trim: true,
            normalize_whitespace: false,
            case_sensitive: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardDefinition {
    pub uid: String,
    pub section_uid: String,
    pub quiz_id: String,
    pub card_type: String,
    pub variant_key: String,
    pub content_hash: String,
    pub content: CardContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CardContent {
    Section {
        title: String,
        body: String,
    },
    Cloze {
        prompt: String,
        cloze: u32,
        /// Defaulted so cards indexed before clips existed still deserialize.
        #[serde(default, skip_serializing_if = "CardClips::is_empty")]
        clips: CardClips,
    },
    MultipleChoice {
        mode: ChoiceMode,
        question: String,
        answers: Vec<ChoiceAnswer>,
        explanation: Option<String>,
        #[serde(default, skip_serializing_if = "CardClips::is_empty")]
        clips: CardClips,
    },
    CodeGap {
        language: String,
        prompt: Option<String>,
        code: String,
        gaps: HashMap<String, GapDefinition>,
        #[serde(default, skip_serializing_if = "CardClips::is_empty")]
        clips: CardClips,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionRow {
    pub uid: String,
    pub note_title: String,
    pub heading: String,
    /// This heading and its ancestors — `OBS > Cursor follow > Smoothing`.
    pub heading_path: String,
    pub body: String,
    pub path: PathBuf,
    pub start_line: usize,
    /// The owning note's front-matter `status:`, for ranking (spec §47).
    pub status: Option<String>,
}

/// Two sections that disagree, with nothing marking which one won.
#[derive(Debug, Clone, Serialize)]
pub struct ContradictionPair {
    pub left_uid: String,
    pub left_heading: String,
    pub left_path: PathBuf,
    pub right_uid: String,
    pub right_heading: String,
    pub right_path: PathBuf,
}

/// An `@action` row as retrieval reads it back.
#[derive(Debug, Clone, Serialize)]
pub struct ActionRow {
    pub section_uid: String,
    pub kind: String,
    pub target: String,
    pub line: Option<u32>,
    pub timestamp_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationRow {
    pub relation_type: String,
    pub target_uid: String,
    pub target_heading: Option<String>,
    pub incoming: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNote {
    pub id: String,
    pub title: String,
    pub topic: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSection {
    pub uid: String,
    pub note_id: String,
    pub heading: String,
    pub parent_uid: Option<String>,
    pub level: u32,
    pub start_line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphLink {
    pub source: String,
    pub target: String,
    pub relation_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphData {
    pub notes: Vec<GraphNote>,
    pub sections: Vec<GraphSection>,
    pub links: Vec<GraphLink>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CardCapability {
    pub card_type: &'static str,
    pub label: &'static str,
    pub template: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationCapability {
    pub relation_type: &'static str,
    pub prefix: &'static str,
}

pub fn card_capabilities() -> Vec<CardCapability> {
    vec![
        CardCapability {
            card_type: "cloze",
            label: "Cloze",
            template: "```quiz\nid: ${id}\ntype: cloze\nprompt: |\n  Write the question with {{c1::the answer::an optional hint}}.\n# clip: URL 06:54-07:20  Optional label — shown on reveal.\n```",
        },
        CardCapability {
            card_type: "multiple-choice",
            label: "Multiple choice",
            template: "```quiz\nid: ${id}\ntype: multiple-choice\nmode: single\nquestion: |\n  Write the question here.\nanswers:\n  - text: Correct answer\n    correct: true\n  - text: Distractor\n    correct: false\nexplanation: |\n  Explain why the answer is correct.\n# clip: URL 06:54-07:20  Optional label — shown on reveal.\n```",
        },
        CardCapability {
            card_type: "code-gap",
            label: "Code gap",
            template: "```quiz\nid: ${id}\ntype: code-gap\nlanguage: text\nprompt: |\n  Complete the code.\ncode: |\n  value = {{gap:answer}}\ngaps:\n  answer: replacement\n```",
        },
    ]
}

pub fn relation_capabilities() -> Vec<RelationCapability> {
    vec![
        RelationCapability {
            relation_type: "related",
            prefix: "",
        },
        RelationCapability {
            relation_type: "outgoing",
            prefix: "outgoing:: ",
        },
        RelationCapability {
            relation_type: "contradicts",
            prefix: "contradicts:: ",
        },
        RelationCapability {
            relation_type: "example-of",
            prefix: "example-of:: ",
        },
        RelationCapability {
            relation_type: "ingoing",
            prefix: "ingoing:: ",
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCard {
    pub id: i64,
    pub uid: String,
    pub section_uid: String,
    pub content: CardContent,
    pub due_at: i64,
    pub stability: Option<f32>,
    pub difficulty: Option<f32>,
    pub last_review_at: Option<i64>,
    pub review_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewScope {
    All,
    Deck(i64),
    Deckless,
}

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub title: String,
    pub topic: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
    pub modified_at: i64,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ReviewSectionRow {
    pub uid: String,
    pub note_title: String,
    pub heading: String,
    pub enrolled: bool,
}

#[derive(Debug, Clone)]
pub struct DeckRow {
    pub id: i64,
    pub name: String,
    pub card_count: usize,
}

#[derive(Debug, Clone)]
pub struct CardRow {
    pub id: i64,
    pub label: String,
    pub decks: Vec<i64>,
    pub section_uid: String,
    pub card_type: String,
}

#[derive(Debug, Clone)]
pub enum ArchivedItem {
    Note {
        note_id: String,
        title: String,
        path: PathBuf,
        section_count: usize,
        quiz_count: usize,
    },
    Section {
        uid: String,
        note_title: String,
        heading: String,
        path: PathBuf,
        start_line: usize,
        quiz_count: usize,
    },
    Quiz {
        section_uid: String,
        quiz_id: String,
        label: String,
        card_count: usize,
    },
    Deck {
        id: i64,
        name: String,
        quiz_count: usize,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Statistics {
    pub note_count: usize,
    pub topic_count: usize,
    pub untopiced_count: usize,
    pub card_count: usize,
    pub due_now: usize,
    pub reviewed_today: usize,
    pub reviewed_week: usize,
    pub accuracy_week: Option<f64>,
    pub accuracy_month: Option<f64>,
    pub average_response_ms: Option<i64>,
    pub streak_days: usize,
    pub rating_counts: [usize; 4],
    pub daily_reviews: Vec<(i64, usize)>,
    pub due_forecast: Vec<(i64, usize)>,
    pub weak_notes: Vec<(String, usize, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewEvent {
    pub event_id: String,
    pub device_id: String,
    pub card_uid: String,
    pub reviewed_at: i64,
    pub rating: u32,
    pub answer_correct: Option<bool>,
    pub response_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileDeck {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileCard {
    #[serde(flatten)]
    pub card: ReviewCard,
    pub deck_ids: Vec<i64>,
    pub due_deck_ids: Vec<i64>,
    pub due_without_deck: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileSnapshot {
    pub protocol_version: u32,
    pub generated_at: i64,
    pub decks: Vec<MobileDeck>,
    pub cards: Vec<MobileCard>,
    pub statistics: Statistics,
}

pub fn validate_quiz(quiz: &QuizDefinition) -> Vec<String> {
    let mut errors = Vec::new();
    if quiz.id().trim().is_empty() {
        errors.push("quiz ID must not be empty".into());
    }

    // A clip is only useful if it names a moment that can actually be played.
    let (answer_clip, prompt_clip) = quiz.clip_sources();
    for (field, raw) in [("clip", answer_clip), ("prompt_clip", prompt_clip)] {
        let Some(raw) = raw else { continue };
        match crate::parser::parse_clip(raw) {
            None => errors.push(format!("`{field}` is empty")),
            Some(clip) => {
                if !clip.url.starts_with("http://") && !clip.url.starts_with("https://") {
                    errors.push(format!("`{field}` must start with a URL"));
                }
                if let Some(end) = clip.end
                    && end <= clip.start
                {
                    errors.push(format!(
                        "`{field}` ends at {end}s, which is not after its start of {}s",
                        clip.start
                    ));
                }
            }
        }
    }

    match quiz {
        QuizDefinition::Cloze { prompt, .. } => {
            let marker = regex::Regex::new(r"\{\{c(\d+)::([^}:]+)(?:::[^}]+)?\}\}").unwrap();
            if !marker.is_match(prompt) {
                errors.push("cloze prompt has no valid {{cN::answer}} marker".into());
            }
        }
        QuizDefinition::MultipleChoice { mode, answers, .. } => {
            if answers.len() < 2 {
                errors.push("multiple-choice quiz needs at least two answers".into());
            }
            let correct = answers.iter().filter(|answer| answer.correct).count();
            if correct == 0 {
                errors.push("multiple-choice quiz has no correct answer".into());
            }
            if *mode == ChoiceMode::Single && correct != 1 {
                errors.push("single-choice quiz must have exactly one correct answer".into());
            }
            let ids = answers.iter().filter_map(|answer| answer.id.as_ref());
            let mut seen = HashSet::new();
            if !ids.into_iter().all(|id| seen.insert(id)) {
                errors.push("answer IDs must be unique".into());
            }
        }
        QuizDefinition::CodeGap { code, gaps, .. } => {
            let placeholder = regex::Regex::new(r"\{\{gap:([a-zA-Z0-9_-]+)\}\}").unwrap();
            let used: HashSet<_> = placeholder
                .captures_iter(code)
                .map(|capture| capture[1].to_string())
                .collect();
            for name in &used {
                if !gaps.contains_key(name) {
                    errors.push(format!("placeholder `{name}` has no gap definition"));
                }
            }
            for name in gaps.keys() {
                if !used.contains(name) {
                    errors.push(format!("gap definition `{name}` is unused"));
                }
            }
            for (name, gap) in gaps {
                let has_matcher = gap.answer.is_some()
                    || gap
                        .answers
                        .as_ref()
                        .is_some_and(|answers| !answers.is_empty())
                    || gap.regex.is_some();
                if !has_matcher {
                    errors.push(format!("gap `{name}` has no answer or regex"));
                }
                if let Some(pattern) = &gap.regex
                    && regex::Regex::new(pattern).is_err()
                {
                    errors.push(format!("gap `{name}` has an invalid regex"));
                }
            }
        }
    }
    errors
}
