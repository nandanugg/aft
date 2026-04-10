import { execSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";
import { parse as parseJsonc } from "comment-json";
import { getCacheDir } from "../downloader.js";
import { getLogFilePath } from "../logger.js";
import { getManualInstallHint } from "../onnx-runtime.js";
import { getBinaryCacheInfo } from "./cache.js";
import { type ConfigPaths, detectConfigPaths } from "./config-paths.js";
import { getOpenCodeVersion, isOpenCodeInstalled } from "./opencode-helpers.js";

const PLUGIN_NAME = "@cortexkit/aft-opencode";
const PLUGIN_ENTRY_WITH_VERSION = `${PLUGIN_NAME}@latest`;
const ONNX_RUNTIME_VERSION = "1.24.4";

export interface DiagnosticReport {
  timestamp: string;
  platform: string;
  arch: string;
  nodeVersion: string;
  pluginVersion: string;
  binaryVersion: string | null;
  opencodeInstalled: boolean;
  opencodeVersion: string | null;
  configPaths: ConfigPaths;
  opencodeConfigHasPlugin: boolean;
  aftConfig: {
    exists: boolean;
    parseError?: string;
    flags: Record<string, unknown>;
  };
  binaryCache: ReturnType<typeof getBinaryCacheInfo>;
  pluginCache: {
    cached?: string;
    latest?: string;
    path: string;
  };
  storageDir: {
    path: string;
    exists: boolean;
    indexSize: number;
    semanticSize: number;
    backupsSize: number;
    urlCacheSize: number;
    onnxruntimeSize: number;
  };
  onnxRuntime: {
    required: boolean;
    systemPath: string | null;
    cachedPath: string | null;
    platform: string;
    installHint: string;
  };
  logFile: {
    path: string;
    exists: boolean;
    sizeKb: number;
  };
}

function getSelfVersion(): string {
  const require = createRequire(import.meta.url);
  for (const relPath of ["../../package.json", "../package.json"]) {
    try {
      const version = (require(relPath) as { version?: string }).version;
      if (typeof version === "string" && version.length > 0) {
        return version;
      }
    } catch {
      // Try next path.
    }
  }
  return "unknown";
}

function getOpenCodeCacheDir(): string {
  const xdgCache = process.env.XDG_CACHE_HOME;
  if (xdgCache) {
    return join(xdgCache, "opencode");
  }
  return join(homedir(), ".cache", "opencode");
}

function getPluginCacheInfo(): { path: string; cached?: string; latest?: string } {
  const path = join(getOpenCodeCacheDir(), "packages", PLUGIN_ENTRY_WITH_VERSION);
  let cached: string | undefined;
  try {
    const installedPkgPath = join(
      path,
      "node_modules",
      "@cortexkit",
      "aft-opencode",
      "package.json",
    );
    if (existsSync(installedPkgPath)) {
      const pkg = JSON.parse(readFileSync(installedPkgPath, "utf-8")) as { version?: unknown };
      cached = typeof pkg.version === "string" ? pkg.version : undefined;
    }
  } catch {
    cached = undefined;
  }
  return {
    path,
    cached,
    latest: getSelfVersion(),
  };
}

function dirSize(path: string): number {
  if (!existsSync(path)) {
    return 0;
  }

  const stat = statSync(path);
  if (stat.isFile()) {
    return stat.size;
  }
  if (!stat.isDirectory()) {
    return 0;
  }

  let total = 0;
  for (const entry of readdirSync(path)) {
    total += dirSize(join(path, entry));
  }
  return total;
}

function sanitizeString(value: string): string {
  const escapedHome = escapeRegex(homedir());
  const username = userInfo().username;
  let sanitized = value.replace(new RegExp(escapedHome, "g"), "~");
  sanitized = sanitized.replace(/\/Users\/[^/]+\//g, "/Users/<USER>/");
  sanitized = sanitized.replace(/\/home\/[^/]+\//g, "/home/<USER>/");
  sanitized = sanitized.replace(/C:\\\\Users\\\\[^\\\\]+\\\\/g, "C:\\\\Users\\\\<USER>\\\\");
  if (username) {
    sanitized = sanitized.replace(new RegExp(escapeRegex(username), "g"), "<USER>");
  }
  return sanitized;
}

function sanitizeValue(value: unknown): unknown {
  if (typeof value === "string") {
    return sanitizeString(value);
  }
  if (Array.isArray(value)) {
    return value.map((entry) => sanitizeValue(entry));
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, sanitizeValue(entry)]),
    );
  }
  return value;
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function readConfig(path: string): { value: Record<string, unknown> | null; error?: string } {
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

function hasPluginEntry(opencodeConfig: Record<string, unknown> | null): boolean {
  const plugins = Array.isArray(opencodeConfig?.plugin) ? opencodeConfig.plugin : [];
  return plugins.some((entry) => {
    if (typeof entry !== "string") return false;
    if (entry === PLUGIN_NAME) return true;
    if (entry.startsWith(`${PLUGIN_NAME}@`)) return true;
    if (entry === PLUGIN_ENTRY_WITH_VERSION) return true;
    // Local dev paths containing our package name or entry point
    if (entry.includes("/opencode-plugin") || entry.includes("/aft-opencode")) return true;
    return false;
  });
}

function getStorageDir(): string {
  const dataHome = process.env.XDG_DATA_HOME || join(homedir(), ".local", "share");
  return join(dataHome, "opencode", "storage", "plugin", "aft");
}

function getOnnxLibraryName(): string {
  if (process.platform === "darwin") {
    return "libonnxruntime.dylib";
  }
  if (process.platform === "win32") {
    return "onnxruntime.dll";
  }
  return "libonnxruntime.so";
}

function findSystemOnnxRuntime(): string | null {
  const libName = getOnnxLibraryName();
  const searchPaths =
    process.platform === "darwin"
      ? ["/opt/homebrew/lib", "/usr/local/lib"]
      : process.platform === "linux"
        ? ["/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/local/lib"]
        : [];

  for (const path of searchPaths) {
    if (existsSync(join(path, libName))) {
      return path;
    }
  }
  return null;
}

function findCachedOnnxRuntime(storageDir: string): string | null {
  const ortDir = join(storageDir, "onnxruntime", ONNX_RUNTIME_VERSION);
  return existsSync(join(ortDir, getOnnxLibraryName())) ? ortDir : null;
}

function normalizeBinaryVersion(output: string): string | null {
  const trimmed = output.trim();
  if (!trimmed) {
    return null;
  }
  return trimmed.replace(/^aft\s+/, "");
}

function probeBinaryVersion(pluginVersion: string): string | null {
  const ext = process.platform === "win32" ? ".exe" : "";
  const candidates = [join(getCacheDir(), `v${pluginVersion}`, `aft${ext}`)];

  try {
    const lookupCommand = process.platform === "win32" ? "where aft" : "which aft";
    const resolved = execSync(lookupCommand, { stdio: "pipe", encoding: "utf-8" }).trim();
    if (resolved) {
      candidates.push(resolved.split(/\r?\n/)[0]);
    }
  } catch {
    // Ignore lookup failures.
  }

  for (const candidate of candidates) {
    try {
      if (!existsSync(candidate)) {
        continue;
      }
      const output = execSync(`"${candidate}" --version`, { stdio: "pipe", encoding: "utf-8" });
      const version = normalizeBinaryVersion(output);
      if (version) {
        return version;
      }
    } catch {
      // Try the next candidate.
    }
  }

  return null;
}

export async function collectDiagnostics(): Promise<DiagnosticReport> {
  const pluginVersion = getSelfVersion();
  const configPaths = detectConfigPaths();
  const opencodeConfig = readConfig(configPaths.opencodeConfig);
  const aftConfig = readConfig(configPaths.aftConfig);
  const storageDirPath = getStorageDir();
  const logPath = getLogFilePath();

  return {
    timestamp: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    nodeVersion: process.version,
    pluginVersion,
    binaryVersion: probeBinaryVersion(pluginVersion),
    opencodeInstalled: isOpenCodeInstalled(),
    opencodeVersion: getOpenCodeVersion(),
    configPaths,
    opencodeConfigHasPlugin: hasPluginEntry(opencodeConfig.value),
    aftConfig: {
      exists: existsSync(configPaths.aftConfig),
      ...(aftConfig.error ? { parseError: aftConfig.error } : {}),
      flags: (sanitizeValue(aftConfig.value ?? {}) as Record<string, unknown>) ?? {},
    },
    binaryCache: getBinaryCacheInfo(),
    pluginCache: getPluginCacheInfo(),
    storageDir: {
      path: storageDirPath,
      exists: existsSync(storageDirPath),
      indexSize: dirSize(join(storageDirPath, "index")),
      semanticSize: dirSize(join(storageDirPath, "semantic")),
      backupsSize: dirSize(join(storageDirPath, "backups")),
      urlCacheSize: dirSize(join(storageDirPath, "url_cache")),
      onnxruntimeSize: dirSize(join(storageDirPath, "onnxruntime")),
    },
    onnxRuntime: {
      required: aftConfig.value?.experimental_semantic_search === true,
      systemPath: findSystemOnnxRuntime(),
      cachedPath: findCachedOnnxRuntime(storageDirPath),
      platform: `${process.platform}-${process.arch}`,
      installHint: getManualInstallHint(),
    },
    logFile: {
      path: logPath,
      exists: existsSync(logPath),
      sizeKb: existsSync(logPath) ? Math.round(statSync(logPath).size / 1024) : 0,
    },
  };
}

export function renderDiagnosticsMarkdown(report: DiagnosticReport): string {
  const configSummary = JSON.stringify(report.configPaths, null, 2);
  const binaryCacheSummary = JSON.stringify(report.binaryCache, null, 2);
  const pluginCacheSummary = JSON.stringify(report.pluginCache, null, 2);
  const storageSummary = JSON.stringify(report.storageDir, null, 2);
  const ortSummary = JSON.stringify(report.onnxRuntime, null, 2);

  return [
    `- Timestamp: ${report.timestamp}`,
    `- OpenCode installed: ${report.opencodeInstalled}`,
    `- OpenCode config has plugin: ${report.opencodeConfigHasPlugin}`,
    `- AFT config parse error: ${report.aftConfig.parseError ?? "none"}`,
    "",
    "### Config paths",
    "```json",
    configSummary,
    "```",
    "",
    "### AFT flags",
    "```json",
    JSON.stringify(report.aftConfig.flags, null, 2),
    "```",
    "",
    "### Plugin cache",
    "```json",
    pluginCacheSummary,
    "```",
    "",
    "### Binary cache",
    "```json",
    binaryCacheSummary,
    "```",
    "",
    "### Storage",
    "```json",
    storageSummary,
    "```",
    "",
    "### ONNX Runtime",
    "```json",
    ortSummary,
    "```",
  ].join("\n");
}
