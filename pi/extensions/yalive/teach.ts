/**
 * Teach note generation.
 *
 * The `teach_save` tool takes a structured representation of a learning note
 * and writes it to a yalive vault as a properly-formed Markdown file. The
 * schema mirrors what the parser expects so that the resulting note indexes
 * cleanly and produces no diagnostics.
 *
 * ## Structure
 *
 * - One note per concept, with multiple sections if the concept warrants it
 * - Topic folder: `<vault>/<topic-slug>/<slug>.md` when `topic` is provided
 * - Frontmatter: id, title, tags, topic, status, created
 * - First section is always H1 with `{#root}` anchor
 * - Sub-sections are H2 with `{#slug}` anchors
 * - Quiz blocks live inside the section they belong to (per `section_id`)
 * - Relations are appended to the root section's body
 * - `video` becomes a section-level `@video` line at the end of the root body
 *
 * ## Validation
 *
 * Quiz blocks are validated against yalive's parser rules before writing:
 *
 * - Cloze: prompt must contain at least one `{{cN::answer}}` marker.
 * - Multiple-choice: ≥2 answers, ≥1 correct, exactly 1 for single mode.
 * - Code-gap: every `{{gap:name}}` in `code` must have a matching entry in
 *   `gaps`, and every entry in `gaps` must be used.
 *
 * Slug collisions are resolved by appending `-2`, `-3`, etc.
 *
 * ## Detection
 *
 * `detectLearnIntent(text)` is a small heuristic that returns true for
 * phrasing like "teach me", "help me understand", "explain X to me", etc.
 * The extension uses it to add a learn-mode hint to the system prompt so
 * the agent knows to consider `teach_save` as a follow-up.
 */

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

export type TeachCardType = "cloze" | "multiple-choice" | "code-gap";

export type TeachRelationType =
	| "related"
	| "outgoing"
	| "contradicts"
	| "example-of"
	| "ingoing"
	| "supersedes";

export interface TeachSection {
	id: string;
	title: string;
	content: string;
}

export interface TeachAnswer {
	id?: string;
	text: string;
	correct: boolean;
}

export interface TeachQuiz {
	id?: string;
	section_id: string;
	type: TeachCardType;
	// Cloze
	prompt?: string;
	// Multiple-choice
	question?: string;
	answers?: TeachAnswer[];
	explanation?: string;
	// Code-gap
	code?: string;
	language?: string;
	gaps?: Record<string, string>;
	// Clip strings (optional) — written as commented YAML lines
	clip?: string;
	prompt_clip?: string;
}

export interface TeachRelation {
	type: TeachRelationType;
	target_uid: string;
}

export interface TeachVideo {
	url: string;
	timestamp?: string;
	label?: string;
}

export interface TeachNote {
	concept: string;
	topic?: string;
	tags?: string[];
	sections: TeachSection[];
	quizzes?: TeachQuiz[];
	relations?: TeachRelation[];
	video?: TeachVideo;
	/** Optional override for the active vault. */
	vault?: string;
}

export interface TeachValidationError {
	field: string;
	message: string;
}

export class TeachValidationFailed extends Error {
	constructor(public readonly errors: TeachValidationError[]) {
		super(
			`teach_save validation failed: ${errors.map((e) => `${e.field}: ${e.message}`).join("; ")}`,
		);
		this.name = "TeachValidationFailed";
	}
}

export interface TeachSaveResult {
	path: string;
	slug: string;
	note_id: string;
	topic_folder: string | null;
	section_count: number;
	quiz_count: number;
	relation_count: number;
	index_status: string;
	diagnostics_count: number;
	diagnostics: Array<Record<string, unknown>>;
}

/**
 * Match the Rust parser's slugify algorithm character-by-character.
 * Lowercase ASCII alphanumerics stay; everything else becomes a single dash.
 * Leading and trailing dashes are stripped.
 */
export function slugify(value: string): string {
	let slug = "";
	let separator = false;
	for (const char of value) {
		if (/[a-zA-Z0-9]/.test(char)) {
			slug += char.toLowerCase();
			separator = false;
		} else if (!separator && slug.length > 0) {
			slug += "-";
			separator = true;
		}
	}
	return slug.replace(/-+$/, "");
}

const VALID_RELATION_TYPES: readonly TeachRelationType[] = [
	"related",
	"outgoing",
	"contradicts",
	"example-of",
	"ingoing",
	"supersedes",
];

const VALID_CARD_TYPES: readonly TeachCardType[] = ["cloze", "multiple-choice", "code-gap"];

