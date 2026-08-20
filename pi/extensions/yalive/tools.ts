/**
 * Custom tools that wrap the yalive CLI.
 *
 * These give the LLM a way to query the live vault without shell-parsing
 * Markdown itself: search sections, walk typed relations, surface parser
 * diagnostics, fetch card templates, and rebuild the disposable index. Every
 * tool picks up the active vault from the session state and falls back to the
 * platform-config "last-vault" file when one is set.
 */

import { existsSync } from "node:fs";
import { join } from "node:path";
import { Type } from "typebox";
import type { AgentToolResult, ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { runYalive, runYaliveJson, YaliveCommandError as YaliveCommandErrorCtor } from "./runner.ts";
import { readLastVaultFile } from "./vault.ts";
import {
	type TeachNote,
	TeachValidationFailed,
	validateTeachNote,
	writeTeachNote,
} from "./teach.ts";
import { addRelationToFile } from "./relations-edit.ts";

interface VaultToolOpts {
	vault?: string | undefined;
}

function normalizeVault(opts: VaultToolOpts, fallback: string | null): string | null {
	if (opts.vault && opts.vault.trim().length > 0) return opts.vault.trim();
	return fallback;
}

/**
 * Report a tool failure to the model.
 *
 * pi only marks a tool call as failed when `execute` throws — from its own
 * docs: "Returning a value never sets the error flag regardless of what
 * properties you include in the return object." Every tool here used to
 * `return { …, isError: true }`, a property that is not part of the result
 * type, so the model was handed each failure as a successful result whose
 * payload happened to read like an error message.
 */
function fail(message: string): never {
	throw new Error(message);
}

/** Turn a thrown value into the sentence the model should read. */
function describeError(err: unknown): string {
	if (err instanceof YaliveCommandErrorCtor) {
		return `yalive command failed:\n${err.message}`;
	}
	if ((err as { name?: string })?.name === "YaliveUnavailableError") {
		return "yalive binary not found. Build the project with `cargo build --release` or install yalive on PATH.";
	}
	return `Unexpected error: ${(err as Error).message ?? String(err)}`;
}

/**
 * The payload every tool hands back on success.
 *
 * `details` is deliberately widened: pi infers a tool's result type from what
 * its handler returns, and while the failure branch still returned an object
 * with a different `details` shape, every handler failed to typecheck — which
 * is why none of them were type-checked at all.
 */
function successResult(payload: Record<string, unknown>): AgentToolResult<Record<string, unknown>> {
	return {
		content: [{ type: "text" as const, text: JSON.stringify(payload, null, 2) }],
		details: payload,
	};
}

/** Report a caught exception as a tool failure. */
function failFrom(err: unknown): never {
	fail(describeError(err));
}

export interface RegisterToolsArgs {
	pi: ExtensionAPI;
	getCwd: () => string;
	getVault: () => string | null;
}

export function registerTools({ pi, getCwd, getVault }: RegisterToolsArgs): void {
	// ---------------------------------------------------------------- yalive_search
	pi.registerTool({
		name: "yalive_search",
		label: "Yalive search",
		description:
			"Full-text search across every indexed section in the active yalive vault. Returns matching sections with their note title, heading, path, line number, and stable section UID.",
		parameters: Type.Object({
			query: Type.String({ description: "Full-text query; empty string lists every section." }),
			limit: Type.Optional(
				Type.Number({
					description: "Maximum number of results to return (1-100).",
					minimum: 1,
					maximum: 100,
				}),
			),
			vault: Type.Optional(
				Type.String({ description: "Override the active vault path for this call." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			const args = ["editor", "sections", params.query];
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: unknown[] }>(args, { cwd, vault });
				const items = Array.isArray(payload.items) ? payload.items : [];
				const limited =
					typeof params.limit === "number" && params.limit > 0 ? items.slice(0, params.limit) : items;
				return successResult({
					query: params.query,
					vault,
					count: limited.length,
					total: items.length,
					sections: limited,
				});
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_relations
	pi.registerTool({
		name: "yalive_relations",
		label: "Yalive relations",
		description:
			"List incoming and outgoing typed relations for a single section UID (format `note-id#section-id`). Returns one row per relation with relation type, target UID, target heading, and direction.",
		parameters: Type.Object({
			section_uid: Type.String({
				description: "Section UID, e.g. `rust-ownership#borrowing`. Discoverable via `yalive_search`.",
			}),
			vault: Type.Optional(
				Type.String({ description: "Override the active vault path for this call." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: unknown[] }>(
					["editor", "relations", params.section_uid],
					{ cwd, vault },
				);
				const relations = payload.items as { incoming?: boolean }[];
				const incoming = relations.filter((relation) => relation.incoming === true);
				const outgoing = relations.filter((relation) => relation.incoming !== true);
				return successResult({
					section_uid: params.section_uid,
					vault,
					count: payload.items.length,
					incoming,
					outgoing,
				});
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_diagnostics
	pi.registerTool({
		name: "yalive_diagnostics",
		label: "Yalive diagnostics",
		description:
			"Run the yalive parser across the active vault and return every diagnostic (duplicate section IDs, malformed quiz blocks, unresolved links, etc.) with file path and line number.",
		parameters: Type.Object({
			vault: Type.Optional(
				Type.String({ description: "Override the active vault path for this call." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(
					["editor", "diagnostics"],
					{ cwd, vault },
				);
				const items = Array.isArray(payload.items) ? payload.items : [];
				const byFile = new Map<string, number>();
				for (const d of items) {
					const path = typeof d.path === "string" ? d.path : "(unknown)";
					byFile.set(path, (byFile.get(path) ?? 0) + 1);
				}
				return successResult({
					vault,
					count: items.length,
					files_affected: byFile.size,
					by_file: Object.fromEntries(byFile),
					diagnostics: items,
				});
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_capabilities
	pi.registerTool({
		name: "yalive_capabilities",
		label: "Yalive capabilities",
		description:
			"Fetch the supported card types and relation syntax from yalive itself (the `editor capabilities` endpoint). Templates are sourced from the Rust capability registry so they stay accurate when card types evolve.",
		parameters: Type.Object({}),
		async execute() {
			const cwd = getCwd();
			const vault = getVault();
			try {
				const payload = runYaliveJson<{
					protocol_version: number;
					card_types: Array<{ card_type: string; label: string; template: string }>;
					relation_types: Array<{ relation_type: string; prefix: string }>;
				}>(["editor", "capabilities"], { cwd, vault });
				return successResult(payload);
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_index
	pi.registerTool({
		name: "yalive_index",
		label: "Yalive index",
		description:
			"Rebuild the disposable SQLite index for the active vault. Returns the summary line that `yalive index` prints (indexed / unchanged / removed / failed / diagnostics counts). Use this after editing notes, or when stale-section problems surface.",
		parameters: Type.Object({
			vault: Type.Optional(
				Type.String({ description: "Override the active vault path for this call." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			try {
				const result = runYalive(["index"], { cwd, vault });
				if (result.status !== 0) {
					fail(`yalive index failed:\n${result.stderr || result.stdout}`);
				}
				return successResult({ vault, summary: result.stdout.trim() });
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_videos
	pi.registerTool({
		name: "yalive_videos",
		label: "Yalive videos",
		description:
			"List every `@video` action in the active vault, with URL, timestamp, label, owning note title, section UID, path, and line number. Optionally restrict to a single section UID. Lets the LLM build a video picker without parsing Markdown itself.",
		parameters: Type.Object({
			section_uid: Type.Optional(
				Type.String({ description: "Restrict to one section UID; omit to list every video in the vault." }),
			),
			vault: Type.Optional(
				Type.String({ description: "Override the active vault path for this call." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			const args = ["editor", "videos"];
			if (params.section_uid) args.push(params.section_uid);
			try {
				const payload = runYaliveJson<{ protocol_version: number; items: Array<Record<string, unknown>> }>(args, {
					cwd,
					vault,
				});
				return successResult({
					vault,
					section_uid: params.section_uid ?? null,
					count: payload.items.length,
					videos: payload.items,
				});
			} catch (err) {
				// `editor videos` is documented but may not exist on every build — degrade gracefully.
				const message =
					err instanceof YaliveCommandErrorCtor
						? `yalive editor videos is not supported by this build: ${err.message}`
						: (err as Error).message;
				fail(message);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_vault
	pi.registerTool({
		name: "yalive_vault",
		label: "Yalive vault",
		description:
			"Show which vault is currently active for this session. The vault is resolved, in order: explicit `vault` argument, session override, $YALIVE_VAULT, the platform `last-vault` file, or a `.notes/` directory in the cwd.",
		parameters: Type.Object({
			reveal: Type.Optional(
				Type.Boolean({ description: "If true, include the path verbatim even when it does not exist." }),
			),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const sessionVault = getVault();
			const remembered = readLastVaultFile();
			return successResult({
				session_vault: sessionVault,
				session_vault_exists: sessionVault ? true : null,
				env_vault: process.env.YALIVE_VAULT ?? null,
				remembered_vault: remembered,
				cwd,
				reveal: params.reveal === true,
			});
		},
	});

	// ---------------------------------------------------------------- yalive_teach_save
	pi.registerTool({
		name: "yalive_teach_save",
		label: "Yalive teach save",
		description: [
			"Save a structured learning note to the active yalive vault. The note is written as a properly-formed Markdown file with frontmatter, sections, and inline quiz blocks. Each quiz block is validated against yalive's parser rules before writing, then the disposable index is rebuilt and diagnostics are filtered to the new file.",
			"",
			"Use this when the user wants to remember a concept for spaced repetition. The agent teaches the concept first, then calls this tool with a structured payload.",
			"",
			"Schema:",
			"- concept (required): human-readable title (e.g., \"Rust Borrow Checker\")",
			"- topic (optional): folder name (e.g., \"Programming\" -> <vault>/programming/<slug>.md)",
			"- tags (optional): list of tags for the frontmatter",
			"- sections (required): ordered, at least one. First section's id must be \"root\" (becomes the H1). Each: { id (slug), title, content (Markdown body) }",
			"- quizzes (optional): each attaches to a section via section_id. Each must have a valid type-specific payload:",
			"  - cloze: { prompt with {{cN::answer}} markers }",
			"  - multiple-choice: { question, answers: [{text, correct}], explanation? }",
			"  - code-gap: { code with {{gap:name}} placeholders, gaps: {name: replacement} }",
			"- relations (optional): each { type (related|outgoing|contradicts|example-of|ingoing|supersedes), target_uid (note-id#section-id) }",
			"- video (optional): { url, timestamp?, label? } -> written as @video at the root section",
			"",
			"After calling, show the user what was saved: file path, section/quiz/relation counts, and any diagnostics. If diagnostics > 0, summarize the issues and offer to fix them.",
		].join("\n"),
		parameters: Type.Object({
			concept: Type.String({ description: "Human-readable title for the note." }),
			topic: Type.Optional(
				Type.String({
					description:
						"Folder name under the vault. The slugified form is used as the directory. Omit for a flat note.",
				}),
			),
			tags: Type.Optional(Type.Array(Type.String(), { description: "Frontmatter tags." })),
			sections: Type.Array(
				Type.Object({
					id: Type.String({ description: "Section slug. The first section must be 'root'." }),
					title: Type.String({ description: "Heading text." }),
					content: Type.String({ description: "Markdown body of the section." }),
				}),
				{ description: "At least one section. The first is the H1 (root)." },
			),
			quizzes: Type.Optional(
				Type.Array(
					Type.Object({
						id: Type.Optional(Type.String({ description: "Quiz ID. Auto-generated if missing." })),
						section_id: Type.String({ description: "Section to attach to." }),
						type: Type.String({ description: "cloze | multiple-choice | code-gap." }),
						prompt: Type.Optional(Type.String()),
						question: Type.Optional(Type.String()),
						answers: Type.Optional(
							Type.Array(
								Type.Object({
									text: Type.String(),
									correct: Type.Boolean(),
								}),
							),
						),
						explanation: Type.Optional(Type.String()),
						code: Type.Optional(Type.String()),
						language: Type.Optional(Type.String()),
						gaps: Type.Optional(Type.Record(Type.String(), Type.String())),
						clip: Type.Optional(Type.String()),
						prompt_clip: Type.Optional(Type.String()),
					}),
				),
			),
			relations: Type.Optional(
				Type.Array(
					Type.Object({
						type: Type.String({
							description:
								"related | outgoing | contradicts | example-of | ingoing | supersedes",
						}),
						target_uid: Type.String({ description: "Target section UID, e.g. 'note-id#section-id'." }),
					}),
				),
			),
			video: Type.Optional(
				Type.Object({
					url: Type.String(),
					timestamp: Type.Optional(Type.String()),
					label: Type.Optional(Type.String()),
				}),
			),
			vault: Type.Optional(Type.String({ description: "Override the active vault path." })),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			if (!vault) {
				fail("No active vault. Use /yvault <path> to set one before saving teach notes.");
			}

			// Drop the optional `vault` override before validation so the
			// teach.ts layer doesn't need to know about the parameter.
			const { vault: _vaultOverride, ...noteInput } = params;
			// The TypeBox schema uses `string` for the quiz `type` field; the
			// TeachNote type narrows it to a literal union. The runtime
			// validation accepts either, so an explicit cast is safe.
			const note = noteInput as TeachNote;

			try {
				validateTeachNote(note);
			} catch (err) {
				if (err instanceof TeachValidationFailed) {
					const summary = err.errors
						.map((e) => `  - ${e.field}: ${e.message}`)
						.join("\n");
					fail(`teach_save validation failed:\n${summary}`);
				}
				throw err;
			}

			const runYaliveArgs = (args: string[]) => {
				const result = runYalive(args, { cwd, vault });
				return { stdout: result.stdout, stderr: result.stderr, status: result.status };
			};

			try {
				const result = writeTeachNote(note, vault, runYaliveArgs);
				return successResult({
					path: result.path,
					slug: result.slug,
					note_id: result.note_id,
					topic_folder: result.topic_folder,
					section_count: result.section_count,
					quiz_count: result.quiz_count,
					relation_count: result.relation_count,
					index_status: result.index_status,
					diagnostics_count: result.diagnostics_count,
					diagnostics: result.diagnostics,
				});
			} catch (err) {
				failFrom(err);
			}
		},
	});

	// ---------------------------------------------------------------- yalive_add_relation
	pi.registerTool({
		name: "yalive_add_relation",
		label: "Yalive add relation",
		description: [
			"Add a typed relation line to an existing section. The line is inserted just before the next same-or-higher heading (or at end-of-file for the last section), so the parser picks it up on the next index.",
			"",
			"Idempotent: if the same (source, target, type) already exists, the tool returns added: false, existing: true and does not mutate the file.",
			"",
			"Use this to wire up the graph between sections you discover while writing — e.g. when a /yteach explanation references an existing note, add an outgoing relation from the new note to the existing one.",
		].join("\n"),
		parameters: Type.Object({
			source_uid: Type.String({
				description: "Source section UID, e.g. 'rust-ownership#borrowing'. The relation line is added to this section's body.",
			}),
			target_uid: Type.String({
				description: "Target section UID, e.g. 'rust-basics#references'. Use yalive_search to discover UIDs.",
			}),
			relation_type: Type.String({
				description:
					"Relation type: related (default for bare [[target]]), outgoing, contradicts, example-of, ingoing, supersedes.",
			}),
			vault: Type.Optional(Type.String({ description: "Override the active vault path." })),
		}),
		async execute(_id, params) {
			const cwd = getCwd();
			const vault = normalizeVault(params, getVault());
			if (!vault) {
				fail("No active vault. Use /yvault <path> to set one before adding relations.");
			}

			const validTypes = ["related", "outgoing", "contradicts", "example-of", "ingoing", "supersedes"];
			if (!validTypes.includes(params.relation_type)) {
				fail(`Invalid relation_type '${params.relation_type}'. Must be one of: ${validTypes.join(", ")}`);
			}

			const sourceParts = params.source_uid.split("#");
			if (sourceParts.length !== 2 || !sourceParts[0] || !sourceParts[1]) {
				fail(`source_uid must be in 'note-id#section-id' format (got '${params.source_uid}').`);
			}
			const [noteId, sectionId] = sourceParts;

			// Locate the source section via the live index. We search by note
			// id (FTS5 will match it in the section body) and then filter by
			// exact UID — empty inputs are accepted so a bare note id returns
			// every section in that note.
			let sourceItem: { uid?: string; path?: string } | undefined;
			try {
				const search = runYaliveJson<{ items: Array<Record<string, unknown>> }>(
					["editor", "sections", noteId],
					{ cwd, vault },
				);
				sourceItem = search.items.find((item) => item.uid === params.source_uid);
			} catch (err) {
				failFrom(err);
			}

			if (!sourceItem) {
				fail(`Source section '${params.source_uid}' not found in vault. Use yalive_search to discover UIDs.`);
			}

			const sourcePathRaw = typeof sourceItem.path === "string" ? sourceItem.path : "";
			if (!sourcePathRaw) {
				fail(`Source section '${params.source_uid}' has no file path in the index.`);
			}

			// Resolve to absolute path. The index returns paths relative to the
			// vault root, so we join with the vault.
			const absolutePath = join(vault, sourcePathRaw);
			if (!existsSync(absolutePath)) {
				fail(`Source file '${sourcePathRaw}' not found at ${absolutePath}. The index may be stale — run yalive_index.`);
			}

			// Validate the target exists. We do not block on a missing target —
			// yalive accepts broken links — but we surface the warning so the
			// agent can decide whether to fix it.
			let targetExists = false;
			try {
				const targetNoteId = params.target_uid.split("#")[0] ?? "";
				const targetSearch = runYaliveJson<{ items: Array<Record<string, unknown>> }>(
					["editor", "sections", targetNoteId],
					{ cwd, vault },
				);
				targetExists = targetSearch.items.some((item) => item.uid === params.target_uid);
			} catch {
				targetExists = false;
			}

			// Edit the file. addRelationToFile handles the idempotency check.
			let outcome: { added: boolean; existing: boolean; relationLine: string };
			try {
				outcome = addRelationToFile({
					filePath: absolutePath,
					sectionId,
					relationType: params.relation_type,
					targetUid: params.target_uid,
				});
			} catch (err) {
				failFrom(err);
			}

			// Re-index and fetch diagnostics so the agent sees the consequences.
			let indexStatus = "";
			let diagnosticsCount = 0;
			try {
				const idx = runYalive(["index"], { cwd, vault });
				indexStatus = idx.stdout.trim();
			} catch {
				indexStatus = "(index run failed)";
			}
			try {
				const diag = runYaliveJson<{ items: Array<Record<string, unknown>> }>(
					["editor", "diagnostics"],
					{ cwd, vault },
				);
				const affected = (diag.items ?? []).filter((d) => d.path === sourcePathRaw);
				diagnosticsCount = affected.length;
			} catch {
				// Ignore — diagnostics are best-effort.
			}

			return successResult({
				added: outcome.added,
				existing: outcome.existing,
				source_path: sourcePathRaw,
				source_section: sectionId,
				relation_line: outcome.relationLine,
				target_uid: params.target_uid,
				target_exists: targetExists,
				relation_type: params.relation_type,
				index_status: indexStatus,
				diagnostics_count: diagnosticsCount,
			});
		},
	});
}

