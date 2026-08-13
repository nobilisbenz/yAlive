use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Deserialize;

use crate::model::{
    CardClips, CardContent, CardDefinition, ClipRef, Diagnostic, ParsedAction, ParsedNote,
    ParsedSection, QuizDefinition, Relation, validate_quiz,
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
    // `@file`, `@video`, `@app`, `@project`, `@url` on a line of their own. Anchored to
    // line start so a `@file` mentioned mid-sentence in prose is not mistaken for one.
    let action_re = Regex::new(r"(?m)^\s*@(file|video|app|project|url)\s+(.+?)\s*$")?;
    let note_dir = path.parent().unwrap_or(vault).to_path_buf();

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
        let actions = parse_actions(&body, &note_dir, &action_re);
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
                // A quiz with no `clip:` inherits the section's own `@video`, so
                // the common case needs no new syntax at all.
                let inherited = actions
                    .iter()
                    .find(|action| action.kind == "video")
                    .map(|action| ClipRef {
                        video_id: youtube_id(&action.target),
                        url: action.target.clone(),
                        start: action.timestamp_seconds.unwrap_or(0),
                        end: None,
                        label: None,
                    });
                cards.extend(cards_from_quiz(&uid, quiz, inherited.as_ref())?);
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
            // Attached to the innermost open section, which is what `body` already is.
            actions,
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

/// Parse the `@action` lines in a section body.
///
/// `note_dir` is the directory containing the note, so a relative `@file ./diagram.png`
/// resolves the way the author meant it — relative to what they were looking at, not to
/// wherever the daemon happens to be running.
fn parse_actions(body: &str, note_dir: &Path, pattern: &Regex) -> Vec<ParsedAction> {
    pattern
        .captures_iter(body)
        .filter_map(|capture| {
            let kind = capture.get(1)?.as_str();
            let rest = capture.get(2)?.as_str().trim();
            if rest.is_empty() {
                return None;
            }

            Some(match kind {
                "file" => {
                    let (path, line) = split_path_and_line(rest);
                    ParsedAction {
                        kind: "file".into(),
                        target: absolute(path, note_dir),
                        line,
                        timestamp_seconds: None,
                    }
                }
                "project" => ParsedAction {
                    kind: "project".into(),
                    target: absolute(rest, note_dir),
                    line: None,
                    timestamp_seconds: None,
                },
                "video" => {
                    // `@video URL HH:MM:SS`, with the timestamp optional and the URL itself
                    // possibly already carrying one.
                    let (url, trailing) = match rest.split_once(char::is_whitespace) {
                        Some((url, rest)) => (url, rest.trim()),
                        None => (rest, ""),
                    };
                    let (clean, embedded) = strip_url_timestamp(url);
                    // Only the first token is a timestamp candidate: the
                    // documented form is `@video URL 06:54  Label`, and feeding
                    // the label to the parser made it reject the whole thing.
                    let stamp = trailing.split_whitespace().next().unwrap_or_default();
                    let seconds = parse_timestamp(stamp).or(embedded);
                    ParsedAction {
                        kind: "video".into(),
                        // Stored clean and rebuilt at launch time by trusted code (spec
                        // §31), rather than round-tripping a URL a note author hand-edited.
                        target: clean,
                        line: None,
                        timestamp_seconds: seconds,
                    }
                }
                "app" => ParsedAction {
                    kind: "app".into(),
                    target: rest.to_string(),
                    line: None,
                    timestamp_seconds: None,
                },
                _ => ParsedAction {
                    kind: "url".into(),
                    target: rest.to_string(),
                    line: None,
                    timestamp_seconds: None,
                },
            })
        })
        .collect()
}

/// Split `PATH:LINE`, on the **last** colon.
///
/// Splitting on the first would break `/home/nabi/notes/c:/thing.md` and every Windows-ish
/// or colon-containing path. A trailing segment only counts as a line number if it is
/// entirely digits — `foo.md:bar` is a path.
fn split_path_and_line(text: &str) -> (&str, Option<u32>) {
    match text.rsplit_once(':') {
        Some((path, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (path, tail.parse().ok())
        }
        _ => (text, None),
    }
}

/// Expand `~` and resolve a relative path against the note's directory.
fn absolute(path: &str, note_dir: &Path) -> String {
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    };

    if expanded.is_absolute() {
        expanded.to_string_lossy().to_string()
    } else {
        note_dir.join(expanded).to_string_lossy().to_string()
    }
}

/// `HH:MM:SS`, `MM:SS`, `1h2m3s`, or bare seconds.
fn parse_timestamp(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    if text.contains(':') {
        let mut seconds = 0u64;
        for part in text.split(':') {
            seconds = seconds.checked_mul(60)?.checked_add(part.parse().ok()?)?;
        }
        return Some(seconds);
    }

    if let Ok(bare) = text.parse::<u64>() {
        return Some(bare);
    }

    // `1h30m15s`, the form YouTube itself uses.
    let mut total = 0u64;
    let mut current = 0u64;
    let mut saw_unit = false;
    for character in text.chars() {
        match character {
            '0'..='9' => current = current.checked_mul(10)?.checked_add(character as u64 - '0' as u64)?,
            'h' => {
                total += current * 3600;
                current = 0;
                saw_unit = true;
            }
            'm' => {
                total += current * 60;
                current = 0;
                saw_unit = true;
            }
            's' => {
                total += current;
                current = 0;
                saw_unit = true;
            }
            _ => return None,
        }
    }
    saw_unit.then_some(total + current)
}

/// Lift a `t=` timestamp out of a URL, returning the URL without it.
///
/// A note author who copied a link from YouTube's "share at current time" gets the
/// timestamp honoured without having to restate it.
/// Parse a `clip:` value — `URL`, `URL 06:54`, `URL 06:54-07:20`, or any of
/// those followed by a label.
///
/// The range separator is `-`, which cannot collide with a timestamp: `06:54`
/// uses colons, and the `1h2m3s` form has no hyphen either.
pub fn parse_clip(raw: &str) -> Option<ClipRef> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let (url, trailing) = match raw.split_once(char::is_whitespace) {
        Some((url, rest)) => (url, rest.trim()),
        None => (raw, ""),
    };

    let (clean, embedded) = strip_url_timestamp(url);

    // The first token after the URL is the range; anything after it is a label.
    let (range, label) = match trailing.split_once(char::is_whitespace) {
        Some((range, rest)) => (range, rest.trim()),
        None => (trailing, ""),
    };

    let (start, end) = match range.split_once('-') {
        Some((from, to)) => (parse_timestamp(from), parse_timestamp(to)),
        None => (parse_timestamp(range), None),
    };

    // A `range` that parsed as nothing was really a label.
    let (start, label) = match start {
        Some(start) => (Some(start), label),
        None if !range.is_empty() => (None, trailing),
        None => (None, label),
    };

    Some(ClipRef {
        video_id: youtube_id(&clean),
        url: clean,
        start: start.or(embedded).unwrap_or(0),
        end,
        label: (!label.is_empty()).then(|| label.to_string()),
    })
}

