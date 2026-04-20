/**
 * aft_delete + aft_move — filesystem ops with per-file backup.
 * Both go through Rust so backups and checkpoint rollback work the same way.
 */

import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const DeleteParams = Type.Object({
  filePath: Type.String({ description: "Path to file to delete" }),
});

const MoveParams = Type.Object({
  filePath: Type.String({ description: "Source file path to move" }),
  destination: Type.String({ description: "Destination file path" }),
});

export interface FsSurface {
  delete: boolean;
  move: boolean;
}

export function registerFsTools(pi: ExtensionAPI, ctx: PluginContext, surface: FsSurface): void {
  if (surface.delete) {
    pi.registerTool({
      name: "aft_delete",
      label: "delete",
      description:
        "Delete a file with backup. The file content is backed up before deletion — use `aft_safety undo` to recover.",
      parameters: DeleteParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof DeleteParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const response = await callBridge(bridge, "delete_file", { file: params.filePath });
        return textResult(`Deleted ${params.filePath}`, response);
      },
    });
  }

  if (surface.move) {
    pi.registerTool({
      name: "aft_move",
      label: "move",
      description:
        "Move or rename a file with backup. Creates parent directories for the destination automatically. This operates on files at the OS level — to relocate a code symbol between files, use `aft_refactor` with op='move' instead.",
      parameters: MoveParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof MoveParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const response = await callBridge(bridge, "move_file", {
          file: params.filePath,
          destination: params.destination,
        });
        return textResult(`Moved ${params.filePath} → ${params.destination}`, response);
      },
    });
  }
}
