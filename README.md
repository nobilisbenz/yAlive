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
cargo run -- --vault ~/Notes sync --repo git@github.com:owner/vault.git
cargo run -- --vault ~/Notes sync
```

### GitHub Sync

The Options tab includes the complete setup flow: authenticate with GitHub CLI, enter the repository URL, and select Sync now. Yalive does not accept or store classic access tokens. Use either:

- `git@github.com:owner/vault.git` with an SSH key configured in GitHub.
- `https://github.com/owner/vault.git` after the Options authentication action, or after running `gh auth login` and `gh auth setup-git`.

Sync commits local changes, fetches and integrates the remote branch, and pushes. Conflicts stop without overwriting either side. Use a standalone vault directory, not a directory nested inside another Git repository. The disposable SQLite index and graph IPC files are automatically ignored; Markdown, images, configuration, and portable review exports are synced.

### Android Reviewer

[`yreviewy/`](yreviewy/) is a Tauri v2 Android companion for reviewing from a phone. It reads a compact, versioned card snapshot through the GitHub Contents API, works from that snapshot offline, and appends reviews to a device-specific mailbox in the vault:

```text
.notes/mobile-snapshot.json
.notes/mobile-reviews/<device-id>.jsonl
```

The SQLite database is never uploaded. Review events have stable IDs and desktop imports are idempotent, so retrying a phone sync is safe. Each phone owns one append-only mailbox file, avoiding Git conflicts between devices. A desktop `sync` imports GitHub review events into FSRS and statistics, then publishes a fresh snapshot containing desktop and mobile progress.

1. Sync the vault once from the PC to create and publish the mobile snapshot:

```bash
cargo run -- --vault ~/Notes sync
```

2. Create a classic GitHub personal access token. Use the `repo` scope for a private vault repository or `public_repo` for a public repository. The phone needs Contents read/write access because it reads the snapshot and writes its review mailbox.

3. Install Android Studio, its SDK/NDK, and the Android Rust targets required by [Tauri mobile prerequisites](https://v2.tauri.app/start/prerequisites/). Then initialize and run the app:

```bash
cd yreviewy
npm install
cargo check --manifest-path src-tauri/Cargo.toml
npm run tauri android init
npm run tauri android dev
```

Use `npm run tauri android build` for an APK/AAB. On first launch, enter `owner/repository`, the branch, and the classic token. The token is kept in Android app-private Rust storage and is never exposed to the web view, snapshot, or review mailbox. Revoke the token from GitHub if the phone is lost.

The normal rhythm is: review offline on either device, sync the phone to upload its mailbox, then run desktop sync to import those events and publish current cards and statistics. A second phone sync receives the refreshed state. The phone groups cards by deck, keeps cards without a deck in a separate No deck group, and offers Force all when you want to repeat a deck before its cards are due.

### ygraphy

`ygraphy/` is a native Rust/wgpu graph for the same vault. Sections are connected by their typed relations. Soft force constraints cluster sections inside notes and notes inside topics, while fitted circles make the `topic -> note -> section` hierarchy visible.

```bash
cd ygraphy
cargo run --release -- --vault ../examples/vault
```

Omit `--vault` to use the vault most recently opened by `yalive`. Drag a section to reposition it, drag the canvas to pan, double-click a section to focus it in the running TUI, use the wheel to zoom, press `Space` to pause the simulation, `F` to fit, and `Esc` to exit.

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
| Pages | `1` dashboard, `2` reviews, `3` relations, `4` statistics, `5` cleanup, `6` options, `7` archived |
| Panel focus | `Shift+h` left, `Shift+l` right, `Shift+j` down, `Shift+k` up |
| Dashboard | `Enter` opens a note/section, `n` creates a note, `/` searches |
| Dashboard | `g` or `b` relations/backlinks, `o` opens URL, `i` opens image |
| Archive | `x` archives the selected note, section, quiz, or deck; `u` restores it on the archived page |
| Review setup | `Enter` opens notes/sections, activates decks, or reviews cards |
| Review setup | `Space` enrolls a section, `r` chooses a deck to review, `n` creates a deck |
| Review setup | `[`/`]` choose active deck, `a` assign card, `x` archive selected item |
| Deck review | `Enter` reviews due cards, `f` force-reviews every card even when not due; deckless cards are grouped under No deck |
| Refresh | `Shift+r` reloads Markdown and SQLite-backed lists immediately |
| Sync | `Ctrl+s` syncs the vault from any page or mode |
| Relations | `j/k` selects in the focused panel, `Enter` follows incoming/outgoing links or opens the center section |
| Cleanup | `Enter` opens an item, `a` assigns a card, `d` deletes an unreferenced image |
| Options | `j/k` selects, `h/l` or arrows changes values, `Space` toggles, `Enter` configures GitHub sync or opens/creates a vault |
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
- The index can be rebuilt from Markdown; review history, decks, deck assignments, and archive state are the non-derived SQLite data worth backing up.
