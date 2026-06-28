/**
 * Unit tests for aft_conflicts tool_call dispatch.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { registerConflictsTool } from "../tools/conflicts.js";
import { executeTool, makeMockApi, makeMockBridge, makePluginContext } from "./tool-test-utils.js";

describe("aft_conflicts adapter", () => {
  test("calls tool_call conflicts with an empty request and returns server text", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "No conflicts" }));
    registerConflictsTool(api, makePluginContext(bridge));

    const result = (await executeTool(tools.get("aft_conflicts")!, {})) as {
      content: Array<{ text: string }>;
    };

    expect(calls[0]).toMatchObject({
      command: "tool_call",
      params: { name: "conflicts", arguments: {} },
    });
    expect(result.content[0].text).toBe("No conflicts");
  });

  test("forwards path to tool_call conflicts when provided", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerConflictsTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_conflicts")!, { path: "/tmp/other-worktree" });

    expect(calls[0]).toMatchObject({
      command: "tool_call",
      params: { name: "conflicts", arguments: { path: "/tmp/other-worktree" } },
    });
  });

  test("omits path from request when blank", async () => {
    const { api, tools } = makeMockApi();
    const { bridge, calls } = makeMockBridge(() => ({ success: true, text: "ok" }));
    registerConflictsTool(api, makePluginContext(bridge));

    await executeTool(tools.get("aft_conflicts")!, { path: "   " });

    expect(calls[0]).toMatchObject({
      command: "tool_call",
      params: { name: "conflicts", arguments: {} },
    });
  });
});
