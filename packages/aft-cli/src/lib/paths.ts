import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

/** `~/.cache/aft/bin/` (or the platform equivalent) — same as plugin's `getCacheDir`. */
export function getAftBinaryCacheDir(): string {
  if (process.env.AFT_CACHE_DIR) {
    return join(process.env.AFT_CACHE_DIR, "bin");
  }
  if (process.platform === "win32") {
    const localAppData = process.env.LOCALAPPDATA || process.env.APPDATA;
    const base = localAppData || join(homedir(), "AppData", "Local");
    return join(base, "aft", "bin");
  }
  const base = process.env.XDG_CACHE_HOME || join(homedir(), ".cache");
  return join(base, "aft", "bin");
}

export function getAftBinaryName(): string {
  return process.platform === "win32" ? "aft.exe" : "aft";
}

/** Resolve the plugin log file path. Shared with the plugin's logger. */
export function getTmpLogPath(filename: string): string {
  return join(tmpdir(), filename);
}
