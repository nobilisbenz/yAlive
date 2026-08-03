# yalive

`yalive` is a section-first Markdown knowledge vault and spaced-repetition TUI. Markdown owns the knowledge; a disposable SQLite database provides FTS5 search, graph edges, diagnostics, and FSRS review state.

## Run

```bash
cargo run
cargo run -- --vault examples/vault
```

Without `--vault`, yalive reopens the last used vault. On first launch it asks whether to open an existing directory or create a new vault. The remembered path is stored in the platform configuration directory. The index is created at `<vault>/.notes/index.sqlite`. Changed Markdown files are detected by content hash while the TUI is running.

```bash
cargo run -- --vault ~/Notes index
cargo run -- --vault ~/Notes diagnostics
cargo run -- --vault ~/Notes export-reviews
```

Machine-readable editor commands are also available. They are versioned JSON so editor plugins do not need to read the disposable SQLite schema:

```bash
yalive --vault ~/Notes editor capabilities
yalive --vault ~/Notes editor sections "borrowing"
yalive --vault ~/Notes editor relations rust-ownership#borrowing
yalive --vault ~/Notes editor diagnostics
```

## Keys

| Mode | Keys |
| --- | --- |
| Pages | `1` dashboard, `2` reviews, `3` relations, `4` statistics, `5` cleanup, `6` options |
| Panel focus | `Shift+h` left, `Shift+l` right, `Shift+j` down, `Shift+k` up |
| Dashboard | `Enter` opens a note/section, `n` creates a note, `/` searches |
| Dashboard | `g` or `b` relations/backlinks, `o` opens URL, `i` opens image |
| Review setup | `Enter` opens notes/sections, activates decks, or reviews cards |
| Review setup | `Space` enrolls a section, `r` reviews due cards, `n` creates a deck |
| Review setup | `[`/`]` choose active deck, `a` assign card, `x` delete active deck |
| Refresh | `Shift+r` reloads Markdown and SQLite-backed lists immediately |
| Relations | `j/k` selects in the focused panel, `Enter` follows incoming/outgoing links or opens the center section |
| Cleanup | `Enter` opens an item, `a` assigns a card, `d` deletes an unreferenced image |
| Options | `j/k` selects, `h/l` or arrows changes values, `Space` toggles, `Enter` opens/creates a vault |
| Search | type to search, arrows navigate, `Enter` open, `Esc` cancel |
| Review | `Space` reveal cloze, `j/k` and `Space` choose answers |
| Review | type code gaps, `Tab` changes gap, `Enter` checks |
| Rating | `1` Again, `2` Hard, `3` Good, `4` Easy |

`e` uses `editor` from `.notes/config.toml`, then `$VISUAL`, `$EDITOR`, and finally `nvim`. The TUI suspends and returns to the same stable section after editing.

## Neovim

A Neovim plugin is included in [`nvim/`](nvim/README.md). It discovers card templates and relation syntax from `yalive editor capabilities`, so adding a card type to the Rust capability registry makes it available without changing Lua. It provides fzf-backed card creation, section search, incoming/outgoing relation editing, relation navigation, automatic indexing, and diagnostics.

## Note Format

Notes should have a stable front matter ID and stable section IDs:

```markdown
---
id: rust-ownership
title: Rust Ownership
tags: [rust, memory]
topic: Programming
pinned: true
---

# Rust Ownership {#root}
## Borrowing {#borrowing}

prerequisite:: [[rust-basics#references]]
```

Quiz blocks are fenced `quiz` blocks containing YAML. Supported `type` values are `cloze`, `multiple-choice`, and `code-gap`. See [`examples/vault/rust-ownership.md`](examples/vault/rust-ownership.md) for complete examples.

## Configuration

Optional `<vault>/.notes/config.toml`:

```toml
editor = "nvim"
desired_retention = 0.90
new_cards_per_day = 20
max_reviews_per_day = 200
review_order = "due"
bury_siblings = true
reindex_interval_ms = 1000
```

Review submissions are not stored. Ratings, correctness, response time, FSRS memory state, and scheduling intervals are retained. Export review history periodically with `export-reviews`; the default output is `.notes/reviews.jsonl`.

## Guarantees

- Notes, quiz definitions, links, tags, and images remain ordinary Markdown files.
- Search results and review cards target stable sections, not only files.
- Moving a file or renaming a heading preserves identity when its IDs stay unchanged.
- Reindexing preserves review state for unchanged card UIDs.
- The index can be rebuilt from Markdown; review history, decks, and deck assignments are the non-derived SQLite data worth backing up.
