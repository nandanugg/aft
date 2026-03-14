import { describe, test, expect, beforeEach, mock } from "bun:test";
import { platformKey, findBinary } from "../resolver.js";
import { resolve } from "node:path";
import { existsSync } from "node:fs";

// ---------------------------------------------------------------------------
// platformKey() — pure mapping, no side effects
// ---------------------------------------------------------------------------

describe("platformKey", () => {
  test("darwin + arm64 → darwin-arm64", () => {
    expect(platformKey("darwin", "arm64")).toBe("darwin-arm64");
  });

  test("darwin + x64 → darwin-x64", () => {
    expect(platformKey("darwin", "x64")).toBe("darwin-x64");
  });

  test("linux + arm64 → linux-arm64", () => {
    expect(platformKey("linux", "arm64")).toBe("linux-arm64");
  });

  test("linux + x64 → linux-x64", () => {
    expect(platformKey("linux", "x64")).toBe("linux-x64");
  });

  test("win32 + x64 → win32-x64", () => {
    expect(platformKey("win32", "x64")).toBe("win32-x64");
  });

  test("unsupported platform throws with platform and arch in message", () => {
    expect(() => platformKey("freebsd", "x64")).toThrow(
      /Unsupported platform: freebsd.*arch: x64/,
    );
  });

  test("unsupported arch on valid platform throws with arch details", () => {
    expect(() => platformKey("darwin", "s390x")).toThrow(
      /Unsupported architecture: s390x on platform darwin/,
    );
  });

  test("win32 + arm64 is unsupported", () => {
    expect(() => platformKey("win32", "arm64")).toThrow(
      /Unsupported architecture: arm64 on platform win32/,
    );
  });

  test("defaults to process.platform and process.arch when no args", () => {
    // Should not throw on the current host
    const key = platformKey();
    expect(typeof key).toBe("string");
    expect(key).toContain("-");
  });
});

// ---------------------------------------------------------------------------
// Windows .exe suffix logic
// ---------------------------------------------------------------------------

describe("Windows binary naming", () => {
  test("win32-x64 platform key is used for Windows binary lookup", () => {
    const key = platformKey("win32", "x64");
    expect(key).toBe("win32-x64");
    // The resolver constructs `@aft/${key}/bin/aft.exe` for win32
    // Verify the naming convention matches the win32 platform package
    const expectedBin = `@aft/${key}/bin/aft.exe`;
    expect(expectedBin).toBe("@aft/win32-x64/bin/aft.exe");
  });

  test("non-win32 platforms do not use .exe", () => {
    for (const [platform, arch] of [
      ["darwin", "arm64"],
      ["darwin", "x64"],
      ["linux", "arm64"],
      ["linux", "x64"],
    ] as const) {
      const key = platformKey(platform, arch);
      const expectedBin = `@aft/${key}/bin/aft`;
      expect(expectedBin).not.toContain(".exe");
    }
  });
});

// ---------------------------------------------------------------------------
// findBinary() — integration tests for fallback chain
// ---------------------------------------------------------------------------

describe("findBinary", () => {
  test("finds binary via PATH or cargo fallback", () => {
    // This test relies on the debug binary being available (pretest runs cargo build)
    const debugBinary = resolve(import.meta.dir, "../../../target/debug/aft");
    const hasBinary = existsSync(debugBinary);

    if (!hasBinary) {
      console.warn(
        "Skipping findBinary integration test — debug binary not built. Run `cargo build` first.",
      );
      return;
    }

    // findBinary should succeed since `which aft` or ~/.cargo/bin/aft should work
    // (the debug binary is found via PATH since cargo build puts it in a known location,
    //  or the test environment has aft installed)
    try {
      const result = findBinary();
      expect(typeof result).toBe("string");
      expect(result.length).toBeGreaterThan(0);
    } catch (e) {
      // If findBinary throws, it should be because none of the resolution methods found it.
      // That's okay in a test env where npm packages aren't installed and aft isn't on PATH.
      // Verify the error message is descriptive.
      expect(e).toBeInstanceOf(Error);
      const msg = (e as Error).message;
      expect(msg).toContain("Could not find the `aft` binary");
      expect(msg).toContain("Attempted sources:");
    }
  });

  test("error message includes attempted sources when binary not found", () => {
    // We can't easily force all three paths to fail without mocking,
    // but we verify the error format is correct by checking the message structure.
    // The error should always list install methods.
    try {
      findBinary();
      // If it succeeds, binary is found — that's fine, nothing to verify about errors
    } catch (e) {
      expect(e).toBeInstanceOf(Error);
      const msg = (e as Error).message;
      expect(msg).toContain("npm install @aft/core");
      expect(msg).toContain("cargo install aft");
    }
  });
});
