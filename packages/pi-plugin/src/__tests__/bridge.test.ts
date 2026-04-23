/**
 * Bridge-level tests for Pi.
 *
 * Mirrors packages/opencode-plugin/src/__tests__/bridge.test.ts. Both plugins
 * share the same bridge design (per-op timeout, SIGKILL recovery, etc) so we
 * keep coverage mirrored to catch regressions in either package.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { BinaryBridge } from "../bridge.js";

const PROJECT_CWD = resolve(import.meta.dir, "../../../..");

describe("Pi BinaryBridge", () => {
  let bridge: BinaryBridge | null = null;

  afterEach(async () => {
    if (bridge) {
      await bridge.shutdown();
      bridge = null;
    }
  });

  test("per-request timeoutMs override rejects before bridge-wide default", async () => {
    // Fake binary: reads stdin and sleeps forever without responding. We want
    // to prove the per-request override (50ms) fires instead of the bridge
    // default (5000ms). If the override isn't honored, the bridge-wide timer
    // triggers and the test would take 5+ seconds to reject.
    const fakeBin = join(tmpdir(), `aft-pi-fake-slow-${Date.now()}.sh`);
    await writeFile(fakeBin, ["#!/bin/sh", "sleep 30", ""].join("\n"), { mode: 0o755 });

    try {
      bridge = new BinaryBridge(fakeBin, PROJECT_CWD, {
        timeoutMs: 5_000, // bridge-wide default
        maxRestarts: 0,
      });

      const start = Date.now();
      // Use "version" to skip the auto-configure path (configure/version are
      // the two commands that bypass it). Pass a tight 50ms override — should
      // reject in ~50ms, not 5000ms. If the override weren't honored, the
      // bridge-wide 5s timer would trigger instead and `elapsed` would be ~5s.
      const err = await bridge.send("version", {}, { timeoutMs: 50 }).catch((e) => e);
      const elapsed = Date.now() - start;

      expect(err).toBeInstanceOf(Error);
      expect((err as Error).message).toContain("timed out after 50ms");
      // Allow generous slack so CI flakes don't fail this — but must be well
      // under the 5s bridge default to prove the override took effect.
      expect(elapsed).toBeLessThan(2_000);
    } finally {
      await rm(fakeBin).catch(() => {});
    }
  });
});
