/**
 * Shared platform / architecture mappings used by both the resolver and downloader.
 */

export const PLATFORM_ARCH_MAP: Record<string, Record<string, string>> = {
  darwin: { arm64: "darwin-arm64", x64: "darwin-x64" },
  linux: { arm64: "linux-arm64", x64: "linux-x64" },
  win32: { x64: "win32-x64" },
};

export const PLATFORM_ASSET_MAP: Record<string, string> = {
  "darwin-arm64": "aft-darwin-arm64",
  "darwin-x64": "aft-darwin-x64",
  "linux-arm64": "aft-linux-arm64",
  "linux-x64": "aft-linux-x64",
  "win32-x64": "aft-win32-x64.exe",
};
