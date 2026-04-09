import { createHash } from "node:crypto";
import { join } from "node:path";

/**
 * Compute a stable hash for a project directory.
 * Used to scope RPC port files per-project so multiple
 * OpenCode Desktop instances don't overwrite each other.
 */
export function projectHash(directory: string): string {
  // Normalize: strip trailing slashes
  const normalized = directory.replace(/\/+$/, "");
  return createHash("sha256").update(normalized).digest("hex").slice(0, 16);
}

/**
 * Get the per-project RPC port file path.
 */
export function rpcPortFilePath(storageDir: string, directory: string): string {
  const hash = projectHash(directory);
  return join(storageDir, "rpc", hash, "port");
}
