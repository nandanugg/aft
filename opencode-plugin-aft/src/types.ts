import type { PluginInput } from "@opencode-ai/plugin";
import type { BinaryBridge } from "./bridge.js";

/**
 * Shared context passed to all tool factory functions.
 * Bundles the binary bridge and the OpenCode SDK client.
 */
export interface ToolContext {
  bridge: BinaryBridge;
  client: PluginInput["client"];
}
