import { afterEach, describe, expect, mock, test } from "bun:test";
import { platformKey } from "../resolver.js";

const downloaderModulePath = new URL("../downloader.js", import.meta.url).pathname;
let importNonce = 0;

function freshResolverImport() {
  return import(`../resolver.ts?resolver-error=${importNonce++}`);
}

function mockNoBinaryEnvironment(downloadedBinary: string | null = null) {
  const resolveMock = mock(() => {
    throw new Error("package missing");
  });
  const execMock = mock(() => {
    throw new Error("binary missing");
  });

  mock.module(downloaderModulePath, () => ({
    getCachedBinaryPath: () => null,
    ensureBinary: async () => downloadedBinary,
  }));
  mock.module("node:fs", () => ({
    chmodSync: () => undefined,
    copyFileSync: () => undefined,
    existsSync: () => false,
    mkdirSync: () => undefined,
  }));
  mock.module("node:child_process", () => ({
    execSync: execMock,
    spawnSync: mock(() => ({ stdout: "", stderr: "", status: 1 })),
  }));
  mock.module("node:module", () => ({
    createRequire: () => ({ resolve: resolveMock }),
  }));
  mock.module("node:os", () => ({ homedir: () => "/tmp/aft-home" }));

  return { resolveMock, execMock };
}

afterEach(() => {
  mock.restore();
});

describe("resolver error paths", () => {
  test("prefers a repo-local checkout binary before cache and PATH fallbacks", async () => {
    const resolveMock = mock(() => "/repo/packages/opencode-plugin/package.json");
    const execMock = mock(() => {
      throw new Error("PATH lookup should not run");
    });

    mock.module(downloaderModulePath, () => ({
      getCachedBinaryPath: () => "/cache/aft",
      ensureBinary: async () => null,
    }));
    mock.module("node:fs", () => ({
      chmodSync: () => undefined,
      copyFileSync: () => undefined,
      existsSync: (path: string) => path === "/repo/target/release/aft",
      mkdirSync: () => undefined,
    }));
    mock.module("node:child_process", () => ({
      execSync: execMock,
      spawnSync: mock(() => ({ stdout: "", stderr: "", status: 1 })),
    }));
    mock.module("node:module", () => ({
      createRequire: () => ({ resolve: resolveMock }),
    }));
    mock.module("node:os", () => ({ homedir: () => "/tmp/aft-home" }));

    const { findBinarySync, findLocalCheckoutBinarySync } = await freshResolverImport();

    expect(findLocalCheckoutBinarySync()).toBe("/repo/target/release/aft");
    expect(findBinarySync()).toBe("/repo/target/release/aft");
    expect(resolveMock).toHaveBeenCalled();
    expect(execMock).not.toHaveBeenCalled();
  });

  test("includes supported platforms when a platform key is missing", () => {
    expect(() => platformKey("plan9", "x64")).toThrow(
      "Unsupported platform: plan9 (arch: x64). Supported platforms: darwin, linux, win32",
    );
  });

  test("returns null when npm package resolution fails and no fallback binary exists", async () => {
    const { resolveMock, execMock } = mockNoBinaryEnvironment();
    const { findBinarySync } = await freshResolverImport();

    expect(findBinarySync()).toBeNull();
    expect(resolveMock).toHaveBeenCalledTimes(2);
    expect(execMock).toHaveBeenCalledTimes(1);
  });

  test("throws detailed installation instructions when no binary exists anywhere", async () => {
    const logCalls: string[] = [];
    const loggerPath = new URL("../logger.js", import.meta.url).pathname;
    mock.module(loggerPath, () => ({
      log: (msg: string) => logCalls.push(msg),
      warn: (msg: string) => logCalls.push(msg),
      error: (msg: string) => logCalls.push(msg),
    }));
    mockNoBinaryEnvironment(null);
    const { findBinary } = await freshResolverImport();
    const promise = findBinary();

    await expect(promise).rejects.toThrow("Could not find the `aft` binary.");
    await expect(promise).rejects.toThrow("Attempted sources:");
    await expect(promise).rejects.toThrow("Auto-download from GitHub releases (failed)");
    await expect(promise).rejects.toThrow("npm install @cortexkit/aft-opencode");

    expect(logCalls).toContain("Binary not found locally, attempting auto-download...");
  });
});
