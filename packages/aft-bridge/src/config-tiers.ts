import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

export type ConfigTier = {
  tier: "user" | "project";
  source: string;
  doc: string;
};

export function readConfigTiers(opts: {
  userConfigPath: string;
  projectConfigPath: string;
}): ConfigTier[] {
  const tiers: ConfigTier[] = [];

  try {
    if (existsSync(opts.userConfigPath)) {
      const doc = readFileSync(opts.userConfigPath, "utf-8");
      tiers.push({
        tier: "user",
        source: resolve(opts.userConfigPath),
        doc,
      });
    }
  } catch {
    // Skip if unreadable or fails to read
  }

  try {
    if (existsSync(opts.projectConfigPath)) {
      const doc = readFileSync(opts.projectConfigPath, "utf-8");
      tiers.push({
        tier: "project",
        source: resolve(opts.projectConfigPath),
        doc,
      });
    }
  } catch {
    // Skip if unreadable or fails to read
  }

  return tiers;
}

/** Wrap an inline aft.jsonc-shaped config object as a single user-tier ConfigTier
 *  (the inline analog of readConfigTiers, which reads from disk). For programmatic
 *  callers / test harnesses that hold a config object rather than a file. */
export function inlineUserConfigTier(
  config: Record<string, unknown>,
  source = "<inline>",
): ConfigTier[] {
  return [{ tier: "user", source, doc: JSON.stringify(config) }];
}

export function formatDroppedKeyWarnings(
  dropped: Array<{ key: string; tier: string; reason: string }>,
): string[] {
  if (!dropped || !Array.isArray(dropped)) {
    return [];
  }
  return dropped.map((item) => `Ignoring ${item.key} from ${item.tier} config (${item.reason})`);
}
