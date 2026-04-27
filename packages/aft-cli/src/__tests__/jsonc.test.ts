/// <reference path="../bun-test.d.ts" />

import { beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { detectJsoncFile, readJsoncFile, writeJsoncFile } from "../lib/jsonc.js";

describe("jsonc", () => {
  let dir: string;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "aft-cli-jsonc-"));
    mkdirSync(dir, { recursive: true });
  });

  test("detectJsoncFile returns .jsonc when both exist", () => {
    writeFileSync(join(dir, "aft.jsonc"), "{}");
    writeFileSync(join(dir, "aft.json"), "{}");
    const detected = detectJsoncFile(dir, "aft");
    expect(detected.format).toBe("jsonc");
  });

  test("detectJsoncFile falls back to .json", () => {
    writeFileSync(join(dir, "aft.json"), "{}");
    const detected = detectJsoncFile(dir, "aft");
    expect(detected.format).toBe("json");
  });

  test("detectJsoncFile returns none when neither exists", () => {
    const detected = detectJsoncFile(dir, "aft");
    expect(detected.format).toBe("none");
    expect(detected.path.endsWith(".json")).toBe(true);
  });

  test("readJsoncFile parses comment-stripped JSON", () => {
    const path = join(dir, "x.jsonc");
    writeFileSync(path, '// a comment\n{\n  "key": 1\n}\n');
    const { value, error } = readJsoncFile(path);
    expect(error).toBeUndefined();
    expect(value?.key).toBe(1);
  });

  test("readJsoncFile returns error on bad content", () => {
    const path = join(dir, "bad.jsonc");
    writeFileSync(path, "{not json}");
    const { value, error } = readJsoncFile(path);
    expect(value).toBeNull();
    expect(error).toBeDefined();
  });

  test("writeJsoncFile creates parent dirs", () => {
    const path = join(dir, "nested", "deep", "out.json");
    writeJsoncFile(path, { foo: 1 });
    const raw = readFileSync(path, "utf-8");
    expect(JSON.parse(raw)).toEqual({ foo: 1 });
  });
});
