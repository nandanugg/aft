import { execSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

/** Supported platform package mapping: `process.platform`-`process.arch` → npm package suffix. */
const PLATFORM_MAP: Record<string, Record<string, string>> = {
  darwin: { arm64: "darwin-arm64", x64: "darwin-x64" },
  linux: { arm64: "linux-arm64", x64: "linux-x64" },
  win32: { x64: "win32-x64" },
};

/**
 * Map the current `process.platform` and `process.arch` to the npm platform
 * package suffix (e.g. `"darwin-arm64"`, `"linux-x64"`).
 *
 * Exported for testability — agents and scripts can call this directly to
 * verify the platform mapping without running the full resolver.
 *
 * @throws {Error} with the exact `process.platform` and `process.arch` values
 *   when the combination is unsupported.
 */
export function platformKey(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  const archMap = PLATFORM_MAP[platform];
  if (!archMap) {
    throw new Error(
      `Unsupported platform: ${platform} (arch: ${arch}). ` +
        `Supported platforms: ${Object.keys(PLATFORM_MAP).join(", ")}`,
    );
  }
  const key = archMap[arch];
  if (!key) {
    throw new Error(
      `Unsupported architecture: ${arch} on platform ${platform}. ` +
        `Supported architectures for ${platform}: ${Object.keys(archMap).join(", ")}`,
    );
  }
  return key;
}

/**
 * Locate the `aft` binary by checking (in order):
 * 1. npm platform package via `require.resolve(@aft/<platform>/bin/aft)`
 * 2. PATH lookup via `which aft` (or `where aft` on Windows)
 * 3. ~/.cargo/bin/aft (Rust cargo install location)
 *
 * Returns the absolute path to the first binary found.
 * Throws a descriptive error with install instructions and which sources
 * were attempted if none found.
 */
export function findBinary(): string {
  const attempted: string[] = [];
  const ext = process.platform === "win32" ? ".exe" : "";

  // 1. Check npm platform package
  try {
    const key = platformKey();
    const packageBin = `@aft/${key}/bin/aft${ext}`;
    // require.resolve finds the file relative to node_modules
    const resolved = require.resolve(packageBin);
    if (existsSync(resolved)) return resolved;
    attempted.push(`npm package @aft/${key}: resolved to ${resolved} but file does not exist`);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    // Only add to attempted if we got past platformKey (i.e. it's a resolution failure, not unsupported platform)
    if (!msg.includes("Unsupported")) {
      attempted.push(`npm package: ${msg}`);
    }
  }

  // 2. Check PATH
  try {
    const whichCmd = process.platform === "win32" ? "where aft" : "which aft";
    const result = execSync(whichCmd, {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    }).trim();
    if (result) return result;
  } catch {
    attempted.push("PATH: `aft` not found");
  }

  // 3. Check ~/.cargo/bin/aft
  const cargoPath = join(homedir(), ".cargo", "bin", `aft${ext}`);
  if (existsSync(cargoPath)) return cargoPath;
  attempted.push(`cargo: ${cargoPath} does not exist`);

  throw new Error(
    [
      "Could not find the `aft` binary.",
      "",
      "Attempted sources:",
      ...attempted.map((s) => `  - ${s}`),
      "",
      "Install it using one of these methods:",
      "  npm install @aft/core        # installs platform-specific binary via npm",
      "  cargo install aft             # from crates.io",
      "  cargo build --release         # from source (binary at target/release/aft)",
      "",
      "Or add the aft directory to your PATH.",
    ].join("\n"),
  );
}
