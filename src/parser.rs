use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Deserialize;

use crate::model::{
    CardContent, CardDefinition, Diagnostic, ParsedNote, ParsedSection, QuizDefinition, Relation,
    validate_quiz,
};

#[derive(Default, Deserialize)]
struct FrontMatter {
    id: Option<String>,
    title: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    topic: Option<String>,
    #[serde(default, alias = "pin")]
    pinned: bool,
    /// `current` (the default) | `draft` | `archived` | `obsolete`.
    ///
    /// A ranking signal rather than a display one: a vault that accumulates years of
    /// how-tos needs superseded workflows to sink, or the answer blends the way you do
    /// something now with the way you stopped doing it in 2023.
    status: Option<String>,
}

/// What joins ancestor headings in `ParsedSection::heading_path`.
///
/// Shown to the user under every retrieved source, so it is a display decision as much as
/// a storage one — and it must not be a character that appears in headings, or the path
/// cannot be split back apart.
pub const HEADING_PATH_SEPARATOR: &str = " > ";

struct HeadingDraft {
    heading: String,
    id: Option<String>,
    level: u32,
    start: usize,
    line: usize,
}

pub fn parse_note(path: &Path, vault: &Path) -> Result<ParsedNote> {
    let source = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let metadata = fs::metadata(path)?;
    let modified_at = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let created_at = metadata
        .created()
        .unwrap_or_else(|_| metadata.modified().unwrap())
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;
    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let (front, markdown_offset) = parse_front_matter(&source)?;
    let relative = path.strip_prefix(vault).unwrap_or(path);
    let fallback_id = relative
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "-");
    let note_id = front.id.unwrap_or(fallback_id);
    let mut diagnostics = Vec::new();
    let mut headings = collect_headings(&source, markdown_offset);

    if headings.is_empty() {
        headings.push(HeadingDraft {
            heading: front.title.clone().unwrap_or_else(|| note_id.clone()),
            id: Some("root".into()),
            level: 1,
            start: markdown_offset,
            line: source[..markdown_offset].lines().count().max(1),
        });
    }

    let title = front
        .title
        .or_else(|| headings.first().map(|heading| heading.heading.clone()))
        .unwrap_or_else(|| note_id.clone());
    let mut seen_section_ids = HashSet::new();
    let mut section_ids = Vec::new();
    for heading in &headings {
        let id = heading
            .id
            .clone()
            .unwrap_or_else(|| slugify(&heading.heading));
        if !seen_section_ids.insert(id.clone()) {
            diagnostics.push(Diagnostic {
                path: relative.to_path_buf(),
                line: heading.line,
                message: format!("duplicate section ID `{id}`"),
            });
        }
        section_ids.push(id);
    }

    let quiz_blocks = collect_quiz_blocks(&source, markdown_offset, relative, &mut diagnostics);
    // `supersedes` separates *valid* time from *recorded* time: it marks the section this
    // one replaces, so retrieval can demote a workflow you abandoned in favour of the one
    // that replaced it. Ranking treats it like any other typed edge, so adding it here is
    // the whole feature.
    let relation_re = Regex::new(
        r"(?m)(?:(outgoing|contradicts|example-of|ingoing|supersedes)::\s*)?\[\[([^\]|]+)(?:\|[^\]]+)?\]\]",
    )?;
    let mut sections = Vec::new();
    // level, uid, heading — the heading is carried so `heading_path` falls out of the
    // stack we already maintain for `parent_uid`.
    let mut stack: Vec<(u32, String, String)> = Vec::new();
    let mut quiz_ids = HashSet::new();
    for (position, heading) in headings.iter().enumerate() {
        let end = headings
            .get(position + 1)
            .map_or(source.len(), |next| next.start);
        let section_id = &section_ids[position];
        let uid = format!("{note_id}#{section_id}");
        while stack
            .last()
            .is_some_and(|(level, _, _)| *level >= heading.level)
        {
            stack.pop();
        }
        let parent_uid = stack.last().map(|(_, uid, _)| uid.clone());
        let heading_path = stack
            .iter()
            .map(|(_, _, ancestor)| ancestor.as_str())
            .chain(std::iter::once(heading.heading.as_str()))
            .collect::<Vec<_>>()
            .join(HEADING_PATH_SEPARATOR);
        stack.push((heading.level, uid.clone(), heading.heading.clone()));
        let body = source[heading.start..end].to_string();
        let relations = relation_re
            .captures_iter(&body)
            .map(|capture| Relation {
                relation_type: capture
                    .get(1)
                    .map_or("related", |kind| kind.as_str())
                    .into(),
                target_uid: normalize_target(&capture[2], &note_id),
                context: capture[0].to_string(),
            })
            .collect();
        let mut cards = Vec::new();
        for (quiz_start, quiz) in quiz_blocks
            .iter()
            .filter(|(quiz_start, _)| *quiz_start >= heading.start && *quiz_start < end)
        {
            if !quiz_ids.insert(quiz.id().to_string()) {
                diagnostics.push(Diagnostic {
                    path: relative.to_path_buf(),
                    line: line_at(&source, *quiz_start),
                    message: format!("duplicate quiz ID `{}` in note", quiz.id()),
                });
                continue;
            }
            let errors = validate_quiz(quiz);
            for error in &errors {
                diagnostics.push(Diagnostic {
                    path: relative.to_path_buf(),
                    line: line_at(&source, *quiz_start),
                    message: format!("quiz `{}`: {error}", quiz.id()),
                });
            }
            if errors.is_empty() {
                cards.extend(cards_from_quiz(&uid, quiz)?);
            }
        }
        sections.push(ParsedSection {
            uid,
            parent_uid,
            heading: heading.heading.clone(),
            heading_path,
            level: heading.level,
            start_byte: heading.start,
            end_byte: end,
            start_line: heading.line,
            position,
            body,
            relations,
            cards,
        });
    }

    Ok(ParsedNote {
        path: relative.to_path_buf(),
        note_id,
        title,
        tags: front.tags,
        topic: front.topic,
        pinned: front.pinned,
        status: front.status,
        created_at,
        content_hash,
        modified_at,
        sections,
        diagnostics,
    })
}

