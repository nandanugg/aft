/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { getActiveLogger, log, setActiveLogger } from "../active-logger.js";
import type { Logger } from "../logger.js";

describe("active logger", () => {
  test("stores logger on Symbol.for global slot", () => {
    const logger: Logger = {
      log: () => undefined,
      warn: () => undefined,
      error: () => undefined,
    };

    setActiveLogger(logger);

    expect(getActiveLogger()).toBe(logger);
    expect((globalThis as Record<symbol, unknown>)[Symbol.for("aft-bridge-active-logger")]).toBe(
      logger,
    );
  });

  test("logger exceptions are caught and do not escape", () => {
    const logger: Logger = {
      log: () => {
        throw new Error("logger exploded");
      },
      warn: () => undefined,
      error: () => undefined,
    };
    setActiveLogger(logger);

    expect(() => log("still safe")).not.toThrow();
  });
});
