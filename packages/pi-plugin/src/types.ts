import type { AftConfig } from "./config.js";
import type { BridgePool } from "./pool.js";

/**
 * Shared context passed to every tool wrapper.
 * Bundles the bridge pool, the resolved AFT config, and the storage dir.
 */
export interface PluginContext {
  pool: BridgePool;
  config: AftConfig;
  /** Absolute path to AFT's storage dir (e.g. ~/.pi/agent/aft) */
  storageDir: string;
}
