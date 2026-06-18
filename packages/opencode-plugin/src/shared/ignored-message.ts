import { resolvePromptContext } from "./last-assistant-model.js";

/**
 * Append an `ignored: true` synthetic user message to a session.
 *
 * Used for user-facing informational panels that must NOT trigger an agent
 * turn (e.g. status output, the external-directory restriction notice). The
 * message renders under the current agent (resolved from recent messages) so
 * it shows in the right place in the OpenCode UI, and carries `noReply: true`
 * so no LLM call is made.
 *
 * IMPORTANT (cache + crash safety): this path deliberately passes ONLY `agent`
 * (never model/variant). OpenCode crashes if model/variant are supplied on a
 * `noReply: true` prompt, and omitting them keeps the synthetic message from
 * busting the provider prefix cache the previous assistant turn warmed.
 *
 * Lives in `shared/` (not `index.ts`) because `index.ts` must export only the
 * plugin default; both the plugin entry and `tools/permissions.ts` call this.
 */
export async function sendIgnoredMessage(
  client: unknown,
  sessionID: string,
  text: string,
): Promise<void> {
  const typedClient = client as {
    session?: {
      prompt?: (input: unknown) => unknown;
      promptAsync?: (input: unknown) => unknown;
    };
  };

  let agent: string | undefined;
  try {
    const ctx = await resolvePromptContext(
      client as Parameters<typeof resolvePromptContext>[0],
      sessionID,
    );
    agent = ctx?.agent;
  } catch {
    agent = undefined;
  }

  const body: Record<string, unknown> = {
    noReply: true,
    parts: [{ type: "text", text, ignored: true }],
  };
  if (agent) body.agent = agent;
  const promptInput = { path: { id: sessionID }, body };

  if (typeof typedClient.session?.prompt === "function") {
    await Promise.resolve(typedClient.session.prompt(promptInput));
    return;
  }

  if (typeof typedClient.session?.promptAsync === "function") {
    await typedClient.session.promptAsync(promptInput);
    return;
  }

  throw new Error("[aft-plugin] client.session.prompt is unavailable");
}
