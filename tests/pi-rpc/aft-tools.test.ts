import { describe, expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  cleanupPiIsolatedEnv,
  createPiIsolatedEnv,
  type PiIsolatedEnv,
  type RpcClient,
  resolvePiPluginDir,
  spawnPiRpc,
  startAimock,
} from "./helpers";

async function enableAllToolSurface(env: PiIsolatedEnv): Promise<void> {
  await mkdir(join(env.workdir, ".pi"), { recursive: true });
  await writeFile(
    join(env.workdir, ".pi", "aft.jsonc"),
    JSON.stringify({ tool_surface: "all", search_index: true, semantic_search: false }),
    "utf8",
  );
}

async function enableAllToolSurfaceWithSearch(env: PiIsolatedEnv): Promise<void> {
  await mkdir(join(env.workdir, ".pi"), { recursive: true });
  await writeFile(
    join(env.workdir, ".pi", "aft.jsonc"),
    JSON.stringify({ tool_surface: "all", search_index: true, semantic_search: true }),
    "utf8",
  );
}

function resultText(event: Record<string, unknown>): string {
  const result = event.result;
  if (result && typeof result === "object" && !Array.isArray(result)) {
    const content = (result as { content?: unknown }).content;
    if (Array.isArray(content)) {
      return content
        .map((part) => {
          if (!part || typeof part !== "object" || Array.isArray(part)) return "";
          const text = (part as { text?: unknown }).text;
          return typeof text === "string" ? text : "";
        })
        .filter((text) => text.length > 0)
        .join("\n");
    }
  }
  return JSON.stringify(result ?? "");
}

async function withPiTool(
  toolCall: { name: string; arguments: Record<string, unknown> },
  opts: {
    message: string;
    setup?: (env: PiIsolatedEnv) => Promise<void>;
  },
): Promise<Record<string, unknown>> {
  const env = createPiIsolatedEnv();
  const aimock = await startAimock();
  let client: RpcClient | undefined;
  try {
    await opts.setup?.(env);
    aimock.registerToolCallFixture({
      predicate: () => true,
      toolCalls: [toolCall],
      followupText: "Done.",
    });
    const spawned = spawnPiRpc({
      mockProviderURL: aimock.url,
      aftPluginDir: resolvePiPluginDir(),
      configDir: env.configDir,
      workdir: env.workdir,
    });
    client = spawned.client;
    expect(spawned.child.pid).toBeGreaterThan(0);
    expect((await client.sendCommand({ type: "prompt", message: opts.message })).success).toBe(
      true,
    );
    return await client.waitForEvent(
      (event) => event.type === "tool_execution_end" && event.toolName === toolCall.name,
      30_000,
    );
  } finally {
    await client?.close();
    await aimock.close();
    await cleanupPiIsolatedEnv(env);
  }
}

