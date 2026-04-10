/// <reference path="../../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const packageRoot = resolve(import.meta.dir, "../../..");
const cacheModuleUrl = new URL("../../cli/cache.ts", import.meta.url).href;
const packageJsonPath = resolve(packageRoot, "package.json");

function createTempRoot(): string {
  return mkdtempSync(join(tmpdir(), "aft-cli-cache-"));
}

function pluginCacheDir(root: string): string {
  return join(root, "opencode", "packages", "@cortexkit", "aft-opencode@latest");
}

function writeCachedPlugin(root: string, version: string): string {
  const cacheDir = pluginCacheDir(root);
  const packageDir = join(cacheDir, "node_modules", "@cortexkit", "aft-opencode");
  mkdirSync(packageDir, { recursive: true });
  writeFileSync(join(packageDir, "package.json"), JSON.stringify({ version }, null, 2));
  return cacheDir;
}

function readPackageVersion(): string {
  return (JSON.parse(readFileSync(packageJsonPath, "utf-8")) as { version: string }).version;
}

function runClearPluginCache(
  root: string,
  force = false,
): Awaited<ReturnType<typeof import("../../cli/cache.js")["clearPluginCache"]>> {
  const script = [
    `const mod = await import(${JSON.stringify(cacheModuleUrl)});`,
    `const result = await mod.clearPluginCache(${force ? "true" : "false"});`,
    "console.log(JSON.stringify(result));",
  ].join("\n");

  const result = spawnSync("bun", ["--eval", script], {
    cwd: packageRoot,
    env: {
      ...process.env,
      XDG_CACHE_HOME: root,
    },
    encoding: "utf-8",
  });

  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || "clearPluginCache subprocess failed");
  }

  return JSON.parse(result.stdout) as Awaited<
    ReturnType<typeof import("../../cli/cache.js")["clearPluginCache"]>
  >;
}

describe("clearPluginCache", () => {
  test("returns not_found when the cache directory does not exist", async () => {
    const root = createTempRoot();
    try {
      const result = runClearPluginCache(root);

      expect(result.action).toBe("not_found");
      expect(result.path).toBe(pluginCacheDir(root));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("returns up_to_date when cached version matches the CLI version", async () => {
    const root = createTempRoot();
    try {
      const version = readPackageVersion();
      const cacheDir = writeCachedPlugin(root, version);
      const result = runClearPluginCache(root);

      expect(result.action).toBe("up_to_date");
      expect(result.cached).toBe(version);
      expect(result.path).toBe(cacheDir);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("clears the cache when force is true", async () => {
    const root = createTempRoot();
    try {
      const version = readPackageVersion();
      const cacheDir = writeCachedPlugin(root, version);
      const result = runClearPluginCache(root, true);

      expect(result.action).toBe("cleared");
      expect(existsSync(cacheDir)).toBe(false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("clears the cache when the cached version is stale", async () => {
    const root = createTempRoot();
    try {
      const cacheDir = writeCachedPlugin(root, "0.0.1");
      const result = runClearPluginCache(root);

      expect(result.action).toBe("cleared");
      expect(result.cached).toBe("0.0.1");
      expect(existsSync(cacheDir)).toBe(false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