/**
 * Validate a complete teach note. Throws TeachValidationFailed on errors.
 */
export function validateTeachNote(note: TeachNote): void {
	const errors: TeachValidationError[] = [];

	if (!note.concept || note.concept.trim().length === 0) {
		errors.push({ field: "concept", message: "concept is required" });
	}

	if (!Array.isArray(note.sections) || note.sections.length === 0) {
		errors.push({ field: "sections", message: "at least one section is required" });
		throw new TeachValidationFailed(errors);
	}

	const sectionIds = new Set<string>();
	note.sections.forEach((section, i) => {
		const prefix = `sections[${i}]`;
		if (!section.id || section.id.trim().length === 0) {
			errors.push({ field: `${prefix}.id`, message: "section id is required" });
		} else {
			const slug = slugify(section.id);
			if (slug.length === 0) {
				errors.push({
					field: `${prefix}.id`,
					message: "section id must contain at least one alphanumeric",
				});
			} else if (section.id !== slug) {
				errors.push({
					field: `${prefix}.id`,
					message: `section id must be slugified (use '${slug}')`,
				});
			}
			if (sectionIds.has(section.id)) {
				errors.push({
					field: `${prefix}.id`,
					message: `duplicate section id '${section.id}'`,
				});
			}
			sectionIds.add(section.id);
		}
		if (!section.title || section.title.trim().length === 0) {
			errors.push({ field: `${prefix}.title`, message: "section title is required" });
		}
		if (section.content === undefined || section.content === null) {
			errors.push({ field: `${prefix}.content`, message: "section content is required" });
		}
	});

	// The first section's id is forced to "root" so it becomes the H1 anchor.
	if (note.sections[0].id !== "root") {
		errors.push({
			field: "sections[0].id",
			message: "first section must be id 'root' (it becomes the H1 anchor)",
		});
	}

	// Validate quizzes
	if (note.quizzes) {
		const quizIds = new Set<string>();
		note.quizzes.forEach((quiz, i) => {
			const prefix = `quizzes[${i}]`;
			errors.push(...validateQuiz(quiz, prefix));

			if (quiz.id) {
				if (quizIds.has(quiz.id)) {
					errors.push({
						field: `${prefix}.id`,
						message: `duplicate quiz id '${quiz.id}' in note`,
					});
				}
				quizIds.add(quiz.id);
			}

			if (!sectionIds.has(quiz.section_id)) {
				errors.push({
					field: `${prefix}.section_id`,
					message: `section_id '${quiz.section_id}' does not match any section`,
				});
			}
		});
	}

	// Validate relations
	if (note.relations) {
		note.relations.forEach((rel, i) => {
			const prefix = `relations[${i}]`;
			if (!VALID_RELATION_TYPES.includes(rel.type)) {
				errors.push({
					field: `${prefix}.type`,
					message: `invalid relation type '${rel.type}', must be one of: ${VALID_RELATION_TYPES.join(", ")}`,
				});
			}
			if (!rel.target_uid || !rel.target_uid.includes("#")) {
				errors.push({
					field: `${prefix}.target_uid`,
					message: "target_uid must be in 'note-id#section-id' format",
				});
			}
		});
	}

	// Validate topic
	if (note.topic !== undefined && note.topic.trim().length === 0) {
		errors.push({ field: "topic", message: "topic must be non-empty if provided" });
	}

	// Validate video
	if (note.video) {
		if (!note.video.url || !note.video.url.startsWith("http")) {
			errors.push({
				field: "video.url",
				message: "video.url must be a URL starting with http(s)://",
			});
		}
	}

	if (errors.length > 0) {
		throw new TeachValidationFailed(errors);
	}
}

