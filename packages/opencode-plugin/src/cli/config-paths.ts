import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

export interface ConfigPaths {
  configDir: string;
  opencodeConfig: string;
  opencodeConfigFormat: "json" | "jsonc" | "none";
  aftConfig: string;
  aftConfigFormat: "json" | "jsonc" | "none";
  tuiConfig: string;
  tuiConfigFormat: "json" | "jsonc" | "none";
}

function getConfigDir(): string {
  const envDir = process.env.OPENCODE_CONFIG_DIR?.trim();
  if (envDir) {
    return resolve(envDir);
  }

  const xdgConfigHome = process.env.XDG_CONFIG_HOME || join(homedir(), ".config");
  return join(xdgConfigHome, "opencode");
}

function detectConfigFile(
  configDir: string,
  name: string,
): {
  path: string;
  format: "json" | "jsonc" | "none";
} {
  const jsoncPath = join(configDir, `${name}.jsonc`);
  const jsonPath = join(configDir, `${name}.json`);

  if (existsSync(jsoncPath)) {
    return { path: jsoncPath, format: "jsonc" };
  }
  if (existsSync(jsonPath)) {
    return { path: jsonPath, format: "json" };
  }
  return { path: jsonPath, format: "none" };
}

export function detectConfigPaths(): ConfigPaths {
  const configDir = getConfigDir();
  const opencodeConfig = detectConfigFile(configDir, "opencode");
  const aftConfig = detectConfigFile(configDir, "aft");
  const tuiConfig = detectConfigFile(configDir, "tui");

  return {
    configDir,
    opencodeConfig: opencodeConfig.path,
    opencodeConfigFormat: opencodeConfig.format,
    aftConfig: aftConfig.path,
    aftConfigFormat: aftConfig.format,
    tuiConfig: tuiConfig.path,
    tuiConfigFormat: tuiConfig.format,
  };
}
