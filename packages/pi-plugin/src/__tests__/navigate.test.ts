/**
 * Unit tests for aft_navigate argument shaping.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { registerNavigateTool } from "../tools/navigate.js";
import { executeTool, makeMockApi, makeMockBridge, makePluginContext } from "./tool-test-utils.js";

async function expectRejectMessage(action: () => Promise<unknown>): Promise<string> {
  try {
    await action();
  } catch (error) {
    expect(error).toBeInstanceOf(Error);
    return (error as Error).message;
  }
  throw new Error("expected action to reject");
}

describe("aft_navigate adapter", () => {
  test("dispatches to the selected op and maps filePath to file", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, total_affected: 0 }));
    registerNavigateTool(api, makePluginContext(bridge));

    const result = (await executeTool(tools.get("aft_navigate")!, {
      op: "impact",
      filePath: "src/app.ts",
      symbol: "run",
      depth: 4,
    })) as { content: Array<{ type: string; text: string }> };

    expect(result.content[0]?.text).toContain('"success": true');
    expect(result.content[0]?.text).toContain('"total_affected": 0');
    expect(calls[0].command).toBe("impact");
    expect(calls[0].params).toEqual({
      op: "impact",
      file: "src/app.ts",
      symbol: "run",
      depth: 4,
    });
  });

  test("trace_data requires expression before bridge dispatch", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge();
    registerNavigateTool(api, makePluginContext(bridge));

    await expect(
      executeTool(tools.get("aft_navigate")!, {
        op: "trace_data",
        filePath: "src/app.ts",
        symbol: "run",
      }),
    ).rejects.toThrow("requires an `expression`");
    expect(calls).toHaveLength(0);
  });

  test("trace_data forwards expression when present", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true }));
    registerNavigateTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_navigate")!, {
      op: "trace_data",
      filePath: "src/app.ts",
      symbol: "run",
      expression: "config.apiKey",
    });

    expect(calls[0].command).toBe("trace_data");
    expect(calls[0].params).toMatchObject({ expression: "config.apiKey" });
  });

  test("trace_to_symbol requires and forwards target fields", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true }));
    registerNavigateTool(api, makePluginContext(bridge));

    await expect(
      executeTool(tools.get("aft_navigate")!, {
        op: "trace_to_symbol",
        filePath: "src/app.ts",
        symbol: "run",
      }),
    ).rejects.toThrow("toSymbol");

    await executeTool(tools.get("aft_navigate")!, {
      op: "trace_to_symbol",
      filePath: "src/app.ts",
      symbol: "run",
      toSymbol: "target",
      toFile: "src/target.ts",
      depth: 3,
    });

    expect(calls).toHaveLength(1);
    expect(calls[0].command).toBe("trace_to_symbol");
    expect(calls[0].params).toMatchObject({
      toSymbol: "target",
      toFile: "src/target.ts",
      depth: 3,
    });
  });

  test("trace_to_symbol ambiguous_target errors include toFile candidates", async () => {
    const { api, tools } = makeMockApi();
    const { bridge } = makeMockBridge(() => ({
      success: false,
      code: "ambiguous_target",
      message: 'multiple symbols named "target"',
      data: {
        candidates: [
          { file: "file1.rs", line: 42, symbol: "target" },
          { file: "file2.rs", line: 78, symbol: "target" },
        ],
      },
    }));
    registerNavigateTool(api, makePluginContext(bridge));

    const message = await expectRejectMessage(() =>
      executeTool(tools.get("aft_navigate")!, {
        op: "trace_to_symbol",
        filePath: "src/app.ts",
        symbol: "run",
        toSymbol: "target",
      }),
    );

    expect(message).toBe(
      'trace_to_symbol: ambiguous_target — multiple symbols named "target". Pass toFile to disambiguate:\n  - file1.rs:42\n  - file2.rs:78',
    );
  });

  test("generic bridge errors keep code, message, and data visible", async () => {
    const { api, tools } = makeMockApi();
    const { bridge } = makeMockBridge(() => ({
      success: false,
      code: "symbol_not_found",
      message: "symbol missing",
      data: { file: "src/app.ts", symbol: "run" },
    }));
    registerNavigateTool(api, makePluginContext(bridge));

    const message = await expectRejectMessage(() =>
      executeTool(tools.get("aft_navigate")!, {
        op: "callers",
        filePath: "src/app.ts",
        symbol: "run",
      }),
    );

    expect(message).toContain("callers: symbol_not_found — symbol missing");
    expect(message).toContain('"file": "src/app.ts"');
    expect(message).toContain('"symbol": "run"');
  });
});
