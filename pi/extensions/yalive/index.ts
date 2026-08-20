/**
 * Pi extension tailored for working on the yalive Rust vault.
 *
 * Wires together:
 *   - tools/        : custom tools the LLM can call (search, relations, diagnostics, …)
 *   - commands/     : slash commands for the human
 *   - vault/        : platform-aware vault path resolution
 *   - runner/       : thin yalive CLI runner with shared error handling
 *   - capabilities/ : cached `editor capabilities` loader
 *
 * The extension runs both inside the yalive source tree (development) and
 * outside it (when paired with a separate vault). It does its own vault
 * discovery rather than relying on the user to pass `--vault` to every
 * command, and surfaces the active vault in the status bar.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { clearCapabilityCache, buildCapabilitySnippet } from "./capabilities.ts";
import { registerCommands } from "./commands.ts";
import {
	findYaliveBinary,
	getConfigDir,
	readLastVaultFile,
	resolveVault,
} from "./vault.ts";
import { registerTools } from "./tools.ts";
import { YaliveUnavailableError } from "./runner.ts";
import { buildLearnHint } from "./teach.ts";

interface SessionVaultState {
	vault: string | null;
	source: "env" | "config" | "cwd-notes" | "session" | "explicit" | null;
}

/**
 * Stable project description used in the system prompt. Stays in sync with
 * README.md so the LLM has the same mental model a human reading the README
 * would have.
 */
const PROJECT_DESCRIPTION = `
You are working on **yalive**, a section-first Markdown knowledge vault and
spaced-repetition TUI written in Rust.

### Architecture

- \`src/parser.rs\` — pulldown-cmark walker; produces \`ParsedNote\` /\`ParsedSection\` rows. Owns relation regex, action regex, quiz block parsing.
- \`src/db.rs\` — rusqlite + FTS5 wrapper; \`Database::open\` /\`index_vault\` /\`search\` /\`relations\` /\`diagnostics\` /\`actions_for\` /\`export_reviews\`.
- \`src/model.rs\` — pure data types (no IO). \`QuizDefinition\` is the single source of truth for card content; \`card_capabilities\` /\`relation_capabilities\` drive the editor protocol and the templates.
- \`src/app/\` — the ratatui TUI, split by concern:
  - \`mod.rs\` — \`App\` state, the event loop, key dispatch, and vault actions. Terminal-suspending work (editor, \`gh auth\`) is queued on \`App::pending\` and run by the event loop, so key handling itself is terminal-free and testable.
  - \`keymap.rs\` — the single binding table. The which-key footer is rendered from it and dispatch resolves through it, so an advertised key always works.
  - \`ui/\` — one module per page (\`library\`, \`review\`, \`relations\`, \`stats\`, \`clean\`, \`options\`, \`archived\`, \`search\`) plus \`widgets.rs\` and the chrome in \`ui/mod.rs\`.
  - \`theme.rs\` — every colour decision. One accent (\`#168bff\`) plus two greys; body text stays the terminal's own foreground.
  - \`palette.rs\` — the \`Ctrl+K\` command list.
  - \`util.rs\` — pure helpers (\`fit\`, \`pad\`, \`slugify\`, \`display_markdown\`, \`matches_gap\`, orphan-image scanning).

  Navigation is four tabs — Library, Review, Relations, Stats, on \`1\`–\`4\` — with Clean, Options, Archived, and the one-off actions behind \`Ctrl+K\`. Do not reintroduce a seven-tab row.
- \`src/sync.rs\` — pull-merge-push via \`git2\`; never accepts classic GitHub tokens.
- \`src/graph.rs\` — soft-force layout for \`yGraphy\` IPC, exported as JSON.
- \`src/player.rs\` — argv-template renderer for the \`player\` config (\`yclippy play …\`-style invocations).
- \`src/search.rs\` — ranked FTS5 query with status-aware ranking.
- \`src/config.rs\` — \`<vault>/.notes/config.toml\` loader.

### CLI surface

\`\`\`
yalive [--vault PATH] [SUBCOMMAND]
  index            Rebuild changed parts of the disposable SQLite index
  diagnostics      Print parser and link/card diagnostics
  export-reviews   Export review history as JSON Lines
  sync [--repo URL]   Pull-merge-push the vault with Git
  editor capabilities       Card types + relation syntax as JSON
  editor sections [Q]      FTS search of every indexed section
  editor relations <uid>   Incoming + outgoing typed relations
  editor diagnostics       Diagnostics as JSON
\`\`\`

### Note format

\`\`\`markdown
---
id: rust-ownership
title: Rust Ownership
tags: [rust, memory]
topic: Programming
pinned: true
status: current
---

# Rust Ownership {#root}
## Borrowing {#borrowing}

outgoing:: [[rust-basics#references]]
contradicts:: [[rust-ownership#string-slicing]]

@video https://www.youtube.com/watch?v=… 06:54  Chapter on borrowing

\`\`\`quiz
id: borrow-1
type: cloze
prompt: A {{c1::mutable}} borrow is exclusive for its whole lifetime.
\`\`\`
\`\`\`

Front-matter is the only source of identity; sections are addressed by
\`{#stable-id}\` (slugs as fallback). Quiz blocks live in fenced \`\`\`quiz\`\`\`
fences and are pure YAML; the parser validates them before indexing. \`@video\` /
\`@file\` / \`@app\` / \`@project\` /\`@url\` lines are anchored to line start so
mid-sentence mentions don't accidentally register as actions.

### Identity rules

- Section identity is the \`{#id}\` — moving or renaming the file is fine as
  long as the ID stays. \`supersedes::\` demotes the section it points at
  without deleting history.
- Reindexing preserves FSRS review state for unchanged card UIDs. Don't
  shuffle card IDs casually.
- The disposable SQLite database is the only non-derived artefact worth
  backing up alongside Markdown.
`.trim();

