//! Pure helpers shared by the state layer and the render layer.
//!
//! Nothing here touches `App`, so each function is testable on its own and the
//! render modules can use them without reaching back into application state.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use crate::model::{ArchivedItem, GapDefinition};

/// Shorten `value` to `width` columns, marking the cut with an ellipsis.
///
/// The old implementation of this spent three columns on `...`, and several
/// callers passed a width wider than the panel they drew into, so labels were
/// clipped mid-word by the terminal with no indication anything was missing —
/// `Sync now` showed as `Set repository firs`. One ellipsis character keeps a
/// column back for content and makes the truncation visible.
pub fn fit(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut out: String = value.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Pad `value` to exactly `width` columns, truncating when it does not fit.
pub fn pad(value: &str, width: usize) -> String {
    let value = fit(value, width);
    let used = value.chars().count();
    format!("{value}{}", " ".repeat(width.saturating_sub(used)))
}

pub fn short_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".into())
}
pub fn day_label(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.format("%a %d").to_string())
        .unwrap_or_else(|| "-------".into())
}
pub fn slugify(value: &str) -> String {
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
pub fn expand_home(value: &str) -> Result<PathBuf> {
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
pub fn target_without_fragment(target: &str) -> &str {
    target.split(['#', '?']).next().unwrap_or(target)
}
/// Turn a section body into something a terminal pane can show.
///
/// The leading ATX heading is dropped: every caller draws the heading as the
/// pane's own title, so keeping it in the body printed it twice — once styled
/// as a title and once as raw `# Heading {#anchor}` Markdown.
pub fn display_markdown(body: &str) -> String {
    let body = match body.strip_prefix('#') {
        Some(rest) => rest
            .split_once('\n')
            .map(|(_, tail)| tail.trim_start_matches('\n'))
            .unwrap_or(""),
        None => body,
    };
    let quiz = Regex::new(r"(?s)```quiz\s.*?```").unwrap();
    let images = Regex::new(r"!\[([^\]]*)\]\([^)]+\)").unwrap();
    let without_quizzes = quiz.replace_all(body, "[Quiz card]");
    images
        .replace_all(&without_quizzes, |capture: &regex::Captures<'_>| {
            format!("[Image: {}] (press i to open)", &capture[1])
        })
        .to_string()
}
pub fn canonical_answer(gap: &GapDefinition) -> String {
    gap.answer
        .clone()
        .or_else(|| {
            gap.answers
                .as_ref()
                .and_then(|answers| answers.first().cloned())
        })
        .unwrap_or_else(|| "<regex answer>".into())
}
pub fn matches_gap(submitted: &str, gap: &GapDefinition) -> bool {
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
pub fn find_orphan_images(vault: &Path) -> Result<Vec<PathBuf>> {
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
pub fn archived_item_label(item: &ArchivedItem) -> String {
    match item {
        ArchivedItem::Note { title, .. } => format!("note {title}"),
        ArchivedItem::Section { heading, .. } => format!("section {heading}"),
        ArchivedItem::Quiz { label, .. } => format!("quiz {label}"),
        ArchivedItem::Deck { name, .. } => format!("deck {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MatchOptions;

    #[test]
    fn fit_marks_where_it_cut() {
        assert_eq!(fit("Sync now", 20), "Sync now");
        assert_eq!(fit("Set a repository first", 10), "Set a rep…");
        assert_eq!(fit("abc", 0), "");
        assert_eq!(fit("abc", 1), "…");
    }

    /// Truncation used to be measured in bytes in places, which panics the
    /// moment a note title contains anything outside ASCII.
    #[test]
    fn fit_counts_characters_not_bytes() {
        assert_eq!(fit("émigré", 20), "émigré");
        assert_eq!(fit("日本語のノート", 3), "日本…");
    }

    #[test]
    fn pad_fills_to_exactly_the_requested_width() {
        assert_eq!(pad("ab", 5).chars().count(), 5);
        assert_eq!(pad("abcdefgh", 5).chars().count(), 5);
    }

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

    /// The pane title already carries the heading, so the body must not repeat
    /// it as raw Markdown.
    #[test]
    fn display_markdown_drops_the_leading_heading() {
        let rendered = display_markdown("# Borrowing {#borrow}\n\nA borrow is a reference.\n");
        assert!(!rendered.contains('#'));
        assert!(rendered.starts_with("A borrow"));
    }

    #[test]
    fn display_markdown_keeps_a_body_that_starts_with_prose() {
        let rendered = display_markdown("Just prose.\n\n## Later heading\n");
        assert!(rendered.starts_with("Just prose."));
        assert!(rendered.contains("## Later heading"));
    }

    #[test]
    fn slugify_produces_a_usable_file_stem() {
        assert_eq!(
            slugify("Rust Ownership & Borrowing"),
            "rust-ownership-borrowing"
        );
        assert_eq!(slugify("   "), "");
    }
}