describe("AFT Pi tools (real Pi RPC)", () => {
  test("aft_callgraph trace_to_symbol returns a reachable path", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_callgraph",
        arguments: {
          op: "trace_to_symbol",
          filePath: "trace.ts",
          symbol: "source",
          toSymbol: "target",
          toFile: "trace.ts",
        },
      },
      {
        message: "Trace source to target in trace.ts.",
        setup: async (env) => {
          await enableAllToolSurface(env);
          await writeFile(
            join(env.workdir, "trace.ts"),
            [
              "export function source(): string { return middle(); }",
              "function middle(): string { return target(); }",
              "export function target(): string { return 'target'; }",
              "",
            ].join("\n"),
            "utf8",
          );
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    // aft_callgraph returns flat text to the agent (structured data is carried
    // in `details` for richer renderers). Assert on the flat output.
    const text = resultText(toolEnd);
    const hopMatch = text.match(/(\d+) hops?/);
    expect(hopMatch).not.toBeNull();
    expect(Number(hopMatch?.[1])).toBe(3);
    // Hop order: source → middle → target, in that sequence.
    const sourceIdx = text.indexOf("source");
    const middleIdx = text.indexOf("middle");
    const targetIdx = text.indexOf("target");
    expect(sourceIdx).toBeGreaterThanOrEqual(0);
    expect(middleIdx).toBeGreaterThan(sourceIdx);
    expect(targetIdx).toBeGreaterThan(middleIdx);
  }, 120_000);

  test("aft_outline files mode returns directory file metadata", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_outline",
        arguments: {
          target: "src",
          files: true,
        },
      },
      {
        message: "Outline files under src.",
        setup: async (env) => {
          await mkdir(join(env.workdir, "src"), { recursive: true });
          await writeFile(
            join(env.workdir, "src", "one.ts"),
            "export function one() { return 1; }\n",
            "utf8",
          );
          await writeFile(join(env.workdir, "src", "two.py"), "def two():\n    return 2\n", "utf8");
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const text = resultText(toolEnd);
    expect(text).toMatch(/one\.ts\s+typescript\s+1 syms\s+\d+ bytes/);
    expect(text).toMatch(/two\.py\s+python\s+1 syms\s+\d+ bytes/);
  }, 120_000);

  test("aft_outline file mode returns server-rendered symbol text", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_outline",
        arguments: { target: "src/one.ts" },
      },
      {
        message: "Outline src/one.ts.",
        setup: async (env) => {
          await enableAllToolSurface(env);
          await mkdir(join(env.workdir, "src"), { recursive: true });
          await writeFile(
            join(env.workdir, "src", "one.ts"),
            "export function one() { return 1; }\n",
            "utf8",
          );
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const text = resultText(toolEnd);
    expect(text).toContain("one.ts");
    expect(text).toContain("function one");
  }, 120_000);

  test("aft_search returns Rust text for literal fallback search", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_search",
        arguments: { query: "needle", hint: "literal", topK: 5, includeTests: true },
      },
      {
        message: "Search for needle.",
        setup: async (env) => {
          await enableAllToolSurfaceWithSearch(env);
          await writeFile(
            join(env.workdir, "needle.ts"),
            "export const value = 'needle';\n",
            "utf8",
          );
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const text = resultText(toolEnd);
    expect(text).toContain("needle.ts");
    expect(text).toContain("Found 1 match");
  }, 120_000);

  test("aft_inspect returns server-rendered health text", async () => {
    const toolEnd = await withPiTool(
      {
        name: "aft_inspect",
        arguments: { sections: "todos", topK: 5 },
      },
      {
        message: "Inspect the project.",
        setup: async (env) => {
          await enableAllToolSurface(env);
          await writeFile(
            join(env.workdir, "todo.ts"),
            "// TODO: check me\nexport const value = 1;\n",
            "utf8",
          );
        },
      },
    );

    expect(toolEnd.isError).toBe(false);
    const text = resultText(toolEnd);
    expect(text).toContain("TODO");
    expect(text).not.toContain('"summary"');
  }, 120_000);

  test("aft_callgraph callers returns text and soft symbol_not_found stays non-error", async () => {
    const env = createPiIsolatedEnv();
    const aimock = await startAimock();
    let client: RpcClient | undefined;
    try {
      await enableAllToolSurface(env);
      await writeFile(
        join(env.workdir, "callers.ts"),
        [
          "export function caller() { return target(); }",
          "export function target() { return 'ok'; }",
          "",
        ].join("\n"),
        "utf8",
      );
      aimock.registerToolCallFixture({
        predicate: () => true,
        toolCalls: [
          {
            name: "aft_callgraph",
            arguments: { op: "callers", filePath: "callers.ts", symbol: "target" },
          },
          {
            name: "aft_callgraph",
            arguments: { op: "callers", filePath: "callers.ts", symbol: "missingSymbol" },
          },
        ],
        followupText: "Done.",
      });
      const spawned = spawnPiRpc({
        mockProviderURL: aimock.url,
        aftPluginDir: resolvePiPluginDir(),
        configDir: env.configDir,
        workdir: env.workdir,
      });
      client = spawned.client;
      expect(
        (await client.sendCommand({ type: "prompt", message: "Run two callgraph checks." }))
          .success,
      ).toBe(true);
      const first = await client.waitForEvent(
        (event) => event.type === "tool_execution_end" && event.toolName === "aft_callgraph",
        30_000,
      );
      const second = await client.waitForEvent(
        (event) =>
          event.type === "tool_execution_end" &&
          event.toolName === "aft_callgraph" &&
          event.toolCallId !== first.toolCallId,
        30_000,
      );

      expect(first.isError).toBe(false);
      expect(resultText(first)).toContain("caller");
      expect(second.isError).toBe(false);
      expect(resultText(second)).toContain("symbol_not_found");
    } finally {
      await client?.close();
      await aimock.close();
      await cleanupPiIsolatedEnv(env);
    }
  }, 120_000);
});
