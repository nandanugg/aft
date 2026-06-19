/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { formatDroppedKeyWarnings, readConfigTiers } from "../config-tiers.js";

const tempRoots = new Set<string>();

function createTempDir(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-bridge-config-tiers-"));
  tempRoots.add(root);
  return root;
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("readConfigTiers", () => {
  test("both files present returns 2 tiers in user-then-project order with verbatim doc", () => {
    const root = createTempDir();
    const userPath = join(root, "user-aft.jsonc");
    const projectPath = join(root, "project-aft.jsonc");

    const userDoc = '{\n  // user comment\n  "key": "value",\n}';
    const projectDoc = '{\n  /* project comment */\n  "key": "project-value",\n}';

    writeFileSync(userPath, userDoc, "utf8");
    writeFileSync(projectPath, projectDoc, "utf8");

    const result = readConfigTiers({
      userConfigPath: userPath,
      projectConfigPath: projectPath,
    });

    expect(result).toHaveLength(2);
    expect(result[0]).toEqual({
      tier: "user",
      source: resolve(userPath),
      doc: userDoc,
    });
    expect(result[1]).toEqual({
      tier: "project",
      source: resolve(projectPath),
      doc: projectDoc,
    });
  });

  test("only user present returns 1 tier (user)", () => {
    const root = createTempDir();
    const userPath = join(root, "user-aft.jsonc");
    const projectPath = join(root, "non-existent.jsonc");

    const userDoc = '{"key": "value"}';
    writeFileSync(userPath, userDoc, "utf8");

    const result = readConfigTiers({
      userConfigPath: userPath,
      projectConfigPath: projectPath,
    });

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({
      tier: "user",
      source: resolve(userPath),
      doc: userDoc,
    });
  });

  test("only project present returns 1 tier (project)", () => {
    const root = createTempDir();
    const userPath = join(root, "non-existent.jsonc");
    const projectPath = join(root, "project-aft.jsonc");

    const projectDoc = '{"key": "project-value"}';
    writeFileSync(projectPath, projectDoc, "utf8");

    const result = readConfigTiers({
      userConfigPath: userPath,
      projectConfigPath: projectPath,
    });

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({
      tier: "project",
      source: resolve(projectPath),
      doc: projectDoc,
    });
  });

  test("neither present returns empty array", () => {
    const root = createTempDir();
    const userPath = join(root, "non-existent-user.jsonc");
    const projectPath = join(root, "non-existent-project.jsonc");

    const result = readConfigTiers({
      userConfigPath: userPath,
      projectConfigPath: projectPath,
    });

    expect(result).toEqual([]);
  });

  test("a path that exists but is unreadable/dir is skipped without throwing", () => {
    const root = createTempDir();
    const userPath = join(root, "user-dir");
    mkdirSync(userPath); // directory, readFileSync will throw
    const projectPath = join(root, "project-aft.jsonc");

    const projectDoc = '{"key": "project-value"}';
    writeFileSync(projectPath, projectDoc, "utf8");

    const result = readConfigTiers({
      userConfigPath: userPath,
      projectConfigPath: projectPath,
    });

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({
      tier: "project",
      source: resolve(projectPath),
      doc: projectDoc,
    });
  });
});

describe("formatDroppedKeyWarnings", () => {
  test("maps dropped keys to readable warning strings", () => {
    const dropped = [
      {
        key: "semantic.backend",
        tier: "project",
        reason: "security: use user config for external backends",
      },
      {
        key: "lsp.servers",
        tier: "project",
        reason: "security: these LSP settings only honor user-level config",
      },
    ];

    const warnings = formatDroppedKeyWarnings(dropped);

    expect(warnings).toEqual([
      "Ignoring semantic.backend from project config (security: use user config for external backends)",
      "Ignoring lsp.servers from project config (security: these LSP settings only honor user-level config)",
    ]);
  });

  test("returns empty array for empty input", () => {
    expect(formatDroppedKeyWarnings([])).toEqual([]);
    expect(formatDroppedKeyWarnings(null as any)).toEqual([]);
    expect(formatDroppedKeyWarnings(undefined as any)).toEqual([]);
  });
});
