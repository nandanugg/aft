/// <reference path="../../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const packageRoot = resolve(import.meta.dir, "../../..");
const configPathsModuleUrl = new URL("../../cli/config-paths.ts", import.meta.url).href;

function createTempRoot(): string {
  return mkdtempSync(join(tmpdir(), "aft-cli-config-"));
}

function runDetectConfigPaths(env: Record<string, string | undefined>) {
  const script = [
    `const mod = await import(${JSON.stringify(configPathsModuleUrl)});`,
    "console.log(JSON.stringify(mod.detectConfigPaths()));",
  ].join("\n");

  const result = spawnSync("bun", ["--eval", script], {
    cwd: packageRoot,
    env: {
      ...process.env,
      ...env,
    },
    encoding: "utf-8",
  });

  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || "detectConfigPaths subprocess failed");
  }

  return JSON.parse(result.stdout) as {
    configDir: string;
    opencodeConfig: string;
    opencodeConfigFormat: string;
    aftConfig: string;
    aftConfigFormat: string;
    tuiConfig: string;
    tuiConfigFormat: string;
  };
}

describe("detectConfigPaths", () => {
  test("prefers OPENCODE_CONFIG_DIR and jsonc files", async () => {
    const root = createTempRoot();
    try {
      writeFileSync(join(root, "aft.json"), "{}");
      writeFileSync(join(root, "aft.jsonc"), "{}");
      writeFileSync(join(root, "opencode.jsonc"), "{}");

      const result = runDetectConfigPaths({
        OPENCODE_CONFIG_DIR: root,
        XDG_CONFIG_HOME: undefined,
      });

      expect(result.configDir).toBe(root);
      expect(result.aftConfigFormat).toBe("jsonc");
      expect(result.aftConfig).toBe(join(root, "aft.jsonc"));
      expect(result.opencodeConfigFormat).toBe("jsonc");
      expect(result.opencodeConfig).toBe(join(root, "opencode.jsonc"));
      expect(result.tuiConfigFormat).toBe("none");
      expect(result.tuiConfig).toBe(join(root, "tui.json"));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("falls back to XDG_CONFIG_HOME/opencode", async () => {
    const root = createTempRoot();
    try {
      const configDir = join(root, "opencode");
      mkdirSync(configDir, { recursive: true });

      writeFileSync(join(configDir, "tui.json"), "{}");
      writeFileSync(join(configDir, "opencode.json"), "{}");

      const result = runDetectConfigPaths({
        OPENCODE_CONFIG_DIR: undefined,
        XDG_CONFIG_HOME: root,
      });

      expect(result.configDir).toBe(configDir);
      expect(result.opencodeConfigFormat).toBe("json");
      expect(result.opencodeConfig).toBe(join(configDir, "opencode.json"));
      expect(result.tuiConfigFormat).toBe("json");
      expect(result.tuiConfig).toBe(join(configDir, "tui.json"));
      expect(result.aftConfigFormat).toBe("none");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
