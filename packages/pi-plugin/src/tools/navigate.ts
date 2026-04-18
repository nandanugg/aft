/**
 * aft_navigate — call-graph navigation across files.
 * Ops: call_tree, callers, trace_to, impact, trace_data.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const NavigateParams = Type.Object({
  op: StringEnum(["call_tree", "callers", "trace_to", "impact", "trace_data"] as const, {
    description: "Navigation operation",
  }),
  filePath: Type.String({ description: "Source file containing the symbol" }),
  symbol: Type.String({ description: "Name of the symbol to analyze" }),
  depth: Type.Optional(Type.Number({ description: "Max traversal depth" })),
  expression: Type.Optional(
    Type.String({ description: "Expression to track (required for trace_data)" }),
  ),
});

export function registerNavigateTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_navigate",
    label: "navigate",
    description:
      "Navigate code structure across files using call graph analysis. All ops require both `filePath` and `symbol`. Use `call_tree` for what a function calls, `callers` for call sites, `trace_to` for entry points, `impact` for blast radius, `trace_data` to follow a value.",
    parameters: NavigateParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof NavigateParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      if (params.op === "trace_data" && !params.expression) {
        throw new Error("op='trace_data' requires an `expression`");
      }
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {
        op: params.op,
        file: params.filePath,
        symbol: params.symbol,
      };
      if (params.depth !== undefined) req.depth = params.depth;
      if (params.expression !== undefined) req.expression = params.expression;
      const response = await callBridge(bridge, params.op, req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
