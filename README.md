# yalive

`yalive` is a section-first Markdown knowledge vault and spaced-repetition TUI. Markdown owns the knowledge; a disposable SQLite database provides FTS5 search, graph edges, diagnostics, and FSRS review state.

## Apps

| | App | Download | Purpose |
| --- | --- | --- | --- |
| <img src="assets/icons/yalive.svg" width="64" alt="Black circle with a blue center" /> | **yalive** | [Linux x86_64](https://github.com/nobilisbenz/yAlive/releases/latest/download/yalive-linux-x86_64.tar.gz) | Write, search, connect, and review from the terminal. |
| <img src="assets/icons/ygraphy.svg" width="64" alt="Black circle with a red center" /> | **yGraphy** | [Linux x86_64](https://github.com/nobilisbenz/yGraphy/releases/latest/download/ygraphy-linux-x86_64.tar.gz) | Explore the vault as an interactive native graph. |
| <img src="assets/icons/yreviewy.svg" width="64" alt="Black circle with a lime center" /> | **yReviewy** | [Android ARM64](https://github.com/nobilisbenz/yReviewy/releases/latest/download/yreviewy-android-arm64.apk) | Review cards offline from an Android phone. |
| <img src="assets/icons/yclippy.svg" width="64" alt="Black circle with an amber center" /> | **yClippy** | [Linux x86_64](https://github.com/nobilisbenz/yClippy/releases/latest/download/yclippy-linux-amd64.deb), [Android ARM64](https://github.com/nobilisbenz/yClippy/releases/latest/download/yclippy-android-arm64.apk) | Watch, trim, and name moments in YouTube videos. The video surface of the vault. |

Downloads are published in each app's GitHub repository. Every release also includes `SHA256SUMS`.

The four share one palette, one spacing scale, and one set of easing curves,
generated from `assets/design/tokens.json` into each app's own language. Each
owns exactly one accent — the hue at the centre of its icon. See
[`DESIGN.md`](DESIGN.md).

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

The Options page — `Ctrl+k`, then "Options" — includes the complete setup flow: authenticate with GitHub CLI, enter the repository URL, and select Sync now. The palette also offers each of those three steps directly. Yalive does not accept or store classic access tokens. Use either:

- `git@github.com:owner/vault.git` with an SSH key configured in GitHub.
- `https://github.com/owner/vault.git` after the Options authentication action, or after running `gh auth login` and `gh auth setup-git`.

Sync commits local changes, fetches and integrates the remote branch, and pushes. Conflicts stop without overwriting either side. Use a standalone vault directory, not a directory nested inside another Git repository. The disposable SQLite index and graph IPC files are automatically ignored; Markdown, images, configuration, and portable review exports are synced.

### Android Reviewer

[`yReviewy/`](yReviewy/) is a Tauri v2 Android companion for reviewing from a phone. It reads a compact, versioned card snapshot through the GitHub Contents API, works from that snapshot offline, and appends reviews to a device-specific mailbox in the vault:

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
cd yReviewy
npm install
cargo check --manifest-path src-tauri/Cargo.toml
npm run tauri android init
npm run tauri android dev
```

Use `npm run tauri android build` for an APK/AAB. On first launch, enter `owner/repository`, the branch, and the classic token. The token is kept in Android app-private Rust storage and is never exposed to the web view, snapshot, or review mailbox. Revoke the token from GitHub if the phone is lost.

### Publishing Releases

Pushing a semantic version tag such as `v0.1.0` in an app's repository builds that app and publishes its GitHub release. The yReviewy repository requires these Actions secrets for Android signing: `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, and `ANDROID_KEY_PASSWORD`. Store the keystore as a single-line Base64 value.

The normal rhythm is: review offline on either device, sync the phone to upload its mailbox, then run desktop sync to import those events and publish current cards and statistics. A second phone sync receives the refreshed state. The phone groups cards by deck, keeps cards without a deck in a separate No deck group, and offers Force all when you want to repeat a deck before its cards are due.

### yGraphy

`yGraphy/` is a native Rust/wgpu graph for the same vault. Sections are connected by their typed relations. Soft force constraints cluster sections inside notes and notes inside topics, while fitted circles make the `topic -> note -> section` hierarchy visible.

```bash
cd yGraphy
cargo run --release -- --vault ../examples/vault
```

Omit `--vault` to use the vault most recently opened by `yalive`. Drag a section to reposition it, drag the canvas to pan, double-click a section to focus it in the running TUI, use the wheel to zoom, press `Space` to pause the simulation, `F` to fit, and `Esc` to exit.

### yClippy

[`yClippy/`](yClippy/) is the video surface of the vault — a Tauri 2 + Svelte 5 desktop and Android app for watching, trimming, and naming moments in YouTube videos. A single line of Markdown is the whole contract:

```markdown
@video https://www.youtube.com/watch?v=dQw4w9WgXcQ 06:54  Chapter on borrowing
```

The line is indexable by `yalive`, searchable by `yGraphy`, and replayable from anywhere. yClippy speaks the line out via the `play` subcommand:

```bash
yclippy play https://www.youtube.com/watch?v=dQw4w9WgXcQ --at 414
yclippy list --json | jq '.items[] | {title, video_id}'
yclippy add https://youtu.be/dQw4w9WgXcQ --folder "Rust"
```

`list` is headless — it prints JSON and exits without starting the GUI, so
pickers can shell out to it. `play` forwards to an already-running instance.

Every surface resolves a player the same way — configured, then `yclippy`, then
`mpv`, then `xdg-open` with the timestamp rebuilt into the URL — so installing
yClippy is enough to make it the player everywhere, with no configuration to
edit. Brain Dock (`yy`) reads `[openers] video` from `config/brain.toml`, yalive
reads `player` from `.notes/config.toml`, and the Neovim plugin reads `player`
from its `setup{}`; all three fall back through the same chain. The Neovim plugin
exposes `:YalivePlay`, `:YaliveLibrary`, `:YaliveInsertClip`, and `:YaliveVideos`
(the older `:YClippy*` names still work). The TUI binds `v` on the Library page
to play the `@video` on the selected section. The phone answers the same intent
through a `yclippy://play?v=…&t=…` deep link registered in the manifest.

The library is a local SQLite database. It syncs into the vault repository:

```
.notes/yclippy/library.json            canonical merged state, written by compaction
.notes/yclippy/devices/<device>.jsonl  one file per device, one writer, ever
```

No device ever writes another device's file, so two phones editing offline
cannot conflict. Sync runs pull → merge → push → compact, in that order. Folders
and clips are identified by `uid` and videos by their YouTube id; local rowids
never travel, and parent and folder links are carried as uids. Conflicts resolve
per record by `(updated_at, last_writer)` — the device id breaks ties so every
device picks the same winner and the fleet converges. Compaction rewrites
`library.json` against the SHA it read, so a concurrent compactor retries rather
than clobbering, and it is desktop-only.

One limitation worth knowing: `updated_at` is a wall clock, so a device with a
badly wrong clock will win conflicts it should not. Timestamps are never used to
decide *whether* to apply an op, only which of two versions of one record wins.

Machine-readable editor commands are also available. They are versioned JSON so editor plugins do not need to read the disposable SQLite schema:

```bash
yalive --vault ~/Notes editor capabilities
yalive --vault ~/Notes editor sections "borrowing"
yalive --vault ~/Notes editor relations rust-ownership#borrowing
yalive --vault ~/Notes editor videos            # every @video in the vault
yalive --vault ~/Notes editor videos rust#own   # just this section's
yalive --vault ~/Notes editor diagnostics
```

`editor videos` returns `{url, seconds, label, note_title, section_uid, path, line}`
per `@video` action, so a plugin never has to parse Markdown to build a picker.

## Keys

The footer always lists the keys that work where you are standing, so this table
is a reference rather than something to memorise. `?` opens the full list for the
current page.

| Where | Keys |
| --- | --- |
| Tabs | `1` Library, `2` Review, `3` Relations, `4` Stats |
| Commands | `Ctrl+k` opens the palette: Clean, Options, Archived, sync, vault switching, and the one-off actions |
| Everywhere | `j`/`k` or arrows move, `Shift+h/j/k/l` moves focus between panes, `/` searches, `Shift+r` re-reads the vault, `Ctrl+s` syncs, `?` shows every key, `q` quits |
| Library | `Enter` opens a note or section, `n` creates a note, `g` jumps to its relations, `x` archives it |
| Library | `v` plays the section's clip, `o` opens its URL, `i` opens its image — each offered only when the section actually carries one |
| Review | `r` starts a session, `Space` enrols a section, `a` assigns a card to the active deck, `[`/`]` change the active deck, `n` creates a deck, `x` archives |
| Deck chooser | `Enter` reviews due cards, `f` forces every card even when not due; deckless cards are grouped under No deck |
| Relations | `j`/`k` selects in the focused pane, `Enter` follows a link or opens the middle section |
| Clean | `Enter` opens an item, `a` assigns a card, `d` deletes an unreferenced image, `x` archives |
| Options | `j`/`k` selects, `h`/`l` or arrows change a value, `Enter` runs an action |
| Search | type to search, arrows navigate, `Enter` opens, `Esc` returns |
| Review session | `Space` reveals a cloze or section; on a multiple-choice card `j`/`k` move, `Space` selects, `Enter` submits |
| Review session | code-gap cards take typed text, `Tab` changes gap, `Enter` checks |
| Review session | `v` plays this card's clip, `Esc` ends the session |
| Rating | `1` Again, `2` Hard, `3` Good, `4` Easy |

Clean, Options, and Archived are reached from `Ctrl+k` rather than from a tab.
The four tabs are what the vault is *for* — writing, reviewing, connecting, and
measuring; the rest is maintenance, and `Esc` returns from it to the Library.

`Enter` on a note or section opens `editor` from `.notes/config.toml`, then
`$VISUAL`, `$EDITOR`, and finally `nvim`. The TUI suspends and returns to the
same section afterwards.

`v` plays the selected section's `@video` in the resolved player, falling back to
the first YouTube URL in the section body.

## Editors and agents

### Neovim

A Neovim plugin is included in [`nvim/`](nvim/README.md). It discovers card templates and relation syntax from `yalive editor capabilities`, so adding a card type to the Rust capability registry makes it available without changing Lua. It provides fzf-backed card creation, section search, incoming/outgoing relation editing, relation navigation, automatic indexing, diagnostics, and the video commands.

### pi

[`pi/`](pi/README.md) is a project-local extension for the pi coding agent,
registered through `.pi/settings.json`. It gives the agent tools that query the
live index — search, relations, diagnostics, capabilities, videos — instead of
grepping Markdown, plus slash commands and a system prompt that stays in sync
with the Rust capability registry. Run `npm run check` in
`pi/extensions/yalive/` after editing it.

Both speak the same versioned `yalive editor …` JSON protocol and both refuse a
protocol version they do not understand, so a bump surfaces as a clear error
rather than as quietly wrong answers.

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

ingoing:: [[rust-basics#references]]
```

Quiz blocks are fenced `quiz` blocks containing YAML. Supported `type` values are `cloze`, `multiple-choice`, and `code-gap`. See [`examples/vault/rust-ownership.md`](examples/vault/rust-ownership.md) for complete examples.

### Clips as answers

Any quiz block can carry a moment in a video, which plays inside the card during
review — in the TUI with `v`, and on the phone as a tappable tile in yReviewy:

```yaml
clip: https://youtu.be/dQw4w9WgXcQ 06:54-07:20  The borrow checker bit
prompt_clip: https://youtu.be/dQw4w9WgXcQ 11:00
```

`clip:` is the evidence and appears only after the answer is revealed.
`prompt_clip:` is the stimulus and appears with the question — use it when the
video *is* the question. The end of a range is optional; so is the label.

A quiz with no `clip:` inherits its section's own `@video` line, so the common
case needs no extra syntax:

```markdown
## Borrowing {#borrow}

@video https://youtu.be/dQw4w9WgXcQ 06:54  Chapter on borrowing

```quiz
id: borrow-1
type: cloze
prompt: A {{c1::mutable}} borrow is exclusive for its whole lifetime.
```
```

Inheritance only ever fills the answer side; putting a video beside the question
is always a deliberate choice. yClippy's "clip: line" clipboard preset emits this
line directly, so marking a range and pasting it into a note is the whole loop.

## Configuration

Optional `<vault>/.notes/config.toml`:

```toml
editor = "nvim"
player = ["yclippy", "play", "{url}", "--at", "{seconds}"]
desired_retention = 0.90
new_cards_per_day = 20
max_reviews_per_day = 200
review_order = "due"
bury_siblings = true
reindex_interval_ms = 1000
```

`player` is the argv template `v` uses on the Library page to launch the `@video`
action of the selected section. It shares its placeholder shape with yy's
`[openers]`, so one template works in both. A template without `{seconds}` — the
default, `["xdg-open", "{url}"]` — gets the timestamp rebuilt into the URL, so a
machine without yClippy installed still lands at the right moment.

Review submissions are not stored. Ratings, correctness, response time, FSRS memory state, and scheduling intervals are retained. Export review history periodically with `export-reviews`; the default output is `.notes/reviews.jsonl`.

## Guarantees

- Notes, quiz definitions, links, tags, and images remain ordinary Markdown files.
- Search results and review cards target stable sections, not only files.
- Moving a file or renaming a heading preserves identity when its IDs stay unchanged.
- Reindexing preserves review state for unchanged card UIDs.
- The index can be rebuilt from Markdown; review history, decks, deck assignments, and archive state are the non-derived SQLite data worth backing up.
