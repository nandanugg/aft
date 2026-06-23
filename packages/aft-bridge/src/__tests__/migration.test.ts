/// <reference path="../bun-test.d.ts" />

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  ensureStorageMigrated,
  migrateAftConfigFile,
  resolveCortexKitStorageRoot,
  resolveLegacyStorageRoot,
} from "../migration.js";
import { type AftFixtureBehavior, writeAftFixture } from "./test-utils/aft-executable-fixture.js";
import { acquireEnv } from "./test-utils/env-guard.js";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));

describe("storage migration bootstrap", () => {
  let tempDir: string;
  let releaseEnv: (() => void) | undefined;

  beforeEach(async () => {
    tempDir = mkdtempSync(join(tmpdir(), "aft-migration-test-"));
    releaseEnv = await acquireEnv({
      XDG_DATA_HOME: tempDir,
      HOME: tempDir,
    });
  });

  afterEach(() => {
    releaseEnv?.();
    releaseEnv = undefined;
    rmSync(tempDir, { recursive: true, force: true });
  });

  function binary(behavior: AftFixtureBehavior): string {
    const path = join(tempDir, `aft-${Math.random().toString(16).slice(2)}`);
    return writeAftFixture(path, behavior);
  }

  test("ensureStorageMigrated_no_legacy_is_noop", async () => {
    await expect(
      ensureStorageMigrated({ harness: "opencode", binaryPath: "/missing/aft" }),
    ).resolves.toBeUndefined();
  });

  test("ensureStorageMigrated_with_source_marker_backfills_target_marker", async () => {
    const legacyRoot = resolveLegacyStorageRoot("opencode");
    mkdirSync(legacyRoot, { recursive: true });
    writeFileSync(join(legacyRoot, ".migrated_to_cortexkit"), "{}", "utf8");
    const aft = binary({ exitCode: 0 });

    await expect(
      ensureStorageMigrated({ harness: "opencode", binaryPath: aft }),
    ).resolves.toBeUndefined();
  });

  test("ensureStorageMigrated_spawns_and_succeeds", async () => {
    const legacyRoot = resolveLegacyStorageRoot("opencode");
    mkdirSync(legacyRoot, { recursive: true });
    writeFileSync(join(legacyRoot, "warned_tools.json"), "{}", "utf8");
    const aft = binary({ exitCode: 0 });

    await expect(
      ensureStorageMigrated({ harness: "opencode", binaryPath: aft }),
    ).resolves.toBeUndefined();
  });

  test("ensureStorageMigrated_throws_on_nonzero_exit", async () => {
    const legacyRoot = resolveLegacyStorageRoot("opencode");
    mkdirSync(legacyRoot, { recursive: true });
    writeFileSync(join(legacyRoot, "warned_tools.json"), "{}", "utf8");
    const aft = binary({ stderr: "failed\n", exitCode: 5 });

    await expect(ensureStorageMigrated({ harness: "opencode", binaryPath: aft })).rejects.toThrow(
      /exit 5.*logs\/migration\/opencode-/,
    );
  });

  test("ensureStorageMigrated_throws_on_timeout", async () => {
    const legacyRoot = resolveLegacyStorageRoot("opencode");
    mkdirSync(legacyRoot, { recursive: true });
    writeFileSync(join(legacyRoot, "warned_tools.json"), "{}", "utf8");
    const aft = binary({ sleepMs: 1_000 });

    await expect(
      ensureStorageMigrated({ harness: "opencode", binaryPath: aft, timeoutMs: 10 }),
    ).rejects.toThrow(/ETIMEDOUT|timed out|spawn error/i);
  });

  test("resolveLegacyStorageRoot_returns_pi_fixed_path", () => {
    expect(resolveLegacyStorageRoot("pi")).toBe(
      join(process.env.HOME as string, ".pi", "agent", "aft"),
    );
  });

  test("resolveLegacyStorageRoot_returns_opencode_xdg_path", () => {
    expect(resolveLegacyStorageRoot("opencode")).toBe(
      join(tempDir, "opencode", "storage", "plugin", "aft"),
    );
  });

  test("resolveCortexKitStorageRoot_uses_new_xdg_path", () => {
    expect(resolveCortexKitStorageRoot()).toBe(join(tempDir, "cortexkit", "aft"));
  });
});

