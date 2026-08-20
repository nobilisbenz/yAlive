/**
 * Relation injection helpers.
 *
 * The `yalive_add_relation` tool reads an existing note file, finds the
 * source section's body in the file, and appends a typed relation line.
 * The parser extracts relations from any section body via regex, so all
 * we need is to put the line in the right place.
 *
 * ## Idempotency
 *
 * Before inserting, we check if the relation already exists in the section
 * body. If it does, we return `added: false, existing: true` without
 * mutating the file. This lets the agent call the tool defensively without
 * worrying about duplicate relations.
 *
 * ## Section boundary detection
 *
 * We use a simple regex-based heading parser. The parser uses pulldown-cmark
 * to extract headings, but for our purposes (finding a section's line range)
 * a regex is sufficient. Edge cases (code blocks containing `#`) are
 * acceptable: the worst that happens is a slightly wrong insertion point,
 * which `yalive_diagnostics` will flag.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { slugify } from "./teach.ts";

export interface SectionLocation {
	headingLine: number;
	bodyStartLine: number;
	bodyEndLine: number;
	nextHeadingLine: number | null;
}

const HEADING_RE = /^(#{1,6})\s+(.+?)(?:\s*\{#([A-Za-z0-9_-]+)\})?\s*$/;

/**
 * Find the line range of a section in a file. The section is identified by
 * its anchor id (`{#id}`) or, when no anchor is present, by the slugified
 * heading text.
 *
 * Returns `null` when the section cannot be found.
 */
export function findSectionInFile(fileContent: string, sectionId: string): SectionLocation | null {
	const lines = fileContent.split("\n");
	const headings: Array<{ level: number; id: string; line: number }> = [];

	for (let i = 0; i < lines.length; i++) {
		const match = lines[i].match(HEADING_RE);
		if (match) {
			const level = match[1].length;
			const heading = match[2].trim();
			const inlineId = match[3];
			const id = inlineId ?? slugify(heading);
			headings.push({ level, id, line: i + 1 });
		}
	}

	const index = headings.findIndex((h) => h.id === sectionId);
	if (index === -1) return null;

	const start = headings[index];
	const nextSameOrHigher = headings.findIndex((h, i) => i > index && h.level <= start.level);
	const nextHeadingLine = nextSameOrHigher !== -1 ? headings[nextSameOrHigher].line : null;
	const bodyEndLine = nextHeadingLine !== null ? nextHeadingLine - 1 : lines.length;

	return {
		headingLine: start.line,
		bodyStartLine: start.line + 1,
		bodyEndLine,
		nextHeadingLine,
	};
}

/**
 * Build a relation line in the format the parser expects.
 *
 *   outgoing:: [[rust-basics#references]]
 *   contradicts:: [[rust-ownership#string-slicing]]
 *   [[rust-basics#references]]           (default: related)
 */
export function formatRelationLine(relationType: string, targetUid: string): string {
	const prefix = relationType === "related" ? "" : `${relationType}:: `;
	return `${prefix}[[${targetUid}]]`;
}

function escapeRegex(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Check whether a relation already exists in the body text.
 *
 * For typed relations (`outgoing`, `contradicts`, etc.), we match the exact
 * prefix and target. For `related`, we match either bare `[[target]]` or
 * any typed `<type>:: [[target]]` — the two are synonymous in the parser.
 */
export function hasRelation(
	bodyText: string,
	relationType: string,
	targetUid: string,
): boolean {
	const escapedTarget = escapeRegex(targetUid);
	if (relationType === "related") {
		// Either bare [[target]] or any typed flavour — covers the case where
		// the agent already added an `outgoing::` line for the same target.
		const anyTypeRe = new RegExp(
			`(?:(?:outgoing|contradicts|example-of|ingoing|supersedes)::\\s*)?\\[\\[${escapedTarget}(?:\\|[^\\]]+)?\\]\\]`,
			"m",
		);
		return anyTypeRe.test(bodyText);
	}
	const typedRe = new RegExp(
		`${relationType}::\\s*\\[\\[${escapedTarget}(?:\\|[^\\]]+)?\\]\\]`,
		"m",
	);
	return typedRe.test(bodyText);
}

/**
 * Extract the body text of a section (excluding the heading line itself).
 */
export function extractSectionBody(fileContent: string, section: SectionLocation): string {
	const lines = fileContent.split("\n");
	return lines.slice(section.bodyStartLine - 1, section.bodyEndLine).join("\n");
}

/**
 * Append a relation line to a section's body. The new line is inserted just
 * before the next same-or-higher heading (or at end-of-file for the last
 * section), so it lands inside the section body and the parser picks it up.
 *
 * If the previous line is not blank, a blank line is inserted before the
 * relation so the result reads cleanly alongside other content blocks.
 */
export function appendRelationToSection(
	fileContent: string,
	section: SectionLocation,
	relationLine: string,
): string {
	const lines = fileContent.split("\n");
	// section.nextHeadingLine is 1-indexed; splice at that - 1 (0-indexed) to
	// push the next heading down by one line.
	const insertIndex = section.nextHeadingLine !== null ? section.nextHeadingLine - 1 : lines.length;
	const prevIsBlank = insertIndex > 0 && lines[insertIndex - 1].trim() === "";
	if (prevIsBlank) {
		lines.splice(insertIndex, 0, relationLine);
	} else {
		lines.splice(insertIndex, 0, "", relationLine);
	}
	return lines.join("\n");
}

/**
 * High-level relation write: read the file, locate the section, append the
 * relation (skipping if it already exists), write back.
 */
export interface AddRelationOptions {
	filePath: string;
	sectionId: string;
	relationType: string;
	targetUid: string;
}

export interface AddRelationResult {
	added: boolean;
	existing: boolean;
	relationLine: string;
}

export function addRelationToFile(opts: AddRelationOptions): AddRelationResult {
	const fileContent = readFileSync(opts.filePath, "utf-8");
	const section = findSectionInFile(fileContent, opts.sectionId);
	if (!section) {
		throw new Error(`section '${opts.sectionId}' not found in ${opts.filePath}`);
	}

	const relationLine = formatRelationLine(opts.relationType, opts.targetUid);
	const bodyText = extractSectionBody(fileContent, section);

	if (hasRelation(bodyText, opts.relationType, opts.targetUid)) {
		return { added: false, existing: true, relationLine };
	}

	const newContent = appendRelationToSection(fileContent, section, relationLine);
	writeFileSync(opts.filePath, newContent, "utf-8");
	return { added: true, existing: false, relationLine };
}
