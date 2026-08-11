//! Turning what the user typed into something FTS5 will accept.
//!
//! `MATCH` does not take text, it takes an **expression language** — `AND`, `OR`, `NOT`,
//! `NEAR`, column filters, prefixes, phrases. Passing a raw query through means the first
//! question containing an apostrophe, a hyphen, or a stray quote is a *SQL error*, not a
//! bad result. That is a crash on the interactive path, and it happens on ordinary English
//! ("don't", "follow-up") rather than on anything exotic.
//!
//! The rule here is total: **nothing the user types is ever interpreted as syntax.** Every
//! term is emitted as a quoted phrase, so operators are matched literally, and the only
//! structure is what this module adds.
//!
//! This lives beside the schema rather than in `yy` because escaping and tokenizer are one
//! decision: what counts as a term is set by `tokenchars '_-.'` in [`crate::db`], and every
//! consumer — the TUI here, `brain-index` there — has to agree with it.

/// Longest query we will build an expression from.
///
/// Not a security boundary — it is the point past which a "query" is a paste accident, and
/// FTS5 expression size is bounded by `SQLITE_MAX_EXPR_DEPTH` anyway.
const MAX_TERMS: usize = 32;

/// `bm25()` column weights, in the column order of `section_search`.
///
/// Heading matches carry most of the retrieval signal in a notes corpus, which is why the
/// heading outweighs the body 8:1. These are defaults; `yy` sweeps them from config against
/// its benchmark, so nothing here should be treated as settled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25Weights {
    pub note_title: f64,
    pub heading: f64,
    pub heading_path: f64,
    pub body: f64,
    pub tags: f64,
}

impl Default for Bm25Weights {
    fn default() -> Self {
        Self {
            note_title: 3.0,
            heading: 8.0,
            heading_path: 4.0,
            body: 1.0,
            tags: 2.0,
        }
    }
}

impl Bm25Weights {
    /// In the column order `section_search` declares.
    pub fn as_array(&self) -> [f64; 5] {
        [
            self.note_title,
            self.heading,
            self.heading_path,
            self.body,
            self.tags,
        ]
    }
}

/// Words carrying no signal in a personal-notes corpus.
///
/// Only consulted in [`Mode::Any`]. They are dropped rather than down-weighted because
/// under `OR` a single common word matches most of the vault, and BM25 then ranks a
/// hundred irrelevant sections above the two that matter.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by", "can",
    "could", "did", "do", "does", "for", "from", "get", "got", "had", "has", "have", "here",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my", "not", "of", "on",
    "or", "over", "should", "so", "some", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "to", "under", "use", "using", "was", "were", "what",
    "when", "where", "which", "while", "why", "will", "with", "without", "would", "you",
    "your",
];

/// Is this word noise?
///
/// Compares against a **punctuation-trimmed** form, which is the part that is easy to get
/// wrong: a user typing `what's the -j flag for?` produces the raw words `for?` and `the`,
/// and testing the raw word means `for?` sails through as content. Every term arriving here
/// still carries whatever punctuation the user typed.
pub fn is_stopword(word: &str) -> bool {
    let bare = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    STOPWORDS.contains(&bare.as_str())
}

/// How the terms of a query combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every term must match. The right default for an interactive filter list, where the
    /// user is narrowing a visible set and expects each keystroke to remove rows.
    #[default]
    All,
    /// Any term may match, with stopwords dropped first.
    ///
    /// The right mode for **retrieval**, where the query is a natural question rather than
    /// a filter. `how did I mirror bones in Blender?` has no section containing every one
    /// of those words, so `All` returns nothing at exactly the moment the user asked a real
    /// question. BM25 ranking, not the boolean, is what separates the results — and the
    /// seed set feeds graph expansion, so recall here matters more than precision.
    Any,
}

