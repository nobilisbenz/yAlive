# `pi/` — pi extension for yalive

A project-local pi extension that makes the agent maximally effective when
working on the [yalive](../../README.md) Rust vault.

The extension lives in `pi/extensions/yalive/` and is registered through
[`.pi/settings.json`](../.pi/settings.json). It auto-discovers on every pi
startup as long as the project is trusted.

## What it gives you

### Tools the LLM can call

| Tool | What it does |
| --- | --- |
| `yalive_search` | FTS5 section search across the active vault |
| `yalive_relations` | Incoming + outgoing typed relations for a section UID |
| `yalive_diagnostics` | Parser diagnostics with file/line rollups |
| `yalive_capabilities` | Card templates + relation syntax from the live Rust registry |
| `yalive_index` | Rebuild the disposable SQLite index |
| `yalive_videos` | Every `@video` action with section UID, timestamp, label |
| `yalive_vault` | Show which vault is currently active and where it came from |
| `yalive_teach_save` | Save a structured learning note (concept + sections + quizzes + relations) to the vault |
| `yalive_add_relation` | Append a typed relation line to an existing section (idempotent) |

All tools accept an optional `vault` override. With no override, they use the
session vault (resolved on startup).

### Slash commands

| Command | Description |
| --- | --- |
| `/yvault [path]` | Show or change the active vault path |
| `/ysearch <query>` | Pick sections from a search result |
| `/yrels <uid>` | Show relations for `note-id#section-id` |
| `/ydiag` | Run diagnostics and present them in a picker |
| `/ycard <type>` | Paste a card template (`cloze`, `multiple-choice`, `code-gap`) into the editor |
| `/yindex` | Rebuild the disposable SQLite index |
| `/ybuild [debug\|release]` | Ask the agent to `cargo build` the project |
| `/ycap` | Show card types + relation prefixes |
| `/yteach <topic>` | Start a Feynman-style learning session and save the explanation to the vault with quizzes |
| `/yhelp` | List every yalive extension command |

### System prompt augmentation

`before_agent_start` injects:

- The yalive project description (architecture, CLI surface, note format)
- The current vault path (or a hint to set one)
- Live card templates + relation syntax from `yalive editor capabilities`

The capability snippet is fetched from the Rust source on every session start
(then cached for 60s), so the LLM sees exactly the templates yalive itself
accepts — no hand-maintained drift.

### Learn-mode hint

When the user's prompt looks like a learning request (`teach me …`,
`help me understand …`, `explain X to me`, `explain how/why …`, `walk me
through …`, `I want to learn …`), a small hint is appended to the system
prompt nudging the agent to consider `yalive_teach_save` after the
explanation. The trigger is conservative — it does not fire on debugging
or quick factual questions — so /yteach remains the explicit entry point
when you want the agent to teach and save without a phrasing match.

### Vault resolution

The vault path is resolved, in order, on every `session_start`:

1. Session override (set via `/yvault <path>`)
2. `$YALIVE_VAULT`
3. `<config-dir>/last-vault` — yalive's own remembered path
   - Linux: `$XDG_CONFIG_HOME/yalive` or `~/.config/yalive`
   - macOS: `~/Library/Application Support/dev.yalive.yalive`
   - Windows: `%APPDATA%\yalive\config`
4. `cwd` (when it contains `.notes/`)

Use `/yvault` to change the vault for the current session. To make it
stick across sessions, `/yvault` also writes to the platform `last-vault`
file so the CLI sees the same choice next time you launch it.

### Bash integration

When the LLM (or you) runs a bash command that contains `yalive`, the
extension prepends the active vault to the shell so the CLI picks up the
right index. The format mirrors yalive's own `--vault` flag:

```bash
yalive_vault="/path/to/notes"
yalive editor sections borrowing
```

## Setup

The extension has one transitive runtime dependency (`typebox`). After
pulling the repo for the first time:

```bash
cd pi/extensions/yalive
npm install
```

`npm install` also pulls in TypeScript, so the extension can be type-checked
before you load it:

```bash
npm run check
```

This is worth running after any edit. It is what caught, among other things, a
`/ybuild` handler calling a function it never imported, `ctx.sendUserMessage`
(which lives on the extension handle, not the command context), and eleven tool
failure paths that returned `isError: true` — a property pi does not read, so
every one of those failures reached the model looking like a success.

If `npm install` is skipped, pi will fail to load the extension with a
`Cannot find module 'typebox'` error from jiti.

## How the extension is wired in

Pi auto-discovers extensions in `.pi/extensions/`, but the source lives in
`pi/extensions/yalive/` so the extension is a normal, visible directory. The
[`.pi/settings.json`](../.pi/settings.json) bridges the two:

```json
{ "extensions": ["../pi/extensions/yalive"] }
```

The leading `../` backs out of `.pi/` so pi resolves the path to
`<project>/pi/extensions/yalive/`. An absolute path works too, but the
relative form keeps the project portable.

## Verifying it loads

From the project root:

```bash
pi --reload
```

The startup header should list `yalive-extension`. The footer should show the
active vault and the resolved `yalive` binary path. Try:

```
/yhelp
```

…to see the full command list.

## Layout

```
pi/
├── README.md                       # this file
└── extensions/
    └── yalive/
        ├── index.ts                # entry point, registration, events
        ├── tools.ts                # registerTool calls
        ├── commands.ts             # registerCommand calls
        ├── vault.ts                # platform-aware vault discovery
        ├── runner.ts               # yalive CLI runner + typed errors
        ├── capabilities.ts         # cached capabilities loader
        ├── teach.ts                # teach note generation + validation
        ├── relations-edit.ts       # relation injection helpers
        └── package.json            # typebox + pi-coding-agent deps
```

## Trust

Because the extension ships with `.pi/settings.json`, the first `pi` run in
this directory will prompt you to trust the project. The trust decision
covers all `pi/` resources, including the extension, system-prompt
augmentation, and any future skills or prompt templates you add.