function validateQuiz(quiz: TeachQuiz, prefix: string): TeachValidationError[] {
	const errors: TeachValidationError[] = [];

	if (!quiz.section_id) {
		errors.push({ field: `${prefix}.section_id`, message: "section_id is required" });
	}

	if (!VALID_CARD_TYPES.includes(quiz.type)) {
		errors.push({
			field: `${prefix}.type`,
			message: `invalid type '${quiz.type}', must be one of: ${VALID_CARD_TYPES.join(", ")}`,
		});
		return errors;
	}

	switch (quiz.type) {
		case "cloze": {
			if (!quiz.prompt || quiz.prompt.trim().length === 0) {
				errors.push({ field: `${prefix}.prompt`, message: "cloze prompt is required" });
			} else {
				// Match the parser's cloze marker regex: {{cN::answer::hint?}}
				const markerRe = /\{\{c\d+::[^}]+\}\}/;
				if (!markerRe.test(quiz.prompt)) {
					errors.push({
						field: `${prefix}.prompt`,
						message: "cloze prompt must contain at least one {{cN::answer}} marker",
					});
				}
			}
			break;
		}
		case "multiple-choice": {
			if (!quiz.question || quiz.question.trim().length === 0) {
				errors.push({
					field: `${prefix}.question`,
					message: "multiple-choice question is required",
				});
			}
			const answers = quiz.answers ?? [];
			if (answers.length < 2) {
				errors.push({
					field: `${prefix}.answers`,
					message: "multiple-choice needs at least 2 answers",
				});
			}
			const correct = answers.filter((a) => a.correct).length;
			if (correct === 0) {
				errors.push({
					field: `${prefix}.answers`,
					message: "multiple-choice has no correct answer",
				});
			}
			if (correct !== 1) {
				errors.push({
					field: `${prefix}.answers`,
					message: "single-choice mode requires exactly one correct answer",
				});
			}
			const answerIds = new Set<string>();
			answers.forEach((a, i) => {
				if (a.id) {
					if (answerIds.has(a.id)) {
						errors.push({
							field: `${prefix}.answers[${i}].id`,
							message: `duplicate answer id '${a.id}'`,
						});
					}
					answerIds.add(a.id);
				}
			});
			break;
		}
		case "code-gap": {
			if (!quiz.code || quiz.code.trim().length === 0) {
				errors.push({ field: `${prefix}.code`, message: "code-gap code is required" });
			}
			const placeholderRe = /\{\{gap:([a-zA-Z0-9_-]+)\}\}/g;
			const codeGaps = new Set<string>();
			if (quiz.code) {
				let match: RegExpExecArray | null;
				while ((match = placeholderRe.exec(quiz.code)) !== null) {
					codeGaps.add(match[1]);
				}
			}
			const gaps = quiz.gaps ?? {};
			for (const name of codeGaps) {
				if (!(name in gaps)) {
					errors.push({
						field: `${prefix}.code`,
						message: `placeholder '${name}' has no gap definition`,
					});
				}
			}
			for (const name of Object.keys(gaps)) {
				if (!codeGaps.has(name)) {
					errors.push({
						field: `${prefix}.gaps.${name}`,
						message: `gap '${name}' is unused in code`,
					});
				}
			}
			for (const [name, value] of Object.entries(gaps)) {
				if (typeof value !== "string" || value.trim().length === 0) {
					errors.push({
						field: `${prefix}.gaps.${name}`,
						message: `gap '${name}' has empty answer`,
					});
				}
			}
			break;
		}
	}

	return errors;
}

/**
 * Find a unique file path in a directory. Appends -2, -3, etc. on collision.
 */
export function findUniqueFilePath(dir: string, baseSlug: string, ext = "md"): string {
	let candidate = baseSlug;
	let counter = 1;
	while (existsSync(join(dir, `${candidate}.${ext}`))) {
		counter += 1;
		candidate = `${baseSlug}-${counter}`;
	}
	return join(dir, `${candidate}.${ext}`);
}

/**
 * Determine the directory to write to, given the topic. Returns the vault
 * root when no topic is provided.
 */
export function teachDir(vault: string, topic: string | undefined): string {
	if (!topic || topic.trim().length === 0) return vault;
	const topicSlug = slugify(topic);
	if (topicSlug.length === 0) return vault;
	return join(vault, topicSlug);
}

/**
 * Build a YAML double-quoted scalar. Falls back to bare strings when safe.
 */
