import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { error, log, warn } from "./logger.js";

// ---------------------------------------------------------------------------
// Config shape (mirrors aft-opencode's schema, simplified for Pi)
// ---------------------------------------------------------------------------

export type Formatter =
  | "biome"
  | "prettier"
  | "deno"
  | "ruff"
  | "black"
  | "rustfmt"
  | "goimports"
  | "gofmt"
  | "none";

export type Checker =
  | "tsc"
  | "biome"
  | "pyright"
  | "ruff"
  | "cargo"
  | "go"
  | "staticcheck"
  | "none";

export type SemanticBackend = "fastembed" | "openai_compatible" | "ollama";

export interface SemanticConfig {
  backend?: SemanticBackend;
  model?: string;
  base_url?: string;
  api_key_env?: string;
  timeout_ms?: number;
  max_batch_size?: number;
}

export type ToolSurface = "minimal" | "recommended" | "all";

export interface AftConfig {
  format_on_edit?: boolean;
  validate_on_edit?: "syntax" | "full";
  formatter?: Record<string, Formatter>;
  checker?: Record<string, Checker>;
  tool_surface?: ToolSurface;
  disabled_tools?: string[];
  restrict_to_project_root?: boolean;
  experimental_search_index?: boolean;
  experimental_semantic_search?: boolean;
  semantic?: SemanticConfig;
}

// ---------------------------------------------------------------------------
// Minimal JSONC parser (strips comments + trailing commas before JSON.parse).
// Kept inline to avoid adding comment-json as a runtime dep for Pi.
// ---------------------------------------------------------------------------

function stripJsonc(input: string): string {
  let result = "";
  let i = 0;
  const n = input.length;
  let inString = false;
  let stringChar = "";
  while (i < n) {
    const ch = input[i];
    const next = input[i + 1];
    if (inString) {
      result += ch;
      if (ch === "\\" && i + 1 < n) {
        result += input[i + 1];
        i += 2;
        continue;
      }
      if (ch === stringChar) inString = false;
      i++;
      continue;
    }
    if (ch === '"' || ch === "'") {
      inString = true;
      stringChar = ch;
      result += ch;
      i++;
      continue;
    }
    if (ch === "/" && next === "/") {
      // line comment
      while (i < n && input[i] !== "\n") i++;
      continue;
    }
    if (ch === "/" && next === "*") {
      i += 2;
      while (i < n && !(input[i] === "*" && input[i + 1] === "/")) i++;
      i += 2;
      continue;
    }
    result += ch;
    i++;
  }
  // Strip trailing commas before } or ]
  return result.replace(/,(\s*[}\]])/g, "$1");
}

// ---------------------------------------------------------------------------
// Config file detection (.jsonc preferred over .json)
// ---------------------------------------------------------------------------

function detectConfigFile(basePath: string): {
  format: "json" | "jsonc" | "none";
  path: string;
} {
  const jsoncPath = `${basePath}.jsonc`;
  const jsonPath = `${basePath}.json`;
  if (existsSync(jsoncPath)) return { format: "jsonc", path: jsoncPath };
  if (existsSync(jsonPath)) return { format: "json", path: jsonPath };
  return { format: "none", path: jsonPath };
}

function loadConfigFromPath(configPath: string): AftConfig | null {
  try {
    if (!existsSync(configPath)) return null;
    const content = readFileSync(configPath, "utf-8");
    const parsed = JSON.parse(stripJsonc(content)) as AftConfig;
    log(`Config loaded from ${configPath}`);
    return parsed;
  } catch (err) {
    const errorMsg = err instanceof Error ? err.message : String(err);
    error(`Error loading config from ${configPath}: ${errorMsg}`);
    return null;
  }
}

// ---------------------------------------------------------------------------
// Merge configs (project overrides user, deep-merge nested maps)
// ---------------------------------------------------------------------------

function mergeSemanticConfig(
  base?: SemanticConfig,
  override?: SemanticConfig,
): SemanticConfig | undefined {
  // SECURITY: Only safe fields from project override are merged.
  // Sensitive fields (backend, base_url, api_key_env) must come from user config.
  const projectSafe: SemanticConfig = {};
  if (override?.model !== undefined) projectSafe.model = override.model;
  if (override?.timeout_ms !== undefined) projectSafe.timeout_ms = override.timeout_ms;
  if (override?.max_batch_size !== undefined) projectSafe.max_batch_size = override.max_batch_size;

  const semantic: SemanticConfig = { ...base, ...projectSafe };
  if (Object.values(semantic).every((v) => v === undefined)) return undefined;

  return Object.fromEntries(
    Object.entries(semantic).filter(([, v]) => v !== undefined),
  ) as SemanticConfig;
}

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  const disabledTools = [...(base.disabled_tools ?? []), ...(override.disabled_tools ?? [])];
  const formatter = { ...base.formatter, ...override.formatter };
  const checker = { ...base.checker, ...override.checker };
  const semantic = mergeSemanticConfig(base.semantic, override.semantic);

  // SECURITY: Strip sensitive semantic fields from override before spreading.
  const { semantic: _stripSemantic, ...safeOverride } = override;

  return {
    ...base,
    ...safeOverride,
    ...(Object.keys(formatter).length > 0 ? { formatter } : {}),
    ...(Object.keys(checker).length > 0 ? { checker } : {}),
    semantic,
    ...(disabledTools.length > 0 ? { disabled_tools: [...new Set(disabledTools)] } : {}),
  };
}

// ---------------------------------------------------------------------------
// Pi config directory detection
//
// Pi's convention:
//   - Global: ~/.pi/agent/
//   - Project: <projectDir>/.pi/
// ---------------------------------------------------------------------------

function getGlobalPiDir(): string {
  return join(homedir(), ".pi", "agent");
}

/**
 * Load AFT config:
 *   1. User-level:    ~/.pi/agent/aft.jsonc (or .json)
 *   2. Project-level: <project>/.pi/aft.jsonc (or .json)
 *
 * Project config merges on top of user config.
 */
export function loadAftConfig(projectDirectory: string): AftConfig {
  const userBasePath = join(getGlobalPiDir(), "aft");
  const userDetected = detectConfigFile(userBasePath);
  const userConfigPath =
    userDetected.format !== "none" ? userDetected.path : `${userBasePath}.json`;

  const projectBasePath = join(projectDirectory, ".pi", "aft");
  const projectDetected = detectConfigFile(projectBasePath);
  const projectConfigPath =
    projectDetected.format !== "none" ? projectDetected.path : `${projectBasePath}.json`;

  let config: AftConfig = loadConfigFromPath(userConfigPath) ?? {};

  const projectConfig = loadConfigFromPath(projectConfigPath);
  if (projectConfig) {
    if (
      projectConfig.semantic?.backend !== undefined ||
      projectConfig.semantic?.base_url !== undefined ||
      projectConfig.semantic?.api_key_env !== undefined
    ) {
      warn(
        "Ignoring semantic.backend/base_url/api_key_env from project config (security: use user config for external backends)",
      );
    }
    config = mergeConfigs(config, projectConfig);
  }

  return config;
}
