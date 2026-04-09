import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { log } from "../logger.js";
import { getOpenCodeConfigPaths } from "./opencode-config-dir.js";

const PLUGIN_NAME = "@cortexkit/aft-opencode";
const PLUGIN_ENTRY = `${PLUGIN_NAME}@latest`;

function stripJsoncComments(text: string): string {
  let result = "";
  let i = 0;
  let inString = false;
  let escaped = false;

  while (i < text.length) {
    const ch = text[i];

    if (inString) {
      result += ch;
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      i++;
      continue;
    }

    if (ch === '"') {
      inString = true;
      result += ch;
      i++;
      continue;
    }

    if (ch === "/" && text[i + 1] === "/") {
      i += 2;
      while (i < text.length && text[i] !== "\n") i++;
      continue;
    }

    if (ch === "/" && text[i + 1] === "*") {
      i += 2;
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++;
      i += 2;
      continue;
    }

    result += ch;
    i++;
  }

  return result;
}

function stripTrailingCommas(text: string): string {
  let result = "";
  let i = 0;
  let inString = false;
  let escaped = false;

  while (i < text.length) {
    const ch = text[i];

    if (inString) {
      result += ch;
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      i++;
      continue;
    }

    if (ch === '"') {
      inString = true;
      result += ch;
      i++;
      continue;
    }

    if (ch === ",") {
      let j = i + 1;
      while (j < text.length && /\s/.test(text[j])) j++;
      if (text[j] === "}" || text[j] === "]") {
        i++;
        continue;
      }
    }

    result += ch;
    i++;
  }

  return result;
}

function parseJsonc<T>(content: string): T {
  return JSON.parse(stripTrailingCommas(stripJsoncComments(content))) as T;
}

function resolveTuiConfigPath(): string {
  const configDir = getOpenCodeConfigPaths({ binary: "opencode" }).configDir;
  const jsoncPath = join(configDir, "tui.jsonc");
  const jsonPath = join(configDir, "tui.json");

  if (existsSync(jsoncPath)) return jsoncPath;
  if (existsSync(jsonPath)) return jsonPath;
  return jsonPath;
}

export function ensureTuiPluginEntry(): boolean {
  try {
    const configPath = resolveTuiConfigPath();

    let config: Record<string, unknown> = {};
    if (existsSync(configPath)) {
      config = parseJsonc<Record<string, unknown>>(readFileSync(configPath, "utf-8")) ?? {};
    }

    const plugins = Array.isArray(config.plugin)
      ? config.plugin.filter((value): value is string => typeof value === "string")
      : [];

    if (
      plugins.some(
        (plugin) =>
          plugin === PLUGIN_NAME ||
          plugin.startsWith(`${PLUGIN_NAME}@`) ||
          plugin.includes("opencode-plugin") ||
          plugin.includes("aft-opencode"),
      )
    ) {
      return false;
    }

    plugins.push(PLUGIN_ENTRY);
    config.plugin = plugins;

    mkdirSync(dirname(configPath), { recursive: true });
    writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    log(`[aft-plugin] added TUI plugin entry to ${configPath}`);
    return true;
  } catch (error) {
    log(
      `[aft-plugin] failed to update tui.json: ${error instanceof Error ? error.message : String(error)}`,
    );
    return false;
  }
}