describe("config file migration", () => {
  let root: string;

  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), "aft-config-migration-test-"));
  });

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  test("single legacy source copies atomically to CortexKit target", () => {
    const source = join(root, "opencode", "aft.jsonc");
    const target = join(root, "cortexkit", "aft.jsonc");
    mkdirSync(join(root, "opencode"), { recursive: true });
    writeFileSync(source, '{\n  // keep me\n  "semantic_search": true,\n}\n', "utf8");

    const result = migrateAftConfigFile({
      scope: "user",
      targetPath: target,
      legacySources: [{ path: source, label: "OpenCode user" }],
    });

    expect(result.migrated).toBe(true);
    expect(result.conflict).toBe(false);
    expect(readFileSync(target, "utf8")).toBe(readFileSync(source, "utf8"));
  });

  test("existing different target is not overwritten and emits a warning", () => {
    const source = join(root, "pi", "aft.jsonc");
    const target = join(root, "cortexkit", "aft.jsonc");
    mkdirSync(join(root, "pi"), { recursive: true });
    mkdirSync(join(root, "cortexkit"), { recursive: true });
    writeFileSync(source, '{"semantic_search":true}\n', "utf8");
    writeFileSync(target, '{"semantic_search":false}\n', "utf8");

    const result = migrateAftConfigFile({
      scope: "user",
      targetPath: target,
      legacySources: [{ path: source, label: "Pi user" }],
    });

    expect(result.migrated).toBe(false);
    expect(result.conflict).toBe(true);
    expect(readFileSync(target, "utf8")).toBe('{"semantic_search":false}\n');
    expect(result.warnings.join("\n")).toContain(source);
    expect(result.warnings.join("\n")).toContain(target);
  });

  test("OpenCode and Pi legacy sources with different semantics refuse without writing", () => {
    const opencode = join(root, "opencode", "aft.jsonc");
    const pi = join(root, "pi", "aft.jsonc");
    const target = join(root, "cortexkit", "aft.jsonc");
    mkdirSync(join(root, "opencode"), { recursive: true });
    mkdirSync(join(root, "pi"), { recursive: true });
    writeFileSync(opencode, '{"semantic_search":true}\n', "utf8");
    writeFileSync(pi, '{"semantic_search":false}\n', "utf8");

    const result = migrateAftConfigFile({
      scope: "user",
      targetPath: target,
      legacySources: [
        { path: opencode, label: "OpenCode user" },
        { path: pi, label: "Pi user" },
      ],
    });

    expect(result.migrated).toBe(false);
    expect(result.conflict).toBe(true);
    expect(existsSync(target)).toBe(false);
    expect(result.warnings.join("\n")).toContain(opencode);
    expect(result.warnings.join("\n")).toContain(pi);
  });

  test("OpenCode and Pi legacy sources with identical semantics copy one", () => {
    const opencode = join(root, "opencode", "aft.jsonc");
    const pi = join(root, "pi", "aft.jsonc");
    const target = join(root, "cortexkit", "aft.jsonc");
    mkdirSync(join(root, "opencode"), { recursive: true });
    mkdirSync(join(root, "pi"), { recursive: true });
    writeFileSync(
      opencode,
      '{\n  "formatter": {"typescript": "biome"},\n  "semantic_search": true,\n}\n',
      "utf8",
    );
    writeFileSync(pi, '{"semantic_search":true,"formatter":{"typescript":"biome"}}\n', "utf8");

    const result = migrateAftConfigFile({
      scope: "project",
      targetPath: target,
      legacySources: [
        { path: opencode, label: "OpenCode project" },
        { path: pi, label: "Pi project" },
      ],
    });

    expect(result.migrated).toBe(true);
    expect(result.conflict).toBe(false);
    expect(readFileSync(target, "utf8")).toBe(readFileSync(opencode, "utf8"));
  });

  test("concurrent migrations are first-wins and leave valid target content", async () => {
    const source = join(root, "legacy", "aft.jsonc");
    const target = join(root, "cortexkit", "aft.jsonc");
    mkdirSync(join(root, "legacy"), { recursive: true });
    writeFileSync(source, '{"semantic_search":true,"formatter":{"typescript":"biome"}}\n', "utf8");

    const script = `
      import { migrateAftConfigFile } from "./src/migration.ts";
      const result = migrateAftConfigFile({
        scope: "user",
        targetPath: process.env.TARGET,
        legacySources: [{ path: process.env.SOURCE, label: "legacy" }],
      });
      console.log(JSON.stringify(result));
    `;
    const children = Array.from(
      { length: 8 },
      () =>
        new Promise<{ status: number | null; stdout: string; stderr: string }>((resolveChild) => {
          const child = spawn(process.execPath, ["-e", script], {
            cwd: packageRoot,
            env: { ...process.env, TARGET: target, SOURCE: source },
            stdio: ["ignore", "pipe", "pipe"],
          });
          let stdout = "";
          let stderr = "";
          child.stdout.on("data", (chunk) => {
            stdout += chunk;
          });
          child.stderr.on("data", (chunk) => {
            stderr += chunk;
          });
          child.on("close", (status) => resolveChild({ status, stdout, stderr }));
        }),
    );

    const results = await Promise.all(children);
    for (const child of results) {
      expect(child.status).toBe(0);
      expect(child.stderr).toBe("");
    }
    const parsed = results.map((child) => JSON.parse(child.stdout.trim()) as { migrated: boolean });
    expect(parsed.filter((result) => result.migrated).length).toBe(1);
    expect(JSON.parse(readFileSync(target, "utf8"))).toEqual({
      semantic_search: true,
      formatter: { typescript: "biome" },
    });
  });
});