fn parse_front_matter(source: &str) -> Result<(FrontMatter, usize)> {
    if !source.starts_with("---\n") {
        return Ok((FrontMatter::default(), 0));
    }
    if let Some(end) = source[4..].find("\n---\n") {
        let yaml_end = 4 + end;
        let front =
            serde_yaml::from_str(&source[4..yaml_end]).context("invalid YAML front matter")?;
        Ok((front, yaml_end + 5))
    } else {
        Ok((FrontMatter::default(), 0))
    }
}

fn collect_headings(source: &str, offset: usize) -> Vec<HeadingDraft> {
    let markdown = &source[offset..];
    let parser = Parser::new_ext(markdown, Options::all()).into_offset_iter();
    let mut headings = Vec::new();
    let mut current: Option<(u32, usize, String, Option<String>)> = None;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, id, .. }) => {
                current = Some((
                    heading_number(level),
                    offset + range.start,
                    String::new(),
                    id.map(|id| id.to_string()),
                ));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                current.as_mut().unwrap().2.push_str(&text);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, raw, attribute_id)) = current.take() {
                    let (heading, inline_id) = split_heading_id(&raw);
                    headings.push(HeadingDraft {
                        heading,
                        id: attribute_id.or(inline_id),
                        level,
                        start,
                        line: line_at(source, start),
                    });
                }
            }
            _ => {}
        }
    }
    headings
}

fn collect_quiz_blocks(
    source: &str,
    offset: usize,
    path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(usize, QuizDefinition)> {
    let parser = Parser::new_ext(&source[offset..], Options::all()).into_offset_iter();
    let mut blocks = Vec::new();
    let mut active: Option<(usize, String)> = None;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next() == Some("quiz") =>
            {
                active = Some((offset + range.start, String::new()));
            }
            Event::Text(text) if active.is_some() => active.as_mut().unwrap().1.push_str(&text),
            Event::End(TagEnd::CodeBlock) if active.is_some() => {
                let (start, yaml) = active.take().unwrap();
                match serde_yaml::from_str::<QuizDefinition>(&yaml) {
                    Ok(quiz) => blocks.push((start, quiz)),
                    Err(error) => diagnostics.push(Diagnostic {
                        path: path.to_path_buf(),
                        line: line_at(source, start),
                        message: format!("invalid quiz block: {error}"),
                    }),
                }
            }
            _ => {}
        }
    }
    blocks
}

fn cards_from_quiz(section_uid: &str, quiz: &QuizDefinition) -> Result<Vec<CardDefinition>> {
    let mut cards = Vec::new();
    match quiz {
        QuizDefinition::Cloze { id, prompt } => {
            let marker = Regex::new(r"\{\{c(\d+)::([^}:]+)(?:::[^}]+)?\}\}")?;
            let variants: HashSet<u32> = marker
                .captures_iter(prompt)
                .filter_map(|capture| capture[1].parse().ok())
                .collect();
            for cloze in variants {
                cards.push(make_card(
                    section_uid,
                    id,
                    "cloze",
                    &format!("c{cloze}"),
                    CardContent::Cloze {
                        prompt: prompt.clone(),
                        cloze,
                    },
                )?);
            }
        }
        QuizDefinition::MultipleChoice {
            id,
            mode,
            question,
            answers,
            explanation,
        } => cards.push(make_card(
            section_uid,
            id,
            "multiple-choice",
            "main",
            CardContent::MultipleChoice {
                mode: mode.clone(),
                question: question.clone(),
                answers: answers.clone(),
                explanation: explanation.clone(),
            },
        )?),
        QuizDefinition::CodeGap {
            id,
            language,
            prompt,
            code,
            gaps,
        } => cards.push(make_card(
            section_uid,
            id,
            "code-gap",
            "main",
            CardContent::CodeGap {
                language: language.clone(),
                prompt: prompt.clone(),
                code: code.clone(),
                gaps: gaps.clone(),
            },
        )?),
    }
    Ok(cards)
}

