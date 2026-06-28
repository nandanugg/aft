/**
 * Unit tests for aft_refactor tool_call argument shaping.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { registerRefactorTool } from "../tools/refactor.js";
import { executeTool, makeMockApi, makeMockBridge, makePluginContext } from "./tool-test-utils.js";

describe("aft_refactor adapter", () => {
  test("extract forwards inclusive Pi endLine for server translation", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerRefactorTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_refactor")!, {
      op: "extract",
      filePath: "src/app.ts",
      name: "computeTotal",
      startLine: 10,
      endLine: 12,
    });

    expect(calls[0].command).toBe("tool_call");
    expect(calls[0].params).toMatchObject({
      name: "refactor",
      arguments: {
        op: "extract",
        filePath: "src/app.ts",
        name: "computeTotal",
        startLine: 10,
        endLine: 12,
      },
    });
  });

  test("inline forwards callSiteLine and non-extract endLine unchanged", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerRefactorTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_refactor")!, {
      op: "inline",
      filePath: "src/app.ts",
      symbol: "helper",
      callSiteLine: 44,
      endLine: 50,
    });

    expect(calls[0].command).toBe("tool_call");
    expect(calls[0].params).toMatchObject({
      name: "refactor",
      arguments: {
        op: "inline",
        filePath: "src/app.ts",
        symbol: "helper",
        callSiteLine: 44,
        endLine: 50,
      },
    });
  });

  test("move forwards symbol destination and disambiguating scope", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerRefactorTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_refactor")!, {
      op: "move",
      filePath: "src/app.ts",
      symbol: "Service",
      destination: "src/service.ts",
      scope: "exports",
    });

    expect(calls[0].command).toBe("tool_call");
    expect(calls[0].params).toMatchObject({
      name: "refactor",
      arguments: {
        op: "move",
        filePath: "src/app.ts",
        symbol: "Service",
        destination: "src/service.ts",
        scope: "exports",
      },
    });
  });

  test("extract and inline reject missing required numeric params before bridge dispatch", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true }));
    registerRefactorTool(api, makePluginContext(bridge));

    await expect(
      executeTool(tools.get("aft_refactor")!, {
        op: "extract",
        filePath: "src/app.ts",
        name: "computeTotal",
        endLine: 12,
      }),
    ).rejects.toThrow("'startLine' is required for 'extract' op");

    await expect(
      executeTool(tools.get("aft_refactor")!, {
        op: "inline",
        filePath: "src/app.ts",
        symbol: "helper",
      }),
    ).rejects.toThrow("'callSiteLine' is required for 'inline' op");

    expect(calls).toHaveLength(0);
  });
});
