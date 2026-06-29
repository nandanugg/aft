import type { AftTransportPool } from "@cortexkit/aft-bridge";
import type { AftConfig } from "./config.js";

/**
 * Shared context passed to every tool wrapper.
 * Bundles the bridge pool, the resolved AFT config, and the storage dir.
 *
 * Note: session ID is resolved per tool call from Pi's `ExtensionContext`
 * (`sessionManager.getSessionId()`) rather than stored here, so that
 * `/new`, `/fork`, and `/resume` each scope their own undo/checkpoint
 * state in AFT.
 */
export interface PluginContext {
  pool: AftTransportPool;
  config: AftConfig;
  /** Absolute path to AFT's data storage dir (e.g. ~/.local/share/cortexkit/aft). */
  storageDir: string;
}
