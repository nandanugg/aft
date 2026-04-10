import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { parse, stringify } from "comment-json";
import { ensureTuiPluginEntry } from "../shared/tui-config.js";
import { detectConfigPaths } from "./config-paths.js";
import { isOpenCodeInstalled } from "./opencode-helpers.js";
import { confirm, intro, log, outro } from "./prompts.js";

const PLUGIN_NAME = "@cortexkit/aft-opencode";
const PLUGIN_ENTRY = `${PLUGIN_NAME}@latest`;

function ensureDir(path: string): void {
  mkdirSync(dirname(path), { recursive: true });
}

function readJsonc(path: string): Record<string, unknown> | null {
  if (!existsSync(path)) {
    return null;
  }
  return (parse(readFileSync(path, "utf-8")) as Record<string, unknown>) ?? {};
}

function writeJsonc(path: string, config: Record<string, unknown>): void {
  ensureDir(path);
  writeFileSync(path, `${stringify(config, null, 2)}\n`);
}

function ensurePluginInOpenCodeConfig(path: string, format: "json" | "jsonc" | "none"): void {
  if (format === "none") {
    writeJsonc(path, { plugin: [PLUGIN_ENTRY] });
    return;
  }

  const existing = readJsonc(path);
  if (!existing) {
    throw new Error(`Could not parse ${path}`);
  }

  const plugins = Array.isArray(existing.plugin)
    ? existing.plugin.filter((entry): entry is string => typeof entry === "string")
    : [];
  if (!plugins.some((entry) => entry === PLUGIN_NAME || entry.startsWith(`${PLUGIN_NAME}@`))) {
    plugins.push(PLUGIN_ENTRY);
  }
  existing.plugin = plugins;
  writeJsonc(path, existing);
}

export async function runSetup(): Promise<number> {
  intro("AFT Setup");

  if (!isOpenCodeInstalled()) {
    log.error("OpenCode is not installed or not in PATH");
    outro("Install OpenCode first, then run setup again.");
    return 1;
  }

  const paths = detectConfigPaths();
  if (paths.opencodeConfigFormat === "none") {
    log.info(`OpenCode config will be created at ${paths.opencodeConfig}`);
  } else {
    log.success(`OpenCode config found at ${paths.opencodeConfig}`);
  }

  try {
    ensurePluginInOpenCodeConfig(paths.opencodeConfig, paths.opencodeConfigFormat);
    log.success("Added AFT to the OpenCode plugin list");
  } catch (error) {
    log.error(error instanceof Error ? error.message : String(error));
    outro("Setup failed");
    return 1;
  }

  const tuiAdded = ensureTuiPluginEntry();
  if (tuiAdded || existsSync(paths.tuiConfig)) {
    log.success("TUI plugin entry configured");
  } else {
    log.warn("Could not verify the TUI plugin entry");
  }

  const enableIndexedSearch = await confirm(
    "Enable indexed grep/glob search? Speeds up code searches with a trigram index.",
    true,
  );
  const enableSemanticSearch = await confirm(
    "Enable semantic code search? Lets you search by meaning (e.g., 'authentication logic'). Downloads a 22MB model on first use.",
    false,
  );

  if (enableSemanticSearch && process.platform === "darwin" && process.arch === "x64") {
    log.warn("Semantic search on Intel macOS requires `brew install onnxruntime`");
  }

  let aftConfig: Record<string, unknown> = {};
  if (paths.aftConfigFormat !== "none") {
    try {
      aftConfig = readJsonc(paths.aftConfig) ?? {};
    } catch (error) {
      log.error(error instanceof Error ? error.message : String(error));
      outro("Setup stopped to avoid overwriting your aft config.");
      return 1;
    }
  }

  aftConfig.experimental_search_index = enableIndexedSearch;
  aftConfig.experimental_semantic_search = enableSemanticSearch;

  writeJsonc(paths.aftConfig, aftConfig);
  log.success(`Wrote AFT config to ${paths.aftConfig}`);
  outro("Restart OpenCode to apply");
  return 0;
}
