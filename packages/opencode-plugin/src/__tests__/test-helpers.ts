/// <reference path="../bun-test.d.ts" />
/**
 * Shared test helpers for OpenCode plugin tests.
 *
 * Why this exists: OpenCode's `ToolContext.ask()` returns `Effect.Effect<void>`
 * (since plugin SDK v1.14). Test mocks that returned `Promise<void>` would
 * silently no-op under the old buggy `await effect` path, but now that
 * `runAsk` calls `Effect.runPromise`, returning a Promise causes
 * `Fiber.runLoop: Not a valid effect: [object Promise]`. These helpers
 * give tests an Effect-shaped ask mock by default.
 */
import { mock } from "bun:test";
import type { ToolContext, ToolResult } from "@opencode-ai/plugin";
import { Effect } from "effect";

const VOID_EFFECT = Effect.asVoid(Effect.succeed(0));

/**
 * Normalize a `ToolResult` (SDK >=1.14 widened this from `string` to
 * `string | { output: string }`) down to the agent-visible string for
 * test assertions like `.toContain` / `.toBe`.
 */
export function toolResultText(result: ToolResult): string {
  return typeof result === "string" ? result : result.output;
}

/** No-op `ctx.ask` that resolves cleanly through `runAsk`. */
export const noopAsk: ToolContext["ask"] = () => VOID_EFFECT;

/**
 * Like `mock(async () => {})` but returns an Effect so it survives
 * `Effect.runPromise`. Use when a test needs to inspect call args.
 */
export function mockAsk(): ReturnType<typeof mock> & ToolContext["ask"] {
  return mock(() => VOID_EFFECT) as unknown as ReturnType<typeof mock> & ToolContext["ask"];
}

/**
 * Build an Effect-shaped ask mock that rejects (simulating a deny).
 * The error message is what `askEditPermission` surfaces to callers.
 */
export function mockAskDeny(
  message: string = "Permission denied.",
): ReturnType<typeof mock> & ToolContext["ask"] {
  return mock(() => Effect.fail(new Error(message))) as unknown as ReturnType<typeof mock> &
    ToolContext["ask"];
}
