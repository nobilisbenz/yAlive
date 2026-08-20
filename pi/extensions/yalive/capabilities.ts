/**
 * Cache for `yalive editor capabilities`.
 *
 * The capability payload is small and only changes when card or relation
 * templates change in the Rust source. We cache it briefly to avoid spawning
 * the binary on every prompt, but always re-fetch on /reload.
 */

import { runYaliveJson } from "./runner.ts";

export interface CardCapability {
	card_type: string;
	label: string;
	template: string;
}

export interface RelationCapability {
	relation_type: string;
	prefix: string;
}

export interface Capabilities {
	protocol_version: number;
	card_types: CardCapability[];
	relation_types: RelationCapability[];
}

interface CacheEntry {
	caps: Capabilities;
	at: number;
}

const TTL_MS = 60_000;
let cache: CacheEntry | null = null;

export function loadCapabilities(cwd: string, vault: string | null, force = false): Capabilities {
	const now = Date.now();
	if (!force && cache && now - cache.at < TTL_MS) return cache.caps;
	const caps = runYaliveJson<Capabilities>(["editor", "capabilities"], { cwd, vault });
	cache = { caps, at: now };
	return caps;
}

export function clearCapabilityCache(): void {
	cache = null;
}

export function getCardTemplate(cwd: string, vault: string | null, cardType: string): string | null {
	const caps = loadCapabilities(cwd, vault);
	const match = caps.card_types.find((c) => c.card_type === cardType);
	return match ? match.template : null;
}

/**
 * Build a markdown snippet describing every supported card and relation
 * syntax. Used to seed the system prompt with concrete, copy-pasteable
 * templates rather than hand-rolled approximations.
 */
export function buildCapabilitySnippet(cwd: string, vault: string | null): string {
	const caps = loadCapabilities(cwd, vault);

	const cardLines = caps.card_types
		.map((c) => `- **${c.label}** (\`${c.card_type}\`) — template:\n\n\`\`\`quiz\n${c.template.trim()}\n\`\`\``)
		.join("\n\n");

	const relLines = caps.relation_types
		.map((r) => {
			const prefix = r.prefix.length > 0 ? r.prefix : "(default `related`)";
			return `- \`${r.relation_type}\` — prefix \`${prefix}\``;
		})
		.join("\n");

	return `### Card syntax

Yalive recognises three card types. Each lives inside a fenced \`\`\`quiz\`\`\`
block in Markdown and is indexable, reviewable, and exportable:

${cardLines}

The \`clip:\` line (commented in the templates above) is optional: it attaches a
video moment to the card — shown with the question as a stimulus, on reveal as
evidence. Cards without their own \`clip:\` inherit the section's \`@video\`
line.

### Relation syntax

Relations are typed wikilinks in section bodies. The prefix selects the
relation type; bare \`[[target]]\` defaults to \`related\`:

${relLines}

\`\`\`markdown
## Borrowing {#borrowing}
outgoing:: [[rust-basics#references]]
contradicts:: [[rust-ownership#string-slicing]]
\`\`\`

Use \`supersedes::\` to mark the section this one replaces (a ranking signal
that demotes abandoned workflows).`;
}
