import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callToolCall, isEmptyParam, resolvePathArg } from "./_shared.js";
import { assertExternalDirectoryPermission, permissionDeniedResponse } from "./permissions.js";

const z = tool.schema;

type ToolArg = ToolDefinition["args"][string];

type StringOrStringArray = string | string[];

function arg(schema: unknown): ToolArg {
  return schema as ToolArg;
}

function normalizeStringOrArray(value: unknown): StringOrStringArray | undefined {
  return isEmptyParam(value) ? undefined : (value as StringOrStringArray);
}

async function resolveAndGateScope(
  ctx: PluginContext,
  context: Parameters<ToolDefinition["execute"]>[1],
  scope: StringOrStringArray | undefined,
): Promise<{ scope: StringOrStringArray | undefined; denial?: string }> {
  if (scope === undefined) return { scope: undefined };
  const values = Array.isArray(scope) ? scope : [scope];
  const resolved = await Promise.all(
    values
      .filter((value): value is string => typeof value === "string" && value.length > 0)
      .map((value) => resolvePathArg(ctx, context, value)),
  );
  const checked = new Set<string>();
  for (const target of resolved) {
    if (checked.has(target)) continue;
    checked.add(target);
    const denial = await assertExternalDirectoryPermission(ctx, context, target);
    if (denial) return { scope: undefined, denial };
  }
  return { scope: Array.isArray(scope) ? resolved : resolved[0] };
}

export interface InspectToolConfig {
  tool_surface?: "minimal" | "recommended" | "all";
  disabled_tools?: string[];
  inspect?: {
    enabled?: boolean;
    tier2_idle_minutes?: number;
  };
}

export function inspectToolSurfaceEnabled(config: InspectToolConfig): boolean {
  return (config.tool_surface ?? "recommended") !== "minimal" && config.inspect?.enabled !== false;
}

export function shouldRegisterInspectTool(config: InspectToolConfig): boolean {
  return (
    inspectToolSurfaceEnabled(config) && !(config.disabled_tools ?? []).includes("aft_inspect")
  );
}

type TimerHandle = ReturnType<typeof setTimeout>;

export interface InspectTier2IdleSchedulerOptions {
  isEnabled: () => boolean;
  idleMinutes: () => number | undefined;
  run: (sessionID: string) => Promise<void>;
  warn?: (message: string) => void;
  setTimer?: (callback: () => void, delayMs: number) => TimerHandle;
  clearTimer?: (timer: TimerHandle) => void;
}

export function createInspectTier2IdleScheduler(options: InspectTier2IdleSchedulerOptions) {
  const timers = new Map<string, TimerHandle>();
  const setTimer = options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
  const clearTimer = options.clearTimer ?? ((timer) => clearTimeout(timer));

  const clear = (sessionID: string): void => {
    const timer = timers.get(sessionID);
    if (!timer) return;
    clearTimer(timer);
    timers.delete(sessionID);
  };

  const clearAll = (): void => {
    for (const timer of timers.values()) {
      clearTimer(timer);
    }
    timers.clear();
  };

  const schedule = (sessionID: string): void => {
    if (!options.isEnabled()) return;
    clear(sessionID);
    const idleMinutes = options.idleMinutes() ?? 4;
    const delayMs = Math.max(0, idleMinutes * 60 * 1000);
    const timer = setTimer(() => {
      timers.delete(sessionID);
      options.run(sessionID).catch((err) => {
        options.warn?.(
          `inspect_tier2_run failed: ${err instanceof Error ? err.message : String(err)}`,
        );
      });
    }, delayMs);
    timers.set(sessionID, timer);
  };

  return { schedule, clear, clearAll };
}

export function inspectTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const inspectTool: ToolDefinition = {
    description:
      "Codebase health snapshot. One call returns summary stats for: TODOs, diagnostics, file/symbol metrics, dead code, unused exports, code duplicates. Pass `sections` for per-category drill-down details.\n\n" +
      "Categories run in tiers — Tier 1 (todos, metrics) return synchronously from cache. Tier 2 (dead_code, unused_exports, duplicates) waits for a fresh reuse scan up to a short deadline; if a category is still scanning the response reports `complete: false` with `pending_categories: [...]` rather than a fabricated clean count.\n\n" +
      "Use when: starting work on unfamiliar code, after multi-edit batches to check diagnostics, before a refactor, before review, or to verify cleanup completeness.\n\n" +
      "Treat `dead_code` as a hint, not proof: reachability is call-based, so symbols reached only via method dispatch or referenced only in type position may be false positives — verify before deleting.",
    args: {
      sections: arg(
        z
          .union([z.string(), z.array(z.string())])
          .optional()
          .describe(
            "Categories to include in detailed drill-down (e.g. 'todos' or ['todos', 'dead_code']). Use 'all' for every active category. Omit for summary-only mode.",
          ),
      ),
      scope: arg(
        z
          .union([z.string(), z.array(z.string())])
          .optional()
          .describe(
            "Restrict scan/results to paths under this scope (file or directory, absolute or relative to project root). Tier 1 scopes the scan; Tier 2 scans project-wide and applies scope as a result filter.",
          ),
      ),
      topK: arg(
        z
          .number()
          .int()
          .positive()
          .max(100)
          .optional()
          .describe("Max drill-down items per category. Default 20, max 100."),
      ),
    },
    execute: async (args, context): Promise<string> => {
      const sections = normalizeStringOrArray(args.sections);
      const scoped = await resolveAndGateScope(ctx, context, normalizeStringOrArray(args.scope));
      if (scoped.denial) return permissionDeniedResponse(scoped.denial);
      const scope = scoped.scope;
      const topK = args.topK === undefined || args.topK === null ? undefined : args.topK;

      const rawArgs: Record<string, unknown> = {};
      if (sections !== undefined) rawArgs.sections = sections;
      if (scope !== undefined) rawArgs.scope = scope;
      if (topK !== undefined) rawArgs.topK = topK;
      const response = await callToolCall(ctx, context, "inspect", rawArgs, {
        keepBridgeOnTimeout: true,
      });
      if (response.success === false) {
        throw new Error((response.message as string) || "inspect failed");
      }
      return response.text;
    },
  };

  return {
    aft_inspect: inspectTool,
  };
}
