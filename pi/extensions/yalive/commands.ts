/**
 * Slash commands exposed by the extension.
 *
 * Each command is a thin front-end over the same helpers the tools use, but
 * aimed at a human driving the editor directly.
 */

import { existsSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { homedir } from "node:os";
import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import { getCardTemplate, loadCapabilities } from "./capabilities.ts";
import { runYalive, runYaliveJson } from "./runner.ts";
import { normalizeVaultPath, readLastVaultFile, writeLastVaultFile } from "./vault.ts";

interface CommandDeps {
	/** The pi extension handle every command registers against. */
	pi: ExtensionAPI;
	getCwd: () => string;
	getVault: () => string | null;
	setVault: (path: string | null) => void;
}

function expandHome(value: string): string {
	if (value === "~") return homedir();
	if (value.startsWith("~/")) return join(homedir(), value.slice(2));
	return value;
}

async function presentResults(
	ctx: ExtensionCommandContext,
	title: string,
	lines: string[],
): Promise<void> {
	if (lines.length === 0) {
		ctx.ui.notify(`${title}: no results`, "info");
		return;
	}
	await ctx.ui.select(title, lines);
}

function searchLines(query: string, items: Array<Record<string, unknown>>): string[] {
	return items.map((item) => {
		const uid = String(item.uid ?? item.section_uid ?? "");
		const heading = String(item.heading ?? item.heading_path ?? "(untitled)");
		const note = String(item.note_title ?? item.path ?? "");
		return `UID: ${uid} — ${heading}${note ? `  (${note})` : ""}`;
	});
}

function relationsLines(items: Array<Record<string, unknown>>): string[] {
	const incoming = items.filter((r) => r.incoming === true);
	const outgoing = items.filter((r) => r.incoming !== true);
	const lines: string[] = [];
	if (outgoing.length > 0) {
		lines.push("--- outgoing ---");
		lines.push(...outgoing.map(formatRelation));
	}
	if (incoming.length > 0) {
		lines.push("--- incoming ---");
		lines.push(...incoming.map(formatRelation));
	}
	return lines;
}

function formatRelation(r: Record<string, unknown>): string {
	const type = String(r.relation_type ?? "related");
	const target = String(r.target_uid ?? r.target ?? "");
	const heading = r.target_heading ? ` (${String(r.target_heading)})` : "";
	return `${type.padEnd(14)} → ${target}${heading}`;
}

function diagnosticLines(items: Array<Record<string, unknown>>): string[] {
	return items.map((d) => {
		const path = String(d.path ?? "");
		const line = typeof d.line === "number" ? d.line : "?";
		const message = String(d.message ?? "");
		return `${path}:${line}  ${message}`;
	});
}

export function registerCommands({ pi, getCwd, getVault, setVault }: CommandDeps): void {
	// ---------------------------------------------------------------- /yvault
	pi.registerCommand("yvault", {
		description: "Show or change the active yalive vault path. With no argument, shows the resolved vault.",
		handler: async (args, ctx) => {
			const cwd = getCwd();
			const trimmed = args?.trim() ?? "";
			if (!trimmed) {
				const sessionVault = getVault();
				const remembered = readLastVaultFile();
				const envVault = process.env.YALIVE_VAULT;
				const lines: string[] = [];
				lines.push(`session:  ${sessionVault ?? "(unset)"}`);
				lines.push(`env:      ${envVault ?? "(unset)"}`);
				lines.push(`remembered: ${remembered ?? "(none)"}`);
				lines.push(`cwd:      ${cwd}`);
				const choice = await ctx.ui.select("Yalive vault state", [...lines, "(clear session override)", "(set vault to cwd)"]);
				if (!choice) return;
				if (choice.startsWith("session:") && sessionVault) {
					// Re-selecting the active vault is a no-op signal
					return;
				}
				if (choice === "(clear session override)") {
					setVault(null);
					ctx.ui.notify("Session vault cleared", "info");
					return;
				}
				if (choice === "(set vault to cwd)") {
					setVault(cwd);
					ctx.ui.notify(`Session vault set to ${cwd}`, "info");
					return;
				}
				return;
			}

			const resolved = normalizeVaultPath(expandHome(trimmed));
			if (!existsSync(resolved)) {
				const ok = await ctx.ui.confirm(
					"Vault path missing",
					`${resolved} does not exist. Save it as the remembered vault anyway?`,
				);
				if (!ok) return;
			}
			setVault(resolved);
			writeLastVaultFile(resolved);
			ctx.ui.notify(`Vault set to ${resolved}`, "info");
		},
	});

	// ---------------------------------------------------------------- /ysearch
	pi.registerCommand("ysearch", {
		description: "Search every section in the active vault and present the matches in a picker.",
		handler: async (args, ctx) => {
			const cwd = getCwd();
			const vault = getVault();
			const query = args?.trim() ?? "";
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(
					["editor", "sections", query],
					{ cwd, vault },
				);
				const lines = searchLines(query, payload.items);
				await presentResults(ctx, `Sections matching "${query || "*"}"`, lines);
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /yrels
	pi.registerCommand("yrels", {
		description: "Show incoming and outgoing relations for a section UID (e.g. `note-id#section-id`).",
		handler: async (args, ctx) => {
			const uid = args?.trim() ?? "";
			if (!uid || !uid.includes("#")) {
				ctx.ui.notify("Usage: /yrels <note-id>#<section-id>", "warning");
				return;
			}
			const cwd = getCwd();
			const vault = getVault();
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(
					["editor", "relations", uid],
					{ cwd, vault },
				);
				const lines = relationsLines(payload.items);
				if (lines.length === 0) {
					ctx.ui.notify(`No relations for ${uid}`, "info");
					return;
				}
				await ctx.ui.select(`Relations for ${uid}`, lines);
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /ydiag
	pi.registerCommand("ydiag", {
		description: "Run diagnostics and present any errors in a picker.",
		handler: async (_args, ctx) => {
			const cwd = getCwd();
			const vault = getVault();
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(
					["editor", "diagnostics"],
					{ cwd, vault },
				);
				const items = Array.isArray(payload.items) ? payload.items : [];
				const lines = diagnosticLines(items);
				if (items.length === 0) {
					ctx.ui.notify("No diagnostics — vault is clean.", "info");
					return;
				}
				ctx.ui.notify(`${items.length} diagnostic(s)`, "warning");
				await ctx.ui.select(`Diagnostics (${items.length})`, lines);
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /ycard
	pi.registerCommand("ycard", {
		description: "Paste a card template into the editor. Usage: /ycard <cloze|multiple-choice|code-gap>.",
		handler: async (args, ctx) => {
			const cwd = getCwd();
			const vault = getVault();
			const cardType = args?.trim() ?? "";
			if (!cardType) {
				try {
					const caps = loadCapabilities(cwd, vault);
					const labels = caps.card_types.map((c) => `${c.label} (${c.card_type})`);
					const choice = await ctx.ui.select("Card type", labels);
					if (!choice) return;
					const match = choice.match(/\(([^)]+)\)\s*$/);
					if (!match) return;
					const template = getCardTemplate(cwd, vault, match[1]);
					if (template) ctx.ui.setEditorText(template);
					return;
				} catch (err) {
					ctx.ui.notify((err as Error).message, "error");
					return;
				}
			}
			try {
				const template = getCardTemplate(cwd, vault, cardType);
				if (!template) {
					const caps = loadCapabilities(cwd, vault);
					const available = caps.card_types.map((c) => c.card_type).join(", ");
					ctx.ui.notify(`Unknown card type "${cardType}". Available: ${available}`, "warning");
					return;
				}
				ctx.ui.setEditorText(template);
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /yindex
	pi.registerCommand("yindex", {
		description: "Rebuild the disposable SQLite index for the active vault.",
		handler: async (_args, ctx) => {
			const cwd = getCwd();
			const vault = getVault();
			try {
				const result = runYalive(["index"], { cwd, vault });
				if (result.status !== 0) {
					ctx.ui.notify(`yalive index failed: ${result.stderr.trim()}`, "error");
					return;
				}
				ctx.ui.notify(result.stdout.trim() || "indexed", "info");
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /ybuild
	pi.registerCommand("ybuild", {
		description: "Run `cargo build --release` inside the yalive project. Convenience for iterating on the TUI.",
		handler: async (args, ctx) => {
			// `resolveYaliveCommand` used to be called here and its result
			// thrown away, but it was never imported — running /ybuild raised a
			// ReferenceError before it reached the build. The build command does
			// not need it: cargo is invoked directly by manifest path.
			const cwd = getCwd();
			const target = args?.trim().includes("debug") ? "debug" : "release";
			const buildCmd = `cargo build --${target} --manifest-path ${cwd}/Cargo.toml`;
			ctx.ui.notify(`Building yalive (${target})…`, "info");
			await pi.sendUserMessage(`Run the following and report the result:\n\n\`\`\`bash\n${buildCmd}\n\`\`\``);
		},
	});

	// ---------------------------------------------------------------- /ycap
	pi.registerCommand("ycap", {
		description: "Show the supported card types and relation syntax returned by `yalive editor capabilities`.",
		handler: async (_args, ctx) => {
			const cwd = getCwd();
			const vault = getVault();
			try {
				const caps = loadCapabilities(cwd, vault);
				const lines: string[] = [];
				lines.push("--- card types ---");
				for (const c of caps.card_types) lines.push(`${c.card_type}  (${c.label})`);
				lines.push("--- relation types ---");
				for (const r of caps.relation_types) {
					const prefix = r.prefix.length > 0 ? `"${r.prefix}"` : "(default)";
					lines.push(`${r.relation_type.padEnd(14)} ${prefix}`);
				}
				await ctx.ui.select(`yalive capabilities (v${caps.protocol_version})`, lines);
			} catch (err) {
				ctx.ui.notify((err as Error).message, "error");
			}
		},
	});

	// ---------------------------------------------------------------- /yhelp
	pi.registerCommand("yhelp", {
		description: "Show every yalive extension command.",
		handler: async (_args, ctx) => {
			const cmds = pi
				.getCommands()
				.filter((c) => c.name.startsWith("y"))
				.map((c) => `/${c.name}${c.description ? ` — ${c.description}` : ""}`);
			if (cmds.length === 0) {
				ctx.ui.notify("No yalive extension commands registered.", "info");
				return;
			}
			await ctx.ui.select("Yalive extension commands", cmds);
		},
	});

	// ---------------------------------------------------------------- /yteach
	pi.registerCommand("yteach", {
		description: "Start a Feynman-style learning session and save the explanation to the vault with quizzes.",
		handler: async (args, ctx) => {
			const topic = args?.trim() ?? "";
			if (!topic) {
				ctx.ui.notify("Usage: /yteach <topic>", "warning");
				return;
			}
			const vault = getVault();
			if (!vault) {
				ctx.ui.notify(
					"No active vault. Use /yvault <path> to set one before starting a teach session.",
					"warning",
				);
				return;
			}

			ctx.ui.notify(`Teach mode: ${topic}`, "info");

			// Surface a few relevant sections from the vault so the agent can
			// wire up typed relations and avoid duplicating existing notes.
			let existingHint = "";
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(
					["editor", "sections", topic],
					{ cwd: getCwd(), vault },
				);
				const matches = payload.items.slice(0, 5);
				if (matches.length > 0) {
					const lines = matches.map((item) => {
						const uid = String(item.uid ?? "");
						const heading = String(
							(item as { heading_path?: string; heading?: string }).heading_path ??
								item.heading ??
								"(untitled)",
						);
						return `- ${uid}  (${heading})`;
					});
					existingHint = `\n\nAlready in the vault (you may want to add relations to these):\n${lines.join("\n")}`;
				}
			} catch {
				// Best-effort: skip the hint if the search fails.
			}

			const prompt = [
				`Teach me about **${topic}** using the Feynman technique.`,
				``,
				`Plan:`,
				`1. Explain the concept in simple terms (as if to a curious child).`,
				`2. Highlight the key mechanisms, definitions, and pitfalls.`,
				`3. Use code examples where useful.`,
				`4. If the topic is broad, pick the most useful sub-aspect and ask a clarifying question if needed.`,
				``,
				`When you have a full explanation, save it to the vault using the \`yalive_teach_save\` tool:`,
				`- \`concept\`: the title (e.g., "Rust Borrow Checker")`,
				`- \`topic\`: a folder name (e.g., "Programming" -> <vault>/programming/<slug>.md)`,
				`- \`sections\`: 1-3 sections. The first must have id "root" and becomes the H1.`,
				`- \`quizzes\`: at least 1 cloze on the core mechanism. Add a multiple-choice if there are useful comparisons, or a code-gap if there's code.`,
				`- \`relations\`: any outgoing relations to existing sections.`,
				``,
				`After saving, show me what was saved (file path, section/quiz counts) and any diagnostics. If the topic already has notes, consider adding typed relations from the new note to them.`,
				``,
				`Vault: ${vault}${existingHint}`,
			].join("\n");

			await pi.sendUserMessage(prompt);
		},
	});
}


