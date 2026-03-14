import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for refactoring commands: move_symbol.
 * S02 will extend this with extract_function and inline_symbol.
 */
export function refactoringTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    aft_move_symbol: {
      description:
        "Move a top-level symbol (function, class, type, const, etc.) from one file to another. Updates all import statements across the workspace so consumers point to the new location. Supports dry_run mode to preview changes without writing. Creates a checkpoint before mutations for rollback safety. Requires aft_configure to be called first.",
      args: {
        file: z.string().describe("Path to the source file containing the symbol (relative to project root or absolute)"),
        symbol: z.string().describe("Name of the top-level symbol to move"),
        destination: z.string().describe("Path to the destination file (will be created if it doesn't exist)"),
        scope: z
          .string()
          .optional()
          .describe("Disambiguation scope when multiple symbols share the same name"),
        dry_run: z
          .boolean()
          .optional()
          .describe("If true, returns a preview diff without modifying files on disk"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
          destination: args.destination,
        };
        if (args.scope !== undefined) params.scope = args.scope;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("move_symbol", params);
        return JSON.stringify(response);
      },
    },
  };
}
