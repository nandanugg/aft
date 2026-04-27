import { execSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { getAftBinaryCacheDir, getAftBinaryName } from "./paths.js";

function normalizeVersion(output: string): string | null {
  const trimmed = output.trim();
  if (!trimmed) return null;
  return trimmed.replace(/^aft\s+/, "");
}

/**
 * Probe `aft --version` from a prioritized list of candidate paths:
 *   1. Versioned cache for the given plugin version (if any)
 *   2. PATH (via `which`/`where`)
 *
 * Returns the first successfully reported version, or null if nothing
 * resolves. Errors and missing files are swallowed — callers get a signal,
 * not an exception.
 */
export function probeBinaryVersion(preferredVersion?: string): string | null {
  const candidates: string[] = [];
  if (preferredVersion) {
    const tag = preferredVersion.startsWith("v") ? preferredVersion : `v${preferredVersion}`;
    candidates.push(join(getAftBinaryCacheDir(), tag, getAftBinaryName()));
  }

  try {
    const lookup = process.platform === "win32" ? "where aft" : "which aft";
    const resolved = execSync(lookup, { stdio: "pipe", encoding: "utf-8" }).trim();
    if (resolved) {
      candidates.push(resolved.split(/\r?\n/)[0]);
    }
  } catch {
    // ignore — PATH lookup is best-effort
  }

  for (const candidate of candidates) {
    try {
      if (!existsSync(candidate)) continue;
      const output = execSync(`"${candidate}" --version`, {
        stdio: "pipe",
        encoding: "utf-8",
      });
      const version = normalizeVersion(output);
      if (version) return version;
    } catch {
      // try next
    }
  }

  return null;
}
