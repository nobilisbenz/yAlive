/**
 * Vault location helpers.
 *
 * The yalive CLI stores the most recently opened vault in a platform-specific
 * config file. We mirror that lookup so the extension and the CLI agree on
 * which vault is "current" without any extra setup.
 */

import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { homedir, platform } from "node:os";
import { isAbsolute, join } from "node:path";
import { spawnSync } from "node:child_process";

export interface VaultLocation {
	path: string;
	source: "env" | "config" | "cwd-notes" | "session" | "explicit";
}

export interface VaultResolution {
	location: VaultLocation | null;
	configDir: string;
}

function configDir(): string {
	if (platform() === "darwin") {
		return join(homedir(), "Library", "Application Support", "dev.yalive.yalive");
	}
	if (platform() === "win32") {
		const appdata = process.env.APPDATA ?? join(homedir(), "AppData", "Roaming");
		return join(appdata, "yalive", "config");
	}
	const xdg = process.env.XDG_CONFIG_HOME ?? join(homedir(), ".config");
	return join(xdg, "yalive");
}

export function getConfigDir(): string {
	return configDir();
}

export function readLastVaultFile(): string | null {
	const path = join(configDir(), "last-vault");
	if (!existsSync(path)) return null;
	try {
		const trimmed = readFileSync(path, "utf-8").trim();
		return trimmed.length > 0 ? trimmed : null;
	} catch {
		return null;
	}
}

export function writeLastVaultFile(vaultPath: string): void {
	const dir = configDir();
	mkdirSync(dir, { recursive: true });
	writeFileSync(join(dir, "last-vault"), vaultPath, "utf-8");
}

export function resolveVault(
	cwd: string,
	options: { envOverride?: string | null; sessionOverride?: string | null } = {},
): VaultLocation | null {
	if (options.sessionOverride && existsSync(options.sessionOverride)) {
		return { path: options.sessionOverride, source: "session" };
	}
	const envVault = options.envOverride ?? process.env.YALIVE_VAULT;
	if (envVault && existsSync(envVault)) {
		return { path: envVault, source: "env" };
	}
	const remembered = readLastVaultFile();
	if (remembered && existsSync(remembered)) {
		return { path: remembered, source: "config" };
	}
	if (existsSync(join(cwd, ".notes"))) {
		return { path: cwd, source: "cwd-notes" };
	}
	return null;
}

export function normalizeVaultPath(input: string): string {
	const trimmed = input.trim();
	if (trimmed === "~" || trimmed.startsWith("~/")) {
		return join(homedir(), trimmed.slice(1));
	}
	if (isAbsolute(trimmed)) {
		return trimmed;
	}
	return join(process.cwd(), trimmed);
}

/**
 * Locate a yalive binary the extension can spawn.
 *
 * Resolution order:
 *   1. `yalive` on PATH (covers installed binaries)
 *   2. `<cwd>/target/release/yalive`
 *   3. `<cwd>/target/debug/yalive`
 *
 * Returns null when nothing is found.
 */
export function findYaliveBinary(cwd: string): string | null {
	const whichResult = spawnSync("which", ["yalive"], { encoding: "utf-8" });
	if (whichResult.status === 0) {
		const found = whichResult.stdout.trim();
		if (found) return found;
	}
	const release = join(cwd, "target", "release", "yalive");
	if (existsSync(release)) return release;
	const debug = join(cwd, "target", "debug", "yalive");
	if (existsSync(debug)) return debug;
	return null;
}

export interface YaliveCommand {
	command: string;
	argsPrefix: string[];
}

/**
 * Resolve how to invoke yalive: either the binary directly, or `cargo run` as
 * a fallback while iterating inside the yalive source tree.
 */
export function resolveYaliveCommand(cwd: string): YaliveCommand | null {
	const bin = findYaliveBinary(cwd);
	if (bin) return { command: bin, argsPrefix: [] };
	if (existsSync(join(cwd, "Cargo.toml"))) {
		return { command: "cargo", argsPrefix: ["run", "--quiet", "--"] };
	}
	return null;
}