export default function yaliveExtension(pi: ExtensionAPI): void {
	const state: SessionVaultState = { vault: null, source: null };

	const getCwd = () => process.cwd();
	const getVault = () => state.vault;
	const setVault = (path: string | null) => {
		state.vault = path;
		state.source = path ? "session" : null;
		// Status widgets are updated on the next session_start or command run;
		// commands that change the vault re-trigger the picker flow, which is
		// already loud about the new path.
	};

	// ---------------------------------------------------------------- session_start
	pi.on("session_start", async (_event, ctx) => {
		const resolved = resolveVault(ctx.cwd, { sessionOverride: state.vault });
		state.vault = resolved?.path ?? null;
		state.source = resolved?.source ?? null;

		clearCapabilityCache();

		const binary = findYaliveBinary(ctx.cwd);
		const binaryLabel = binary ? humanBinaryPath(binary) : "(none found)";
		const vaultLabel = state.vault ? humanBinaryPath(state.vault) : "(none)";
		const configDir = getConfigDir();
		const remembered = readLastVaultFile();

		if (ctx.hasUI) {
			ctx.ui.setStatus(
				"yalive",
				`${ctx.ui.theme.fg("dim", "yalive: ")}${ctx.ui.theme.fg("accent", vaultLabel)}`,
			);
			ctx.ui.setStatus(
				"yalive-bin",
				`${ctx.ui.theme.fg("dim", "bin: ")}${ctx.ui.theme.fg("muted", binaryLabel)}`,
			);

			if (!binary) {
				ctx.ui.notify(
					"yalive binary not found — `cargo build --release` or add yalive to PATH to enable tools.",
					"warning",
				);
			} else if (!state.vault) {
				ctx.ui.notify(
					`No active vault. Run \`/yvault <path>\` (or set $YALIVE_VAULT, or open yalive once so it writes ${configDir}/last-vault).`,
					"info",
				);
			} else if (remembered && remembered !== state.vault) {
				ctx.ui.notify(`Vault: ${state.vault} (last used: ${remembered})`, "info");
			}
		}
	});

	// ---------------------------------------------------------------- resources_discover
	// We don't ship our own skills/prompts/themes, but we could — leaving the
	// hook here so adding them later doesn't require a code change.

	// ---------------------------------------------------------------- before_agent_start
	pi.on("before_agent_start", async (event, ctx) => {
		const cwd = ctx.cwd;
		const vault = state.vault;
		let capSnippet = "";
		try {
			capSnippet = buildCapabilitySnippet(cwd, vault);
		} catch (err) {
			capSnippet = `\n\n> _yalive capabilities unavailable: ${(err as Error).message}_\n`;
		}

		const vaultLine = vault
			? `Active vault: \`${vault}\`. Use \`/yvault\` to change it.`
			: `No active vault. Run \`/yvault <path>\` or set \`$YALIVE_VAULT\` to enable vault-aware tools.`;

		// Phrasing hint: when the user's prompt looks like a learning request,
		// nudge the agent to consider `yalive_teach_save` as a follow-up. The
		// hint is conservative — it only fires on explicit "teach me" / "help
		// me understand" / "explain X to me" phrasing, not on debugging or
		// quick factual questions.
		const learnHint = buildLearnHint(event.prompt);

		const append = `
## yalive project conventions

${PROJECT_DESCRIPTION}

## Vault context

${vaultLine}

${capSnippet}${learnHint}

## Working in this project

- Run \`yalive editor capabilities\` (or the \`yalive_capabilities\` tool) before guessing at card or relation syntax — the templates come from the Rust registry.
- Prefer the dedicated tools (\`yalive_search\`, \`yalive_relations\`, \`yalive_diagnostics\`) over \`grep\` /\`rg\` — they hit the live FTS5 index and return stable section UIDs.
- Use \`yalive_add_relation\` to wire up typed relations between existing sections (idempotent — duplicates are skipped). Useful after a teach session to link the new note into the graph.
- To capture a learning session as a structured note, use \`yalive_teach_save\`. The full schema is in the tool's description.
- For TUI-side changes, edit the module under \`src/app/\` that owns the concern — a page's layout lives in \`src/app/ui/<page>.rs\`, a key in \`src/app/keymap.rs\`, a colour in \`src/app/theme.rs\` — then rerun \`cargo run -- --vault <path>\` (or \`/ybuild\` then \`cargo run\`).
- Never hardcode a colour in a page module; add it to \`theme.rs\`. Never add a key without a \`keymap.rs\` entry, or the footer will not advertise it.
- \`cargo test screens -- --ignored --nocapture\` prints every page, overlay, and review phase as text. Use it to check a layout change without launching the TUI.
- The vault for development can be a temp directory with hand-written notes.
- Diagnostics must be clean before merging a parser change — \`yalive_diagnostics\` and \`/ydiag\` are the canonical checks.
`.trim();

		return {
			systemPrompt: `${event.systemPrompt}\n\n${append}`,
		};
	});

	// ---------------------------------------------------------------- tool_call (bash guard)
	pi.on("tool_call", (event) => {
		if (event.toolName !== "bash") return undefined;
		const cmd = (event.input.command as string | undefined) ?? "";
		if (!cmd.includes("yalive")) return undefined;
		// Surface the active vault to the LLM by appending `--vault` when the
		// command doesn't already specify one. We mutate `event.input` so
		// subsequent handlers (and the actual bash invocation) see it.
		if (state.vault && !/\s--vault\s/.test(cmd) && !/\s-v\s+\S+/.test(cmd)) {
			event.input.command = `yalive_vault="${state.vault}"\n${cmd}`;
		}
		return undefined;
	});

	// ---------------------------------------------------------------- tool_result (diagnostics nudge)
	pi.on("tool_result", (event) => {
		if (event.toolName !== "yalive_diagnostics") return undefined;
		const text = JSON.stringify(event.details ?? {});
		const match = text.match(/"count":\s*(\d+)/);
		const count = match?.[1] ? Number.parseInt(match[1], 10) : 0;
		if (!Number.isFinite(count) || count === 0) return undefined;
		return {
			content: [
				...event.content,
				{
					type: "text" as const,
					text: `\n\n> ${count} diagnostic(s) found. Use \`/ydiag\` to inspect, or read the affected files directly.`,
				},
			],
		};
	});

	// ---------------------------------------------------------------- register tools + commands
	registerTools({ pi, getCwd, getVault });
	registerCommands({ pi, getCwd, getVault, setVault });
}

/**
 * Optional: render `bash` calls that invoke yalive compactly.
 *
 * Currently a no-op — the default bash renderer already shows the full
 * command and exit code, which is what we want for an audit trail. Kept as
 * a hook so a future revision can swap in a custom renderer without
 * touching the registration logic.
 */

/**
 * Convert an absolute path into a short, terminal-friendly label.
 * `/home/nabi/Dev/projects/yalive/target/release/yalive` → `…/release/yalive`
 * `/home/nabi/Notes` → `~/Notes`
 */
function humanBinaryPath(path: string): string {
	const home = process.env.HOME ?? "";
	if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
	const parts = path.split("/");
	if (parts.length <= 3) return path;
	return `…/${parts.slice(-2).join("/")}`;
}

// Reference YaliveUnavailableError so tree-shaking keeps the import live for
// consumers that may want to catch it explicitly.
export { YaliveUnavailableError };
