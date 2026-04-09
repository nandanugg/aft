type BridgeLike = {
  getBridge: (
    directory: string,
    sessionID: string,
  ) => {
    send: (command: string, params?: Record<string, unknown>) => Promise<Record<string, unknown>>;
  };
};

const GLOBAL_KEY = "__AFT_SHARED_BRIDGE_POOL__";

function getGlobalState(): { [GLOBAL_KEY]?: BridgeLike | null } {
  return globalThis as { [GLOBAL_KEY]?: BridgeLike | null };
}

export function setSharedBridgePool(pool: BridgeLike): void {
  getGlobalState()[GLOBAL_KEY] = pool;
}

export function getSharedBridgePool(): BridgeLike | null {
  return getGlobalState()[GLOBAL_KEY] ?? null;
}

export function clearSharedBridgePool(): void {
  getGlobalState()[GLOBAL_KEY] = null;
}
