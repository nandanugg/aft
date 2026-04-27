import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { parse as parseJsonc, stringify as stringifyJsonc } from "comment-json";

export type JsoncFormat = "json" | "jsonc" | "none";

export interface JsoncFile {
  path: string;
  format: JsoncFormat;
}

/** Detect an existing {name}.jsonc or {name}.json next to a base directory. */
export function detectJsoncFile(configDir: string, baseName: string): JsoncFile {
  const jsoncPath = `${configDir}/${baseName}.jsonc`;
  const jsonPath = `${configDir}/${baseName}.json`;

  if (existsSync(jsoncPath)) {
    return { path: jsoncPath, format: "jsonc" };
  }
  if (existsSync(jsonPath)) {
    return { path: jsonPath, format: "json" };
  }
  return { path: jsonPath, format: "none" };
}

/** Parse a JSONC file; returns null on missing file or unreadable content. */
export function readJsoncFile(path: string): {
  value: Record<string, unknown> | null;
  error?: string;
} {
  if (!existsSync(path)) {
    return { value: null };
  }
  try {
    const raw = readFileSync(path, "utf-8");
    const value = parseJsonc(raw) as Record<string, unknown>;
    return { value };
  } catch (error) {
    return {
      value: null,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Write a JSON/JSONC file, preserving comments when possible. Creates parent
 * directories. When `format === "jsonc"` and the value was produced by
 * `comment-json`'s `parse`, embedded comments are retained via `stringifyJsonc`.
 */
export function writeJsoncFile(
  path: string,
  value: Record<string, unknown>,
  format: JsoncFormat = "json",
): void {
  mkdirSync(dirname(path), { recursive: true });
  const serialized =
    format === "jsonc" ? stringifyJsonc(value, null, 2) : JSON.stringify(value, null, 2);
  writeFileSync(path, `${serialized}\n`);
}
