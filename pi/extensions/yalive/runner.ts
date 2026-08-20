/**
 * Thin wrapper around the yalive CLI.
 *
 * All command execution goes through `runYalive` so that every tool, slash
 * command, and prompt snippet gets consistent error handling and the same
 * vault override.
 */

import { spawnSync, type SpawnSyncOptionsWithStringEncoding } from "node:child_process";
import { resolveYaliveCommand, type YaliveCommand } from "./vault.ts";

export interface RunResult {
	stdout: string;
	stderr: string;
	status: number;
}

export interface RunOptions {
	cwd: string;
	vault: string | null;
	signal?: AbortSignal;
	timeoutMs?: number;
}

export class YaliveUnavailableError extends Error {
	constructor() {
		super(
			"yalive binary not found. Install yalive on PATH, build `target/release/yalive`, or run from a yalive checkout with a Cargo.toml.",
		);
		this.name = "YaliveUnavailableError";
	}
}

export class YaliveCommandError extends Error {
	constructor(public readonly result: RunResult, message?: string) {
		super(message ?? `yalive exited with status ${result.status}: ${result.stderr.trim() || "<no stderr>"}`);
		this.name = "YaliveCommandError";
	}
}

function buildArgs(cmd: YaliveCommand, vault: string | null, args: string[]): string[] {
	const base = vault ? ["--vault", vault, ...args] : args;
	return [...cmd.argsPrefix, ...base];
}

/**
 * Run a yalive subcommand synchronously.
 *
 * Use this from tool handlers and slash commands; results are returned as raw
 * strings plus status. Callers parse stdout as needed (most subcommands print
 * JSON to stdout).
 */
export function runYalive(args: string[], options: RunOptions): RunResult {
	const cmd = resolveYaliveCommand(options.cwd);
	if (!cmd) throw new YaliveUnavailableError();

	const spawnOpts: SpawnSyncOptionsWithStringEncoding = {
		cwd: options.cwd,
		encoding: "utf-8",
		maxBuffer: 32 * 1024 * 1024,
	};
	if (options.timeoutMs) spawnOpts.timeout = options.timeoutMs;

	const result = spawnSync(cmd.command, buildArgs(cmd, options.vault, args), spawnOpts);
	return {
		stdout: result.stdout ?? "",
		stderr: result.stderr ?? "",
		status: result.status ?? -1,
	};
}

/**
 * The editor protocol this extension understands.
 *
 * Every `yalive editor …` response carries a `protocol_version`. The Neovim
 * plugin has always refused a version it does not know; this extension used to
 * read whatever came back and hope the field names had not moved, which would
 * turn a protocol bump into silently wrong answers rather than a clear error.
 */
export const EDITOR_PROTOCOL_VERSION = 1;

/**
 * Run a yalive subcommand and return parsed JSON from stdout.
 *
 * Throws when the binary is missing, exits non-zero, prints invalid JSON, or
 * answers in a protocol version this extension does not understand.
 */
export function runYaliveJson<T = unknown>(args: string[], options: RunOptions): T {
	const result = runYalive(args, options);
	if (result.status !== 0) {
		throw new YaliveCommandError(result, `yalive ${args.join(" ")} exited with status ${result.status}`);
	}
	const trimmed = result.stdout.trim();
	if (!trimmed) throw new YaliveCommandError(result, `yalive ${args.join(" ")} produced no output`);

	let parsed: unknown;
	try {
		parsed = JSON.parse(trimmed);
	} catch (err) {
		throw new YaliveCommandError(
			result,
			`yalive ${args.join(" ")} produced non-JSON output: ${(err as Error).message}`,
		);
	}

	const version = (parsed as { protocol_version?: unknown } | null)?.protocol_version;
	if (version !== undefined && version !== EDITOR_PROTOCOL_VERSION) {
		throw new YaliveCommandError(
			result,
			`yalive ${args.join(" ")} answered with editor protocol ${String(version)}, but this extension speaks ${EDITOR_PROTOCOL_VERSION}. Update the extension.`,
		);
	}
	return parsed as T;
}

export function runYaliveOrThrow(args: string[], options: RunOptions): RunResult {
	const result = runYalive(args, options);
	if (result.status !== 0) {
		throw new YaliveCommandError(result);
	}
	return result;
}
