/**
 * E2E coverage for aft_outline + aft_zoom.
 */

/// <reference path="../../bun-test.d.ts" />

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { createHarness, type Harness, prepareBinary } from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = initialBinary.binaryPath ? describe : describe.skip;

maybeDescribe("aft_outline + aft_zoom (real bridge)", () => {
  let harness: Harness;

  beforeAll(async () => {
    harness = await createHarness(initialBinary);
  });

  afterAll(async () => {
    if (harness) await harness.cleanup();
  });

  test("outline single file — sample.ts lists functions and class", async () => {
    const result = await harness.callTool("aft_outline", { filePath: "sample.ts" });
    const text = harness.text(result);
    expect(text).toContain("funcA");
    expect(text).toContain("funcB");
    expect(text).toContain("SampleService");
  });

  test("outline batched files", async () => {
    const result = await harness.callTool("aft_outline", {
      files: [harness.path("sample.ts"), harness.path("imports.ts")],
    });
    const text = harness.text(result);
    expect(text).toContain("sample.ts");
    expect(text).toContain("imports.ts");
  });

  test("outline directory", async () => {
    const result = await harness.callTool("aft_outline", { directory: "." });
    const text = harness.text(result);
    expect(text).toContain("sample.ts");
    // Go file should be included
    expect(text).toContain("sample.go");
  });

  test("outline rejects multiple exclusive params", async () => {
    await expect(
      harness.callTool("aft_outline", { filePath: "sample.ts", directory: "." }),
    ).rejects.toThrow(/Provide exactly ONE/);
  });

  test("outline rejects no params", async () => {
    await expect(harness.callTool("aft_outline", {})).rejects.toThrow(/Provide exactly one/);
  });

  test("outline auto-detects directory passed as filePath", async () => {
    const result = await harness.callTool("aft_outline", { filePath: "directory" });
    const text = harness.text(result);
    // Directory mode returned (tree output) — real content depends on fixture
    expect(text.length).toBeGreaterThan(0);
  });

  test("zoom into single symbol returns source", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbol: "funcB",
    });
    const text = harness.text(result);
    expect(text).toContain("funcB");
    expect(text).toContain("normalize");
  });

  test("zoom multi-symbol returns array", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbols: ["funcA", "funcB"],
    });
    const text = harness.text(result);
    // Array-shaped JSON: two results
    expect(text).toContain("funcA");
    expect(text).toContain("funcB");
  });

  test("zoom with contextLines expands range", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbol: "funcA",
      contextLines: 10,
    });
    const text = harness.text(result);
    expect(text).toContain("funcA");
  });
});
