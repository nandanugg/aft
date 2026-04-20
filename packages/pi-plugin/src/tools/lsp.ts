/**
 * lsp_diagnostics — on-demand LSP diagnostics.
 * Edit/write flows already inject diagnostics inline; this tool is for
 * explicit checks on a file or directory.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const LspDiagnosticsParams = Type.Object({
  filePath: Type.Optional(
    Type.String({ description: "File to get diagnostics for (mutually exclusive with directory)" }),
  ),
  directory: Type.Optional(
    Type.String({
      description: "Directory to get diagnostics for (mutually exclusive with filePath)",
    }),
  ),
  severity: Type.Optional(
    StringEnum(["error", "warning", "information", "hint", "all"] as const, {
      description: "Filter by severity (default: all)",
    }),
  ),
  waitMs: Type.Optional(
    Type.Number({
      description: "Wait N ms for fresh diagnostics (max 10000, default: 0)",
    }),
  ),
});

export function registerLspTools(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "lsp_diagnostics",
    label: "lsp diagnostics",
    description:
      "Get errors, warnings, hints from a language server. Provide `filePath` for a single file, `directory` for all files under a path, or omit both for all tracked files.",
    parameters: LspDiagnosticsParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof LspDiagnosticsParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const hasFile = typeof params.filePath === "string" && params.filePath.length > 0;
      const hasDir = typeof params.directory === "string" && params.directory.length > 0;
      if (hasFile && hasDir) {
        throw new Error(
          "'filePath' and 'directory' are mutually exclusive — provide one or neither",
        );
      }
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const req: Record<string, unknown> = {};
      if (hasFile) req.file = params.filePath;
      if (hasDir) req.directory = params.directory;
      if (params.severity !== undefined) req.severity = params.severity;
      if (params.waitMs !== undefined) req.wait_ms = params.waitMs;
      const response = await callBridge(bridge, "lsp_diagnostics", req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