function yamlScalar(value: string): string {
	const needsQuoting =
		value.length === 0 ||
		/[":#&*?|<>=!%@`{}\[\],]/.test(value) ||
		/^\s/.test(value) ||
		/\s$/.test(value) ||
		/^-?\d/.test(value) ||
		/^(true|false|null|yes|no|on|off)$/i.test(value);
	if (needsQuoting) {
		return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
	}
	return value;
}

/**
 * YAML literal block scalar (`|`) for multi-line content, scalar for single-line.
 */
function yamlLiteral(value: string): string {
	if (value.includes("\n")) {
		const indented = value.split("\n").map((line) => `  ${line}`).join("\n");
		return `|\n${indented}`;
	}
	return yamlScalar(value);
}

function buildFrontmatter(
	noteId: string,
	concept: string,
	topic: string | undefined,
	tags: string[],
): string {
	const today = new Date().toISOString().split("T")[0];
	const lines: string[] = ["---", `id: ${noteId}`, `title: ${yamlScalar(concept)}`];
	if (tags.length > 0) {
		lines.push(`tags: [${tags.map((t) => yamlScalar(t)).join(", ")}]`);
	}
	if (topic) {
		lines.push(`topic: ${yamlScalar(topic)}`);
	}
	lines.push("status: current");
	lines.push(`created: ${today}`);
	lines.push("---");
	return lines.join("\n");
}

function buildQuizBlock(quiz: TeachQuiz, id: string): string {
	const lines: string[] = ["```quiz", `id: ${id}`, `type: ${quiz.type}`];

	if (quiz.type === "cloze") {
		lines.push(`prompt: ${yamlLiteral(quiz.prompt ?? "")}`);
	} else if (quiz.type === "multiple-choice") {
		lines.push(`mode: single`);
		lines.push(`question: ${yamlLiteral(quiz.question ?? "")}`);
		lines.push(`answers:`);
		for (const answer of quiz.answers ?? []) {
			lines.push(`  - text: ${yamlScalar(answer.text)}`);
			lines.push(`    correct: ${answer.correct ? "true" : "false"}`);
		}
		if (quiz.explanation) {
			lines.push(`explanation: ${yamlLiteral(quiz.explanation)}`);
		}
	} else if (quiz.type === "code-gap") {
		lines.push(`language: ${yamlScalar(quiz.language ?? "text")}`);
		if (quiz.prompt) {
			lines.push(`prompt: ${yamlLiteral(quiz.prompt)}`);
		}
		lines.push(`code: ${yamlLiteral(quiz.code ?? "")}`);
		lines.push(`gaps:`);
		for (const [name, value] of Object.entries(quiz.gaps ?? {})) {
			lines.push(`  ${name}: ${yamlScalar(value)}`);
		}
	}

	if (quiz.clip) {
		lines.push(`# clip: ${quiz.clip}`);
	}
	if (quiz.prompt_clip) {
		lines.push(`# prompt_clip: ${quiz.prompt_clip}`);
	}

	lines.push("```");
	return lines.join("\n");
}

function buildRelationLine(rel: TeachRelation): string {
	const prefix = rel.type === "related" ? "" : `${rel.type}:: `;
	return `${prefix}[[${rel.target_uid}]]`;
}

function buildVideoLine(video: TeachVideo): string {
	let line = `@video ${video.url}`;
	if (video.timestamp) line += ` ${video.timestamp}`;
	if (video.label) line += `  ${video.label}`;
	return line;
}

/**
 * Build the full markdown for a teach note. Pure function — no IO.
 */
export function buildTeachMarkdown(note: TeachNote, noteId: string): string {
	// Group content by section id so we can append relations/video/quizzes
	// while keeping the section's authored content intact.
	const sectionContent = new Map<string, string[]>();
	for (const section of note.sections) {
		sectionContent.set(section.id, [section.content?.trim() ?? ""]);
	}

	// Attach video and relations to the root section.
	const rootContent = sectionContent.get("root") ?? [];
	if (note.video) {
		rootContent.push(buildVideoLine(note.video));
	}
	if (note.relations && note.relations.length > 0) {
		for (const rel of note.relations) {
			rootContent.push(buildRelationLine(rel));
		}
	}

	// Attach quizzes to their respective sections, auto-generating IDs when missing.
	const sectionQuizIndex = new Map<string, number>();
	if (note.quizzes) {
		for (const quiz of note.quizzes) {
			const target = sectionContent.get(quiz.section_id);
			if (!target) continue;
			const id =
				quiz.id ??
				`${noteId}-${quiz.section_id}-${quiz.type}-${(sectionQuizIndex.get(quiz.section_id) ?? 0) + 1}`;
			sectionQuizIndex.set(quiz.section_id, (sectionQuizIndex.get(quiz.section_id) ?? 0) + 1);
			target.push(buildQuizBlock(quiz, id));
		}
	}

	// Assemble the markdown.
	const parts: string[] = [];
	parts.push(buildFrontmatter(noteId, note.concept, note.topic, note.tags ?? []));
	parts.push("");

	const rootSection = note.sections[0];
	const rootTitle = rootSection.title?.trim() || note.concept.trim();
	parts.push(`# ${rootTitle} {#root}`);
	parts.push("");
	parts.push(rootContent.join("\n\n"));

	for (let i = 1; i < note.sections.length; i++) {
		const section = note.sections[i];
		const content = sectionContent.get(section.id) ?? [];
		parts.push("");
		parts.push(`## ${section.title} {#${section.id}}`);
		parts.push("");
		parts.push(content.join("\n\n"));
	}

	return parts.join("\n").trim() + "\n";
}

/**
 * Phrase heuristics that signal the user wants to *learn* a concept (as
 * opposed to debugging or asking a quick factual question). Conservative on
 * purpose — false positives are worse than false negatives because the hint
 * nudges the agent to do extra work.
 */
const LEARN_TRIGGERS: readonly RegExp[] = [
	/\bteach me\b/i,
	/\bhelp me (?:understand|learn|figure out)\b/i,
	/\bi (?:want|need|'d like|wanna) to (?:understand|learn|know about)\b/i,
	/\bcan you (?:teach|explain)\b/i,
	/\bcould you (?:teach|explain)\b/i,
	/\bexplain\s+\w+(?:\s+\w+){0,3}\s+to\s+me\b/i,
	/\bexplain\s+(?:how|why|what\s+(?:is|are))\b/i,
	/\bexplain\s+(?:the\s+)?(?:concept|idea|mechanism)\s+of\b/i,
	/\bwalk me through\b/i,
];

export function detectLearnIntent(text: string): boolean {
	if (!text || text.trim().length === 0) return false;
	return LEARN_TRIGGERS.some((re) => re.test(text));
}

/**
 * Build a learn-mode hint for the system prompt. Returns an empty string when
 * the user's prompt doesn't look like learning intent.
 */
export function buildLearnHint(prompt: string): string {
	if (!detectLearnIntent(prompt)) return "";
	return [
		"\n",
		"> [Learn-mode hint] The user appears to want to learn or understand a concept.",
		"> Explain it using the Feynman technique — simple language, key mechanisms, concrete examples, common pitfalls.",
		"> If they want to remember it for spaced repetition, save a structured note to their vault using the `yalive_teach_save` tool with at least one cloze quiz and, where useful, a multiple-choice or code-gap card.",
		"> Skip the save if they're asking a quick factual question or debugging something.",
	].join("\n");
}

/**
 * Write the teach note to disk and re-index. Returns the result without
 * throwing — callers should inspect the diagnostics to decide whether to
 * surface warnings.
 */
export function writeTeachNote(note: TeachNote, vault: string, runYaliveFn: (args: string[]) => { stdout: string; stderr: string; status: number }): TeachSaveResult {
	const baseSlug = slugify(note.concept);
	if (baseSlug.length === 0) {
		throw new TeachValidationFailed([
			{ field: "concept", message: "concept contains no alphanumeric characters" },
		]);
	}

	const dir = teachDir(vault, note.topic);
	mkdirSync(dir, { recursive: true });

	const filePath = findUniqueFilePath(dir, baseSlug);
	const finalSlug = basename(filePath).replace(/\.md$/, "");
	const noteId = finalSlug;

	const markdown = buildTeachMarkdown(note, noteId);
	writeFileSync(filePath, markdown, "utf-8");

	// Rebuild the disposable index so the new note is searchable. The index
	// command is idempotent — unchanged files are skipped.
	const indexResult = runYaliveFn(["index"]);
	const indexStatus = indexResult.stdout.trim() || "(no output)";

	// Diagnostics are fetched separately (the index command does not embed them).
	const diagResult = runYaliveFn(["editor", "diagnostics"]);
	let diagnostics: Array<Record<string, unknown>> = [];
	if (diagResult.status === 0) {
		try {
			const parsed = JSON.parse(diagResult.stdout) as { items?: Array<Record<string, unknown>> };
			const all = Array.isArray(parsed.items) ? parsed.items : [];
			// Filter to the file we just wrote so the agent only sees issues
			// that affect the new note. The relative path is what the parser
			// reports when the vault is the root.
			const relativePath = filePath
				.replace(/\\/g, "/")
				.replace(vault.replace(/\\/g, "/") + "/", "");
			diagnostics = all.filter((d) => {
				const p = typeof d.path === "string" ? d.path : "";
				return p === relativePath || p.endsWith(`/${relativePath}`) || p === finalSlug;
			});
		} catch {
			// Non-JSON diagnostics output — leave the list empty rather than
			// confusing the caller with a parse error.
		}
	}

	return {
		path: filePath,
		slug: finalSlug,
		note_id: noteId,
		topic_folder: note.topic ? dir : null,
		section_count: note.sections.length,
		quiz_count: note.quizzes?.length ?? 0,
		relation_count: note.relations?.length ?? 0,
		index_status: indexStatus,
		diagnostics_count: diagnostics.length,
		diagnostics,
	};
}
