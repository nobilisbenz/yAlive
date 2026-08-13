//! Launching an `@video` action in an external player.
//!
//! The vault's contract is one line of Markdown:
//!
//! ```text
//! @video https://www.youtube.com/watch?v=dQw4w9WgXcQ 06:54  Chapter on borrowing
//! ```
//!
//! The parser turns that into a target; this module turns a target into an argv
//! and spawns it detached. The placeholder shape matches yy's `[openers]` so a
//! single template works in both, and — as in yy — a template that cannot take a
//! timestamp separately gets it rebuilt into the URL here, in trusted code, from
//! the parsed number rather than by string-mangling what the author wrote.

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// The default when `.notes/config.toml` sets no `player`. Opens in whatever
/// handles the URL, with the timestamp rebuilt in by [`expand`].
pub fn default_template() -> Vec<String> {
    vec!["xdg-open".to_string(), "{url}".to_string()]
}

/// Expand `{url}` and `{seconds}` into an argv.
///
/// Substitution happens inside each element independently and nothing is
/// re-split on whitespace, so a URL containing a space stays one argument.
pub fn expand(template: &[String], url: &str, seconds: Option<u64>) -> Vec<String> {
    let rebuild_url =
        seconds.is_some() && !template.iter().any(|part| part.contains("{seconds}"));

    template
        .iter()
        .map(|element| {
            let url = match (rebuild_url, seconds) {
                (true, Some(seconds)) => with_timestamp(url, seconds),
                _ => url.to_string(),
            };
            element
                .replace("{url}", &url)
                .replace("{seconds}", &seconds.unwrap_or(0).to_string())
        })
        .collect()
}

/// Put a timestamp back into a URL for a handler that cannot take one separately.
fn with_timestamp(url: &str, seconds: u64) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}t={seconds}s")
}

/// Spawn the expanded template, detached, so the TUI keeps its terminal.
pub fn play(template: &[String], url: &str, seconds: Option<u64>) -> Result<Vec<String>> {
    let argv = expand(template, url, seconds);
    let (program, args) = argv
        .split_first()
        .context("player template is empty; set `player` in .notes/config.toml")?;

    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not launch `{program}`"))?;

    Ok(argv)
}

/// `414` → `6:54`, `4014` → `1:06:54`. The shape `@video` lines are written in.
pub fn format_hms(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The first YouTube-ish URL in a block of text, for sections that carry a link
/// without an `@video` line.
pub fn first_video_url(body: &str) -> Option<String> {
    let re = regex::Regex::new(r"https?://[^\s)>\]]+").ok()?;
    re.find_iter(body)
        .map(|m| m.as_str())
        .find(|url| {
            url.contains("youtube.com") || url.contains("youtu.be")
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn seconds_placeholder_is_filled_and_the_url_left_alone() {
        let template = argv(&["yclippy", "play", "{url}", "--at", "{seconds}"]);
        let out = expand(&template, "https://youtu.be/ABC", Some(414));
        assert_eq!(out, argv(&["yclippy", "play", "https://youtu.be/ABC", "--at", "414"]));
    }

    #[test]
    fn a_template_without_seconds_gets_the_timestamp_rebuilt_into_the_url() {
        let template = argv(&["xdg-open", "{url}"]);
        let out = expand(&template, "https://youtu.be/ABC", Some(414));
        assert_eq!(out, argv(&["xdg-open", "https://youtu.be/ABC?t=414s"]));
    }

    #[test]
    fn rebuilding_respects_an_existing_query_string() {
        let template = argv(&["xdg-open", "{url}"]);
        let out = expand(&template, "https://www.youtube.com/watch?v=ABC", Some(90));
        assert_eq!(out, argv(&["xdg-open", "https://www.youtube.com/watch?v=ABC&t=90s"]));
    }

    #[test]
    fn no_timestamp_means_no_rebuild() {
        let template = argv(&["xdg-open", "{url}"]);
        let out = expand(&template, "https://youtu.be/ABC", None);
        assert_eq!(out, argv(&["xdg-open", "https://youtu.be/ABC"]));
    }

    #[test]
    fn substitution_does_not_resplit_on_whitespace() {
        let template = argv(&["mpv", "--start={seconds}", "{url}"]);
        let out = expand(&template, "https://example.com/a b", Some(5));
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], "https://example.com/a b");
    }

    #[test]
    fn finds_only_video_urls_in_a_body() {
        let body = "See https://example.com/docs and https://youtu.be/ABC for more.";
        assert_eq!(
            first_video_url(body).as_deref(),
            Some("https://youtu.be/ABC")
        );
        assert_eq!(first_video_url("no links here"), None);
    }
}
