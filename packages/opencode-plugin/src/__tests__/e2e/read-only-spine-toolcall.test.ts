/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import type { BridgePool } from "@cortexkit/aft-bridge";
import type { ToolContext } from "@opencode-ai/plugin";
import { inspectTools } from "../../tools/inspect.js";
import { searchTools } from "../../tools/search.js";
import { semanticTools } from "../../tools/semantic.js";
import type { PluginContext } from "../../types.js";
import { noopAsk } from "../test-helpers";
import {
  cleanupHarnesses,
  createHarness,
  type E2EHarness,
  type PreparedBinary,
  prepareBinary,
} from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = describe.skipIf(!initialBinary.binaryPath);

function createMockClient(): PluginContext["client"] {
  return { lsp: {}, find: {} } as PluginContext["client"];
}

function createPluginContext(harness: E2EHarness): PluginContext {
  const pool = { getBridge: () => harness.bridge } as unknown as BridgePool;
  return {
    pool,
    client: createMockClient(),
    config: {} as PluginContext["config"],
    storageDir: harness.path(".aft-test-storage"),
  };
}

function createToolContext(harness: E2EHarness): ToolContext {
  return {
    sessionID: "read-only-spine-toolcall-e2e",
    messageID: "read-only-spine-toolcall-message",
    agent: "test",
    directory: harness.tempDir,
    worktree: harness.tempDir,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: noopAsk,
  } as ToolContext;
}

async function createFixtureProject(harness: E2EHarness): Promise<void> {
  await mkdir(harness.path("src"), { recursive: true });
  await Promise.all([
    writeFile(
      harness.path("src", "hit.ts"),
      [
        "export const toolCallGrepMarker = 'tool_call_grep_marker';",
        "export const toolCallSearchMarker = 'tool_call_search_marker';",
        "// TODO cutover inspect marker",
        "",
      ].join("\n"),
      "utf8",
    ),
    writeFile(harness.path("src", "other.ts"), "export const unrelated = true;\n", "utf8"),
  ]);
}

maybeDescribe("e2e read-only spine tool_call cutover", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    await cleanupHarnesses(harnesses);
  });

  async function harness(): Promise<E2EHarness> {
    const created = await createHarness(preparedBinary, {
      fixtureNames: [],
      timeoutMs: 20_000,
      tempPrefix: "aft-plugin-readonly-spine-",
    });
    harnesses.push(created);
    await createFixtureProject(created);
    return created;
  }

  test("grep returns server-rendered matches through tool_call", async () => {
    const h = await harness();
    const tools = searchTools(createPluginContext(h));

    const output = await tools.grep.execute(
      { pattern: "tool_call_grep_marker", path: "src" },
      createToolContext(h),
    );

    expect(output).toContain("tool_call_grep_marker");
    expect(output).toContain("Found 1 match across 1 file");
  });

  test("grep appends the plugin-side skipped path footer after tool_call", async () => {
    const h = await harness();
    const tools = searchTools(createPluginContext(h));

    const output = await tools.grep.execute(
      { pattern: "tool_call_grep_marker", path: "src missing-dir" },
      createToolContext(h),
    );

    expect(output).toContain("tool_call_grep_marker");
    expect(output).toContain("Found 1 match across 1 file");
    expect(output).toContain("Skipped 1 path not found: missing-dir");
  });

  test("aft_search returns server-rendered literal search text through tool_call", async () => {
    const h = await harness();
    const tools = semanticTools(createPluginContext(h));

    const output = await tools.aft_search.execute(
      { query: "tool_call_search_marker", hint: "literal", topK: 5 },
      createToolContext(h),
    );

    expect(output).toContain("tool_call_search_marker");
    expect(output).toContain("Found ");
  });

  test("aft_inspect returns server-rendered todos details through tool_call", async () => {
    const h = await harness();
    const tools = inspectTools(createPluginContext(h));

    const output = await tools.aft_inspect.execute(
      { sections: "todos", topK: 5 },
      createToolContext(h),
    );

    expect(output).toContain("TODOs: 1");
    expect(output).toContain("src/hit.ts:3 TODO cutover inspect marker");
  });
});