/// Build an FTS5 `MATCH` expression, or `None` if there is nothing to search for.
///
/// `None` is a real answer and callers must handle it: a query of `"--"` or `"'''"`
/// contains no searchable token, and the correct response is an empty result set, not an
/// error and not every section in the vault.
///
/// The last term gets a `*` so search-as-you-type matches a word being typed. Earlier terms
/// do not — by the time you have typed a space, you meant that word.
pub fn expression(query: &str) -> Option<String> {
    expression_with(query, Mode::All)
}

/// [`expression`], choosing how the terms combine. See [`Mode`].
pub fn expression_with(query: &str, mode: Mode) -> Option<String> {
    let mut terms: Vec<String> = query
        .split_whitespace()
        .filter_map(sanitise)
        .take(MAX_TERMS)
        .collect();

    if mode == Mode::Any {
        let kept: Vec<String> = terms
            .iter()
            .filter(|term| !is_stopword(term))
            .cloned()
            .collect();
        // A query that is *nothing but* stopwords ("how do I") still has to search for
        // something, or typing a question word by word blanks the results mid-sentence.
        if !kept.is_empty() {
            terms = kept;
        }
    }

    if terms.is_empty() {
        return None;
    }

    let joiner = match mode {
        Mode::All => " AND ",
        Mode::Any => " OR ",
    };

    let last = terms.len() - 1;
    let expression = terms
        .iter()
        .enumerate()
        .map(|(index, term)| {
            if index == last {
                // `"term"*` is a prefix query. The `*` sits *outside* the quotes, which is
                // what makes it an operator rather than a literal asterisk.
                format!("\"{term}\"*")
            } else {
                format!("\"{term}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(joiner);

    Some(expression)
}

/// Reduce one whitespace-separated word to something safe to put inside quotes.
///
/// A double quote is the only character that can escape a quoted phrase, and FTS5 escapes
/// it by doubling. Everything else is inert once quoted — including `*`, which is why a
/// user typing `foo*` searches for a literal asterisk rather than getting a prefix query
/// they did not ask for.
///
/// Returns `None` for a word with no searchable content, so it does not become an empty
/// phrase — `""*` is a syntax error.
fn sanitise(word: &str) -> Option<String> {
    let escaped: String = word.replace('"', "\"\"");

    // A phrase that tokenizes to nothing matches nothing, and with a `*` on the end it is
    // an error. Require at least one character the tokenizer will keep.
    escaped
        .chars()
        .any(|character| character.is_alphanumeric())
        .then_some(escaped)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn ordinary_words_become_a_conjunction_with_a_prefix_on_the_last() {
        assert_eq!(
            expression("cursor follow smoothing").as_deref(),
            Some(r#""cursor" AND "follow" AND "smoothing"*"#)
        );
    }

    #[test]
    fn a_single_word_is_a_prefix_query() {
        assert_eq!(expression("pico").as_deref(), Some(r#""pico"*"#));
    }

    #[test]
    fn fts5_operators_are_matched_literally() {
        // Without quoting, every one of these changes the meaning of the query, and `NEAR`
        // and the unbalanced paren are outright syntax errors.
        let expression = expression("AND OR NOT NEAR (x)").unwrap();
        assert_eq!(
            expression,
            r#""AND" AND "OR" AND "NOT" AND "NEAR" AND "(x)"*"#
        );
    }

    #[test]
    fn quotes_are_doubled_so_a_phrase_cannot_be_escaped() {
        // The attack shape: close the phrase, inject an operator, reopen. Doubling turns
        // the closing quote into a literal one.
        let expression = expression(r#"a" OR "b"#).unwrap();
        assert_eq!(expression, r#""a""" AND "OR" AND """b"*"#);
        assert_eq!(
            expression.matches('"').count() % 2,
            0,
            "quotes must stay balanced: {expression}"
        );
    }

    #[test]
    fn punctuation_only_queries_are_none_rather_than_an_error() {
        // Each of these used to reach SQLite as an expression and throw.
        for query in ["", "   ", "-", "--", "''", "\"", "***", "()"] {
            assert_eq!(expression(query), None, "{query:?} should not be searchable");
        }
    }

    #[test]
    fn punctuation_around_a_real_word_is_kept_but_inert() {
        // `calculate_pivot` and `sprite.rs` must survive as single terms — that is what the
        // `tokenchars '_-.'` tokenizer option exists for, and quoting must not interfere
        // with it.
        assert_eq!(
            expression("calculate_pivot").as_deref(),
            Some(r#""calculate_pivot"*"#)
        );
        assert_eq!(expression("sprite.rs").as_deref(), Some(r#""sprite.rs"*"#));
    }

    #[test]
    fn retrieval_mode_drops_stopwords_and_joins_with_or() {
        // `All` finds nothing here: no single section contains every one of these words.
        // That is the mode returning nothing precisely when a real question is asked.
        assert_eq!(
            expression_with("how did I mirror bones in Blender?", Mode::Any).as_deref(),
            Some(r#""mirror" OR "bones" OR "Blender?"*"#)
        );
    }

    #[test]
    fn a_stopword_carrying_punctuation_is_still_a_stopword() {
        // `what's the -j flag for?` produces the raw words `for?` and `the`. Testing the
        // raw word lets `for?` through as content, and it then earns a heading-match boost
        // on every note whose heading contains "for".
        assert!(is_stopword("for?"));
        assert!(is_stopword("The"));
        assert!(is_stopword("(with)"));
        assert!(!is_stopword("wireguard"));
        assert!(!is_stopword("-j"));

        assert_eq!(
            expression_with("what's the -j flag for?", Mode::Any).as_deref(),
            Some(r#""what's" OR "-j" OR "flag"*"#)
        );
    }

    #[test]
    fn a_query_of_only_stopwords_still_searches_for_something() {
        // Typing a question word by word passes through this state. Dropping every term
        // would blank the results mid-sentence and then repopulate them, which reads as a
        // flicker bug rather than as search working.
        assert_eq!(
            expression_with("how do i", Mode::Any).as_deref(),
            Some(r#""how" OR "do" OR "i"*"#)
        );
    }

    #[test]
    fn filter_mode_keeps_every_term_including_common_ones() {
        assert_eq!(
            expression_with("the rust book", Mode::All).as_deref(),
            Some(r#""the" AND "rust" AND "book"*"#)
        );
    }

    #[test]
    fn very_long_queries_are_truncated_rather_than_refused() {
        let query = (0..500)
            .map(|n| format!("term{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let expression = expression(&query).unwrap();
        assert_eq!(expression.matches(" AND ").count(), MAX_TERMS - 1);
    }

    /// Every generated expression keeps its quotes balanced.
    ///
    /// This is a necessary condition, not a sufficient one — a balanced expression can
    /// still be rejected by FTS5 — so the authoritative version of this test runs the same
    /// alphabet through a real SQLite in `db.rs`. This one exists because it fails with a
    /// readable message when the escaping is what broke.
    #[test]
    fn no_input_produces_unbalanced_quotes() {
        for query in hostile_queries() {
            for mode in [Mode::All, Mode::Any] {
                let Some(expression) = expression_with(&query, mode) else {
                    continue;
                };
                assert_eq!(
                    expression.matches('"').count() % 2,
                    0,
                    "unbalanced quotes from {query:?} in {mode:?}: {expression}"
                );
            }
        }
    }

    /// Every three-character string over an alphabet of FTS5 metacharacters.
    ///
    /// Shared with the database test so both check the same inputs. Three characters is
    /// enough to build `a"b`, `"""`, `* -`, and the other shapes that break naive escaping;
    /// longer strings add combinations, not new failure modes.
    pub fn hostile_queries() -> Vec<String> {
        let alphabet = ['"', '\'', '*', '-', '(', ')', 'a', ' ', ':', '^', 'ö'];
        let mut queries = Vec::with_capacity(alphabet.len().pow(3));
        for a in alphabet {
            for b in alphabet {
                for c in alphabet {
                    queries.push([a, b, c].iter().collect());
                }
            }
        }
        queries
    }
}
