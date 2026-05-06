import * as path from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
import { Effect } from "effect";

/**
 * Execute a `ctx.ask(...)` result.
 *
 * Why this exists: OpenCode's plugin contract returns `Effect.Effect<void>`
 * from `ask()` (since v1.14). Plain `await effect` resolves silently to the
 * Effect object without ever executing it — meaning the deny/ask evaluation
 * never runs and the user's `bash: { "*": deny }` (and edit/external_directory)
 * rules are silently ignored. The Effect must be run via `Effect.runPromise`.
 *
 * `effect` is marked external in our bun build and listed as a peerDependency,
 * so this import resolves at runtime to the same `effect` runtime that
 * `@opencode-ai/plugin` is using to construct the Effect. Bundling our own
 * `effect` would create a runtime instance mismatch where
 * `Effect.runPromise(...)` rejects with "Not a valid effect".
 *
 * On deny, `Effect.runPromise` rejects with the underlying defect
 * (DeniedError / RejectedError) so callers can rely on `try/catch` to
 * detect denial.
 */
export async function runAsk(maybe: Effect.Effect<void>): Promise<void> {
  await Effect.runPromise(maybe);
}

export function resolveAbsolutePath(context: ToolContext, target: string): string {
  return path.isAbsolute(target) ? target : path.resolve(context.directory, target);
}

export function resolveRelativePattern(context: ToolContext, target: string): string {
  return path.relative(context.worktree, resolveAbsolutePath(context, target)) || ".";
}

export function resolveRelativePatterns(context: ToolContext, targets: string[]): string[] {
  const seen = new Set<string>();
  const patterns: string[] = [];

  for (const target of targets) {
    if (!target) continue;
    const pattern = resolveRelativePattern(context, target);
    if (seen.has(pattern)) continue;
    seen.add(pattern);
    patterns.push(pattern);
  }

  return patterns;
}

export function workspacePattern(_context: ToolContext): string {
  return ".";
}

export async function askEditPermission(
  context: ToolContext,
  patterns: string[],
  metadata: Record<string, unknown> = {},
): Promise<string | undefined> {
  try {
    await runAsk(
      context.ask({
        permission: "edit",
        patterns: patterns.length > 0 ? patterns : [workspacePattern(context)],
        always: ["*"],
        metadata,
      }),
    );
    return undefined;
  } catch (error) {
    if (error instanceof Error && error.message) {
      return error.message;
    }
    return "Permission denied.";
  }
}

export function permissionDeniedResponse(message: string): string {
  return JSON.stringify({
    success: false,
    code: "permission_denied",
    message,
    error: message,
  });
}