/// The 11-character id out of any YouTube URL shape, or `None` for anything else.
fn youtube_id(url: &str) -> Option<String> {
    if !url.contains("youtube.com") && !url.contains("youtu.be") {
        return None;
    }
    let pattern = Regex::new(r"(?:v=|/v/|youtu\.be/|/embed/|/shorts/|/live/)([A-Za-z0-9_-]{11})")
        .ok()?;
    pattern
        .captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn strip_url_timestamp(url: &str) -> (String, Option<u64>) {
    let Some((base, query)) = url.split_once('?') else {
        return (url.to_string(), None);
    };

    let mut seconds = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|parameter| {
            match parameter.split_once('=') {
                // `t` is the canonical one; `start` is what embeds use.
                Some(("t" | "start", value)) => {
                    seconds = parse_timestamp(value.trim_end_matches('s'));
                    // Only drop it if we understood it; an unparseable `t` is left alone
                    // rather than silently deleted from the link.
                    seconds.is_none()
                }
                _ => true,
            }
        })
        .collect();

    if seconds.is_none() {
        return (url.to_string(), None);
    }
    if kept.is_empty() {
        (base.to_string(), seconds)
    } else {
        (format!("{base}?{}", kept.join("&")), seconds)
    }
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

fn cards_from_quiz(
    section_uid: &str,
    quiz: &QuizDefinition,
    inherited: Option<&ClipRef>,
) -> Result<Vec<CardDefinition>> {
    let (answer_src, prompt_src) = quiz.clip_sources();
    // An explicit `clip:` wins; otherwise the section's `@video` becomes the
    // answer-side clip. A `prompt_clip:` is never inherited — putting a video
    // beside the question is a deliberate choice about what the card tests.
    let clips = CardClips {
        prompt: prompt_src.and_then(parse_clip),
        answer: answer_src
            .and_then(parse_clip)
            .or_else(|| inherited.cloned()),
    };

    let mut cards = Vec::new();
    match quiz {
        QuizDefinition::Cloze { id, prompt, .. } => {
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
                        clips: clips.clone(),
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
            ..
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
                clips: clips.clone(),
            },
        )?),
        QuizDefinition::CodeGap {
            id,
            language,
            prompt,
            code,
            gaps,
            ..
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
                clips: clips.clone(),
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

    /// Write a note into a temp vault and parse it, returning both so the
    /// tempdir outlives the borrow.
    fn note_from(source: &str) -> (tempfile::TempDir, ParsedNote) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rust.md");
        std::fs::write(&path, source).unwrap();
        let note = parse_note(&path, dir.path()).unwrap();
        (dir, note)
    }

    fn actions_in(body: &str) -> Vec<ParsedAction> {
        let pattern = Regex::new(r"(?m)^\s*@(file|video|app|project|url)\s+(.+?)\s*$").unwrap();
        parse_actions(body, Path::new("/home/nabi/brain"), &pattern)
    }

    #[test]
    fn a_file_action_takes_a_line_number_from_the_last_colon() {
        // Splitting on the *first* colon breaks any path containing one, and a trailing
        // segment that is not all digits is part of the path rather than a line.
        assert_eq!(split_path_and_line("/tmp/a.rs:42"), ("/tmp/a.rs", Some(42)));
        assert_eq!(
            split_path_and_line("/tmp/odd:name/a.rs:42"),
            ("/tmp/odd:name/a.rs", Some(42))
        );
        assert_eq!(split_path_and_line("/tmp/a.rs:bar"), ("/tmp/a.rs:bar", None));
        assert_eq!(split_path_and_line("/tmp/a.rs"), ("/tmp/a.rs", None));
    }

    #[test]
    fn a_relative_file_action_resolves_against_the_note_not_the_daemon() {
        // The author wrote the path relative to what they were looking at. Resolving it
        // against the process's cwd would work in testing and break at login.
        let actions = actions_in("@file ./diagrams/pipeline.svg\n");
        assert_eq!(actions[0].target, "/home/nabi/brain/./diagrams/pipeline.svg");
    }

    #[test]
    fn a_tilde_in_an_action_is_expanded() {
        let actions = actions_in("@file ~/projects/obs/src/smoothing.rs:41\n");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            actions[0].target,
            format!("{home}/projects/obs/src/smoothing.rs")
        );
        assert_eq!(actions[0].line, Some(41));
    }

    #[test]
    fn a_video_timestamp_is_accepted_in_every_form_a_note_might_use() {
        assert_eq!(parse_timestamp("1:23"), Some(83));
        assert_eq!(parse_timestamp("01:02:03"), Some(3723));
        assert_eq!(parse_timestamp("414"), Some(414));
        assert_eq!(parse_timestamp("6m54s"), Some(414));
        assert_eq!(parse_timestamp("1h30m"), Some(5400));
        assert_eq!(parse_timestamp("nonsense"), None);
        assert_eq!(parse_timestamp(""), None);
    }

    #[test]
    fn a_timestamp_already_in_the_url_is_lifted_out_and_the_url_stored_clean() {
        // Spec §31: store the canonical URL and rebuild the timestamped one at launch in
        // trusted code, rather than round-tripping whatever a note author pasted.
        let actions = actions_in("@video https://youtu.be/ABC?t=414\n");
        assert_eq!(actions[0].target, "https://youtu.be/ABC");
        assert_eq!(actions[0].timestamp_seconds, Some(414));

        // Other query parameters survive.
        let actions = actions_in("@video https://www.youtube.com/watch?v=ABC&t=6m54s\n");
        assert_eq!(actions[0].target, "https://www.youtube.com/watch?v=ABC");
        assert_eq!(actions[0].timestamp_seconds, Some(414));
    }

    #[test]
    fn an_explicit_timestamp_beats_one_embedded_in_the_url() {
        let actions = actions_in("@video https://youtu.be/ABC?t=10 1:00\n");
        assert_eq!(actions[0].timestamp_seconds, Some(60));
    }

    #[test]
    fn clip_values_carry_a_range_and_an_optional_label() {
        let clip = parse_clip("https://youtu.be/dQw4w9WgXcQ 06:54-07:20  The borrow checker bit")
            .unwrap();
        assert_eq!(clip.url, "https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(clip.video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(clip.start, 414);
        assert_eq!(clip.end, Some(440));
        assert_eq!(clip.label.as_deref(), Some("The borrow checker bit"));

        // A start with no end means "play to the end".
        let clip = parse_clip("https://youtu.be/dQw4w9WgXcQ 06:54").unwrap();
        assert_eq!(clip.start, 414);
        assert_eq!(clip.end, None);

        // A bare URL is a whole video.
        let clip = parse_clip("https://youtu.be/dQw4w9WgXcQ").unwrap();
        assert_eq!(clip.start, 0);
        assert_eq!(clip.label, None);

        // A timestamp already in the URL is lifted out, as `@video` does.
        let clip = parse_clip("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=90s").unwrap();
        assert_eq!(clip.url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(clip.start, 90);

        // Text where a range would be is a label, not a failed parse.
        let clip = parse_clip("https://youtu.be/dQw4w9WgXcQ  Just a label").unwrap();
        assert_eq!(clip.start, 0);
        assert_eq!(clip.label.as_deref(), Some("Just a label"));

        // A non-YouTube URL is still a clip, just without an id to embed.
        let clip = parse_clip("https://example.com/talk.mp4 1:00").unwrap();
        assert_eq!(clip.video_id, None);
        assert_eq!(clip.start, 60);

        assert!(parse_clip("   ").is_none());
    }

    #[test]
    fn a_quiz_without_a_clip_inherits_the_sections_video() {
        let (_dir, note) = note_from(
            "---\nid: rust\ntitle: Rust\n---\n\n# Rust {#root}\n\n\
             @video https://youtu.be/dQw4w9WgXcQ 06:54  Chapter on borrowing\n\n\
             ```quiz\nid: q1\ntype: cloze\nprompt: A {{c1::mutable}} borrow is exclusive.\n```\n",
        );

        let clips = match &note.sections[0].cards[0].content {
            CardContent::Cloze { clips, .. } => clips.clone(),
            other => panic!("expected a cloze card, got {other:?}"),
        };
        let answer = clips.answer.expect("should have inherited the @video");
        assert_eq!(answer.url, "https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(answer.start, 414);
        // Inheritance never fills the prompt side: that is a deliberate choice.
        assert!(clips.prompt.is_none());
    }

    #[test]
    fn an_explicit_clip_beats_the_inherited_one_and_prompt_clip_is_separate() {
        let (_dir, note) = note_from(
            "---\nid: rust\ntitle: Rust\n---\n\n# Rust {#root}\n\n\
             @video https://youtu.be/AAAAAAAAAAA 1:00\n\n\
             ```quiz\nid: q1\ntype: cloze\nprompt: A {{c1::mutable}} borrow.\n\
             clip: https://youtu.be/BBBBBBBBBBB 2:00-3:00\n\
             prompt_clip: https://youtu.be/CCCCCCCCCCC 4:00\n```\n",
        );

        let clips = match &note.sections[0].cards[0].content {
            CardContent::Cloze { clips, .. } => clips.clone(),
            other => panic!("expected a cloze card, got {other:?}"),
        };
        assert_eq!(clips.answer.as_ref().unwrap().video_id.as_deref(), Some("BBBBBBBBBBB"));
        assert_eq!(clips.answer.as_ref().unwrap().end, Some(180));
        assert_eq!(clips.prompt.as_ref().unwrap().video_id.as_deref(), Some("CCCCCCCCCCC"));
        assert_eq!(clips.prompt.as_ref().unwrap().start, 240);
    }

    #[test]
    fn a_backwards_clip_range_is_a_diagnostic() {
        let (_dir, note) = note_from(
            "---\nid: rust\ntitle: Rust\n---\n\n# Rust {#root}\n\n\
             ```quiz\nid: q1\ntype: cloze\nprompt: A {{c1::mutable}} borrow.\n\
             clip: https://youtu.be/BBBBBBBBBBB 3:00-2:00\n```\n",
        );

        assert!(
            note.diagnostics
                .iter()
                .any(|d| d.message.contains("not after its start")),
            "expected a range diagnostic, got {:?}",
            note.diagnostics
        );
    }

    #[test]
    fn a_label_after_the_timestamp_does_not_swallow_it() {
        // The documented form carries a human label: `@video URL 06:54  Label`.
        let actions = actions_in("@video https://youtu.be/ABC 06:54  Chapter on borrowing\n");
        assert_eq!(actions[0].target, "https://youtu.be/ABC");
        assert_eq!(actions[0].timestamp_seconds, Some(414));

        // A label with no timestamp still leaves the URL's own one intact.
        let actions = actions_in("@video https://youtu.be/ABC?t=90 Just a label\n");
        assert_eq!(actions[0].timestamp_seconds, Some(90));

        // And a bare label with nothing to fall back on is simply no timestamp.
        let actions = actions_in("@video https://youtu.be/ABC Just a label\n");
        assert_eq!(actions[0].timestamp_seconds, None);
    }

    #[test]
    fn an_unparseable_timestamp_is_left_in_the_url_rather_than_deleted() {
        // Silently dropping a query parameter we did not understand would break the link.
        let actions = actions_in("@video https://example.com/v?t=chapter-three\n");
        assert_eq!(actions[0].target, "https://example.com/v?t=chapter-three");
        assert_eq!(actions[0].timestamp_seconds, None);
    }

    #[test]
    fn an_action_mentioned_mid_sentence_is_prose_not_an_action() {
        // Notes talk *about* things. `@file` has to be a line of its own to count.
        let actions = actions_in("I use the @file syntax to link things.\nSee @app blender too.\n");
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn every_action_kind_parses() {
        let actions = actions_in(
            "@file /tmp/a.rs:12\n\
             @video https://youtu.be/ABC 2:00\n\
             @app com.obsproject.Studio\n\
             @project ~/projects/obs\n\
             @url https://example.com/docs\n",
        );
        let kinds: Vec<&str> = actions.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["file", "video", "app", "project", "url"]);
        assert_eq!(actions[1].timestamp_seconds, Some(120));
        assert_eq!(actions[2].target, "com.obsproject.Studio");
    }

    #[test]
    fn actions_attach_to_the_section_that_declared_them() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("obs.md");
        fs::write(
            &path,
            "---\nid: obs\ntitle: OBS\n---\n\
             # OBS {#root}\n@app com.obsproject.Studio\n\
             ## Smoothing {#smooth}\n@video https://youtu.be/ABC 6:54\n",
        )
        .unwrap();

        let note = parse_note(&path, dir.path()).unwrap();
        let root = note.sections.iter().find(|s| s.uid == "obs#root").unwrap();
        let smooth = note.sections.iter().find(|s| s.uid == "obs#smooth").unwrap();

        assert_eq!(root.actions.len(), 1);
        assert_eq!(root.actions[0].kind, "app");
        assert_eq!(smooth.actions.len(), 1);
        assert_eq!(smooth.actions[0].timestamp_seconds, Some(414));
    }

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