fn make_card(
    section_uid: &str,
    quiz_id: &str,
    card_type: &str,
    variant: &str,
    content: CardContent,
) -> Result<CardDefinition> {
    let json = serde_json::to_string(&content)?;
    Ok(CardDefinition {
        uid: format!("{section_uid}/{quiz_id}:{variant}"),
        section_uid: section_uid.into(),
        quiz_id: quiz_id.into(),
        card_type: card_type.into(),
        variant_key: variant.into(),
        content_hash: blake3::hash(json.as_bytes()).to_hex().to_string(),
        content,
    })
}

fn split_heading_id(raw: &str) -> (String, Option<String>) {
    let pattern = Regex::new(r"^(.*?)\s*\{#([A-Za-z0-9_-]+)\}\s*$").unwrap();
    pattern.captures(raw).map_or_else(
        || (raw.trim().to_string(), None),
        |capture| (capture[1].trim().to_string(), Some(capture[2].to_string())),
    )
}

fn normalize_target(target: &str, note_id: &str) -> String {
    let target = target.trim();
    if target.starts_with('#') {
        format!("{note_id}{target}")
    } else if target.contains('#') {
        target.to_string()
    } else {
        format!("{target}#root")
    }
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

fn heading_number(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn line_at(source: &str, byte: usize) -> usize {
    1 + source[..byte.min(source.len())]
        .bytes()
        .filter(|value| *value == b'\n')
        .count()
}

pub fn markdown_files(vault: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(vault)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".notes")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "md")
        })
        .map(|entry| entry.into_path())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::model::card_capabilities;

    #[test]
    fn parses_sections_relations_and_all_card_types() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rust.md");
        fs::write(
            &path,
            r#"---
id: rust
title: Rust
tags: [rust]
---
# Rust {#root}
## Borrowing {#borrowing}
Related: [[memory#safety]]
outgoing:: [[rust#root]]
ingoing:: [[memory#ownership]]

```quiz
id: q1
type: cloze
prompt: One {{c1::owner}} and {{c2::borrowers}}.
```

```quiz
id: q2
type: multiple-choice
mode: single
question: Pick one.
answers:
  - id: yes
    text: Yes
    correct: true
  - id: no
    text: No
    correct: false
```

```quiz
id: q3
type: code-gap
language: rust
code: "let x = {{gap:value}};"
gaps:
  value:
    answer: "1"
```
"#,
        )
        .unwrap();
        let note = parse_note(&path, dir.path()).unwrap();
        assert_eq!(note.note_id, "rust");
        assert_eq!(note.sections.len(), 2);
        assert_eq!(note.sections[1].parent_uid.as_deref(), Some("rust#root"));
        assert_eq!(note.sections[1].relations[0].target_uid, "memory#safety");
        assert_eq!(note.sections[1].relations[1].relation_type, "outgoing");
        assert_eq!(note.sections[1].relations[2].relation_type, "ingoing");
        assert_eq!(note.sections[1].cards.len(), 4);
        assert!(note.diagnostics.is_empty(), "{:?}", note.diagnostics);
    }

    #[test]
    fn heading_path_names_every_ancestor_and_supersedes_is_a_relation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("obs.md");
        fs::write(
            &path,
            r#"---
id: obs
---
# OBS {#root}
## Cursor follow {#follow}
### Smoothing {#smoothing}
supersedes:: [[obs#old-smoothing]]
## Scenes {#scenes}
"#,
        )
        .unwrap();
        let note = parse_note(&path, dir.path()).unwrap();

        let paths: Vec<&str> = note
            .sections
            .iter()
            .map(|section| section.heading_path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "OBS",
                "OBS > Cursor follow",
                "OBS > Cursor follow > Smoothing",
                // Back out to depth 2: the H3 must not stay on the stack.
                "OBS > Scenes",
            ]
        );

        let smoothing = &note.sections[2];
        assert_eq!(smoothing.relations[0].relation_type, "supersedes");
        assert_eq!(smoothing.relations[0].target_uid, "obs#old-smoothing");
    }

    #[test]
    fn advertised_card_templates_are_valid() {
        let dir = tempdir().unwrap();
        for capability in card_capabilities() {
            let path = dir.path().join(format!("{}.md", capability.card_type));
            let template = capability.template.replace("${id}", "template-test");
            fs::write(&path, format!("# Test {{#root}}\n\n{template}\n")).unwrap();
            let note = parse_note(&path, dir.path()).unwrap();
            assert!(note.diagnostics.is_empty(), "{:?}", note.diagnostics);
            assert_eq!(note.sections[0].cards[0].card_type, capability.card_type);
        }
    }
}
