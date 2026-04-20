import { execSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, renameSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import { ensureBinary, getCacheDir, getCachedBinaryPath } from "./downloader.js";
import { log, warn } from "./logger.js";
import { PLATFORM_ARCH_MAP } from "./platform.js";

/**
 * Copy an npm platform binary to the versioned cache so we never run from
 * node_modules directly. This prevents corruption when npm updates the
 * package while a bridge process is running the binary.
 */
function copyToVersionedCache(npmBinaryPath: string): string | null {
  try {
    const result = spawnSync(npmBinaryPath, ["--version"], {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
      timeout: 5000,
    });
    const rawVersion = result.stdout?.trim();
    if (!rawVersion) return null;

    const version = rawVersion.replace(/^aft\s+/, "");
    const tag = version.startsWith("v") ? version : `v${version}`;
    const cacheDir = getCacheDir();
    const versionedDir = join(cacheDir, tag);
    const ext = process.platform === "win32" ? ".exe" : "";
    const cachedPath = join(versionedDir, `aft${ext}`);

    if (existsSync(cachedPath)) return cachedPath;

    mkdirSync(versionedDir, { recursive: true });
    const tmpPath = `${cachedPath}.tmp`;
    copyFileSync(npmBinaryPath, tmpPath);
    if (process.platform !== "win32") {
      chmodSync(tmpPath, 0o755);
    }
    renameSync(tmpPath, cachedPath);
    log(`Copied npm binary to versioned cache: ${cachedPath}`);
    return cachedPath;
  } catch (err) {
    warn(`Failed to copy binary to cache: ${err instanceof Error ? err.message : String(err)}`);
    return null;
  }
}

export function platformKey(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  const archMap = PLATFORM_ARCH_MAP[platform];
  if (!archMap) {
    throw new Error(
      `Unsupported platform: ${platform} (arch: ${arch}). ` +
        `Supported platforms: ${Object.keys(PLATFORM_ARCH_MAP).join(", ")}`,
    );
  }
  const key = archMap[arch];
  if (!key) {
    throw new Error(
      `Unsupported architecture: ${arch} on platform ${platform}. ` +
        `Supported architectures for ${platform}: ${Object.keys(archMap).join(", ")}`,
    );
  }
  return key;
}

/**
 * Locate the `aft` binary synchronously by checking (in order):
 * 1. Cached binary from previous auto-download (~/.cache/aft/bin/)
 * 2. npm platform package via `require.resolve(@cortexkit/aft-<platform>/bin/aft)`
 * 3. PATH lookup via `which aft` (or `where aft` on Windows)
 * 4. ~/.cargo/bin/aft
 */
export function findBinarySync(): string | null {
  const ext = process.platform === "win32" ? ".exe" : "";

  const pluginVersion = (() => {
    try {
      const req = createRequire(import.meta.url);
      return `v${(req("../package.json") as { version: string }).version}`;
    } catch {
      return null;
    }
  })();
  if (pluginVersion) {
    const versionCached = getCachedBinaryPath(pluginVersion);
    if (versionCached) return versionCached;
  }

  try {
    const key = platformKey();
    const packageBin = `@cortexkit/aft-${key}/bin/aft${ext}`;
    const req = createRequire(import.meta.url);
    const resolved = req.resolve(packageBin);
    if (existsSync(resolved)) {
      const copied = copyToVersionedCache(resolved);
      return copied ?? resolved;
    }
  } catch {
    // npm package not installed or resolution failed
  }

  try {
    const whichCmd = process.platform === "win32" ? "where aft" : "which aft";
    const result = execSync(whichCmd, {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    }).trim();
    if (result) return result;
  } catch {
    // not in PATH
  }

  const cargoPath = join(homedir(), ".cargo", "bin", `aft${ext}`);
  if (existsSync(cargoPath)) return cargoPath;

  return null;
}

export async function findBinary(): Promise<string> {
  const syncResult = findBinarySync();
  if (syncResult) {
    log(`Resolved binary: ${syncResult}`);
    return syncResult;
  }

  log("Binary not found locally, attempting auto-download...");
  const downloaded = await ensureBinary();
  if (downloaded) return downloaded;

  throw new Error(
    [
      "Could not find the `aft` binary.",
      "",
      "Attempted sources:",
      "  - Cache directory (~/.cache/aft/bin/)",
      "  - npm platform package (@cortexkit/aft-<platform>)",
      "  - PATH lookup (which aft)",
      "  - ~/.cargo/bin/aft",
      "  - Auto-download from GitHub releases (failed)",
      "",
      "Install it using one of these methods:",
      "  npm install @cortexkit/aft-pi              # installs platform-specific binary via npm",
      "  cargo install agent-file-tools             # from crates.io",
      "",
      "Or add the aft directory to your PATH.",
    ].join("\n"),
  );
}
