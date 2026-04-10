import { existsSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir, platform } from "node:os";
import { join } from "node:path";
import { getCacheDir } from "../downloader.js";

const PLUGIN_NAME = "@cortexkit/aft-opencode";
const PLUGIN_ENTRY_WITH_VERSION = `${PLUGIN_NAME}@latest`;

function getOpenCodeCacheDir(): string {
  const xdgCache = process.env.XDG_CACHE_HOME;
  if (xdgCache) {
    return join(xdgCache, "opencode");
  }

  if (platform() === "win32") {
    const localAppData = process.env.LOCALAPPDATA ?? join(homedir(), "AppData", "Local");
    return join(localAppData, "opencode");
  }

  return join(homedir(), ".cache", "opencode");
}

function getPluginCacheDir(): string {
  return join(getOpenCodeCacheDir(), "packages", PLUGIN_ENTRY_WITH_VERSION);
}

function getSelfVersion(): string | undefined {
  const require = createRequire(import.meta.url);
  for (const relPath of ["../../package.json", "../package.json"]) {
    try {
      const version = (require(relPath) as { version?: string }).version;
      if (typeof version === "string" && version.length > 0) {
        return version;
      }
    } catch {
      // Try the next path.
    }
  }
  return undefined;
}

function readCachedPluginVersion(pluginCacheDir: string): string | undefined {
  try {
    const installedPkgPath = join(
      pluginCacheDir,
      "node_modules",
      "@cortexkit",
      "aft-opencode",
      "package.json",
    );
    if (!existsSync(installedPkgPath)) {
      return undefined;
    }
    const pkg = JSON.parse(readFileSync(installedPkgPath, "utf-8")) as { version?: unknown };
    return typeof pkg.version === "string" ? pkg.version : undefined;
  } catch {
    return undefined;
  }
}

function inspectPluginCache(): {
  path: string;
  cached?: string;
  latest?: string;
  exists: boolean;
} {
  const path = getPluginCacheDir();
  return {
    path,
    cached: readCachedPluginVersion(path),
    latest: getSelfVersion(),
    exists: existsSync(path),
  };
}

function compareVersionLabels(a: string, b: string): number {
  return a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });
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

export async function clearPluginCache(force = false): Promise<{
  action: "cleared" | "up_to_date" | "not_found" | "error";
  path: string;
  cached?: string;
  latest?: string;
  error?: string;
}> {
  const info = inspectPluginCache();

  if (!info.exists) {
    return { action: "not_found", path: info.path };
  }

  if (!force && info.cached && info.cached === info.latest) {
    return {
      action: "up_to_date",
      path: info.path,
      cached: info.cached,
      latest: info.latest,
    };
  }

  try {
    rmSync(info.path, { recursive: true, force: true });
    return {
      action: "cleared",
      path: info.path,
      cached: info.cached,
      latest: info.latest,
    };
  } catch (error) {
    return {
      action: "error",
      path: info.path,
      cached: info.cached,
      latest: info.latest,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

export function getBinaryCacheInfo(): {
  versions: string[];
  activeVersion: string | null;
  totalSize: number;
  path: string;
} {
  const path = getCacheDir();
  if (!existsSync(path)) {
    return {
      versions: [],
      activeVersion: null,
      totalSize: 0,
      path,
    };
  }

  const versions = readdirSync(path)
    .filter((entry) => {
      try {
        return statSync(join(path, entry)).isDirectory();
      } catch {
        return false;
      }
    })
    .sort(compareVersionLabels);

  const selfVersion = getSelfVersion();
  const selfTag = selfVersion
    ? selfVersion.startsWith("v")
      ? selfVersion
      : `v${selfVersion}`
    : null;
  const activeVersion = selfTag && versions.includes(selfTag) ? selfTag : null;

  return {
    versions,
    activeVersion,
    totalSize: dirSize(path),
    path,
  };
}
