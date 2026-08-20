//! The command palette behind `Ctrl+K`.
//!
//! Four tabs cover what the vault is for — writing, reviewing, connecting, and
//! measuring. Everything else is maintenance: cleanup, settings, the archive,
//! syncing, switching vaults. Those used to occupy three of seven tabs and a
//! permanent row of chrome for actions taken a few times a month. They live
//! here instead, one keystroke away and out of the way.

use super::Page;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    OpenClean,
    OpenOptions,
    OpenArchived,
    NewNote,
    NewDeck,
    SyncNow,
    Reindex,
    AuthenticateGithub,
    SetRepository,
    OpenVault,
    CreateVault,
    Quit,
}

pub struct Entry {
    pub command: Command,
    pub name: &'static str,
    /// One line explaining what running this does, shown beside the name.
    pub detail: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry {
        command: Command::OpenClean,
        name: "Clean",
        detail: "notes without topics, cards without decks, unused images",
    },
    Entry {
        command: Command::OpenOptions,
        name: "Options",
        detail: "review scheduling, editor, and GitHub sync",
    },
    Entry {
        command: Command::OpenArchived,
        name: "Archived",
        detail: "restore something you archived",
    },
    Entry {
        command: Command::NewNote,
        name: "New note",
        detail: "create a Markdown note and open it in your editor",
    },
    Entry {
        command: Command::NewDeck,
        name: "New deck",
        detail: "group cards into a deck you can review on its own",
    },
    Entry {
        command: Command::SyncNow,
        name: "Sync now",
        detail: "commit, fetch, integrate, and push the vault",
    },
    Entry {
        command: Command::Reindex,
        name: "Reindex vault",
        detail: "re-read every Markdown file and rebuild the search index",
    },
    Entry {
        command: Command::AuthenticateGithub,
        name: "Authenticate with GitHub",
        detail: "sign in through GitHub CLI and configure Git credentials",
    },
    Entry {
        command: Command::SetRepository,
        name: "Set repository URL",
        detail: "choose the GitHub repository this vault syncs with",
    },
    Entry {
        command: Command::OpenVault,
        name: "Open another vault",
        detail: "close this vault and open an existing one",
    },
    Entry {
        command: Command::CreateVault,
        name: "Create new vault",
        detail: "make a directory, index it, and remember it as the default",
    },
    Entry {
        command: Command::Quit,
        name: "Quit",
        detail: "leave yalive",
    },
];

impl Command {
    /// The page this command opens, when it is a navigation command.
    pub fn page(self) -> Option<Page> {
        match self {
            Command::OpenClean => Some(Page::Clean),
            Command::OpenOptions => Some(Page::Options),
            Command::OpenArchived => Some(Page::Archived),
            _ => None,
        }
    }
}

/// Entries matching `query`, best match first.
///
/// Matching is a case-insensitive subsequence over the name, so `nn` finds
/// "New note" and `sync` finds "Sync now". An entry whose name contains the
/// query as a run of characters outranks one that merely spells it out across
/// the name, which keeps exact prefixes at the top where they belong.
pub fn matching(query: &str) -> Vec<&'static Entry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return ENTRIES.iter().collect();
    }
    let mut scored: Vec<(u8, usize, &'static Entry)> = ENTRIES
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let haystack = entry.name.to_lowercase();
            if let Some(position) = haystack.find(&needle) {
                // Rank 0 for a contiguous hit, earliest position first.
                Some((0, position * 100 + index, entry))
            } else if is_subsequence(&needle, &haystack) {
                Some((1, index, entry))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by_key(|(rank, tiebreak, _)| (*rank, *tiebreak));
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut characters = haystack.chars();
    needle
        .chars()
        .all(|wanted| characters.any(|candidate| candidate == wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_offers_every_command() {
        assert_eq!(matching("").len(), ENTRIES.len());
    }

    #[test]
    fn contiguous_matches_outrank_scattered_ones() {
        let results = matching("new");
        assert!(results[0].name.starts_with("New"));
    }

    #[test]
    fn initials_find_a_command() {
        let results = matching("nn");
        assert!(results.iter().any(|entry| entry.name == "New note"));
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing() {
        assert!(matching("zzzz").is_empty());
    }
}
