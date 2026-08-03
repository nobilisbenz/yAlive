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
    pub level: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub position: usize,
    pub body: String,
    pub relations: Vec<Relation>,
    pub cards: Vec<CardDefinition>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QuizDefinition {
    Cloze {
        id: String,
        prompt: String,
    },
    MultipleChoice {
        id: String,
        #[serde(default)]
        mode: ChoiceMode,
        question: String,
        answers: Vec<ChoiceAnswer>,
        explanation: Option<String>,
    },
    CodeGap {
        id: String,
        #[serde(default)]
        language: String,
        prompt: Option<String>,
        code: String,
        gaps: HashMap<String, GapDefinition>,
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
    },
    MultipleChoice {
        mode: ChoiceMode,
        question: String,
        answers: Vec<ChoiceAnswer>,
        explanation: Option<String>,
    },
    CodeGap {
        language: String,
        prompt: Option<String>,
        code: String,
        gaps: HashMap<String, GapDefinition>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionRow {
    pub uid: String,
    pub note_title: String,
    pub heading: String,
    pub body: String,
    pub path: PathBuf,
    pub start_line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationRow {
    pub relation_type: String,
    pub target_uid: String,
    pub target_heading: Option<String>,
    pub incoming: bool,
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
            template: "```quiz\nid: ${id}\ntype: cloze\nprompt: |\n  Write the question with {{c1::the answer::an optional hint}}.\n```",
        },
        CardCapability {
            card_type: "multiple-choice",
            label: "Multiple choice",
            template: "```quiz\nid: ${id}\ntype: multiple-choice\nmode: single\nquestion: |\n  Write the question here.\nanswers:\n  - text: Correct answer\n    correct: true\n  - text: Distractor\n    correct: false\nexplanation: |\n  Explain why the answer is correct.\n```",
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
            relation_type: "supports",
            prefix: "supports:: ",
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
            relation_type: "prerequisite",
            prefix: "prerequisite:: ",
        },
    ]
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Default)]
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

pub fn validate_quiz(quiz: &QuizDefinition) -> Vec<String> {
    let mut errors = Vec::new();
    if quiz.id().trim().is_empty() {
        errors.push("quiz ID must not be empty".into());
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
