/**
 * AFT reading tools: aft_outline + aft_zoom.
 * Structural overview and symbol/section inspection.
 */

import { stat } from "node:fs/promises";
import { resolve } from "node:path";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import { discoverSourceFiles } from "../shared/discover-files.js";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const OutlineParams = Type.Object({
  filePath: Type.Optional(
    Type.String({
      description: "Path to a single file to outline. Directories are auto-detected.",
    }),
  ),
  files: Type.Optional(
    Type.Array(Type.String(), { description: "Array of file paths to outline in one call" }),
  ),
  directory: Type.Optional(
    Type.String({ description: "Directory to outline recursively (200 file cap)" }),
  ),
});

const ZoomParams = Type.Object({
  filePath: Type.String({ description: "Path to file (absolute or project-relative)" }),
  symbol: Type.Optional(
    Type.String({ description: "Symbol name (function/class/type) or Markdown heading" }),
  ),
  symbols: Type.Optional(
    Type.Array(Type.String(), { description: "Multiple symbols — returns array of matches" }),
  ),
  contextLines: Type.Optional(
    Type.Number({ description: "Lines of context before/after (default: 3)" }),
  ),
});

export interface ReadingSurface {
  outline: boolean;
  zoom: boolean;
}

export function registerReadingTools(
  pi: ExtensionAPI,
  ctx: PluginContext,
  surface: ReadingSurface,
): void {
  if (surface.outline) {
    pi.registerTool({
      name: "aft_outline",
      label: "outline",
      description:
        "Structural outline of source code or Markdown. For code, returns symbols (functions, classes, types) with line ranges. For Markdown/HTML, returns heading hierarchy. Use this to explore structure before reading specific sections with aft_zoom.\n\nProvide exactly ONE of: `filePath`, `files`, or `directory`.",
      parameters: OutlineParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof OutlineParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const hasFilePath = typeof params.filePath === "string" && params.filePath.length > 0;
        const hasFiles = Array.isArray(params.files) && params.files.length > 0;
        const hasDirectory = typeof params.directory === "string" && params.directory.length > 0;

        const provided = [hasFilePath, hasFiles, hasDirectory].filter(Boolean).length;
        if (provided === 0) {
          throw new Error("Provide exactly one of 'filePath', 'files', or 'directory'");
        }
        if (provided > 1) {
          throw new Error(
            "Provide exactly ONE of 'filePath', 'files', or 'directory' — not multiple",
          );
        }

        // Auto-detect directory passed as filePath.
        let dirArg = hasDirectory ? params.directory : undefined;
        if (!dirArg && hasFilePath) {
          try {
            const resolved = resolve(extCtx.cwd, params.filePath as string);
            const st = await stat(resolved);
            if (st.isDirectory()) dirArg = params.filePath;
          } catch {
            // not a dir or missing — fall through
          }
        }

        if (dirArg) {
          const dirPath = resolve(extCtx.cwd, dirArg);
          const files = await discoverSourceFiles(dirPath);
          if (files.length === 0) {
            return textResult(`No source files found under ${dirArg}`);
          }
          const response = await callBridge(bridge, "outline", { files });
          return textResult((response.text as string | undefined) ?? "");
        }

        if (hasFiles) {
          const response = await callBridge(bridge, "outline", { files: params.files });
          return textResult((response.text as string | undefined) ?? "");
        }

        const response = await callBridge(bridge, "outline", { file: params.filePath });
        return textResult((response.text as string | undefined) ?? "");
      },
    });
  }

  if (surface.zoom) {
    pi.registerTool({
      name: "aft_zoom",
      label: "zoom",
      description:
        "Inspect a code symbol or Markdown/HTML section. For code, returns the full source of the symbol with call-graph annotations (calls/called-by). Pass `symbols` for batched lookups.",
      parameters: ZoomParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof ZoomParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);

        // Multi-symbol: fire in parallel and JSON-stringify the array.
        if (Array.isArray(params.symbols) && params.symbols.length > 0) {
          const results = await Promise.all(
            params.symbols.map((sym) => {
              const req: Record<string, unknown> = { file: params.filePath, symbol: sym };
              if (params.contextLines !== undefined) req.context_lines = params.contextLines;
              return bridge.send("zoom", req);
            }),
          );
          return textResult(JSON.stringify(results, null, 2));
        }

        const req: Record<string, unknown> = { file: params.filePath };
        if (params.symbol) req.symbol = params.symbol;
        if (params.contextLines !== undefined) req.context_lines = params.contextLines;
        const response = await callBridge(bridge, "zoom", req);
        return textResult(JSON.stringify(response, null, 2));
      },
    });
  }
}
