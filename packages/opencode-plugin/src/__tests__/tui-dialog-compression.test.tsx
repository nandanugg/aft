/// <reference path="../bun-test.d.ts" />

import { describe, expect, mock, test } from "bun:test";
import type { StatusCompression } from "../shared/status";

mock.module("@opentui/solid/jsx-dev-runtime", () => ({
  Fragment: (props: { children?: unknown }) => props.children,
  jsxDEV: () => null,
}));
mock.module("@opentui/solid/jsx-runtime", () => ({
  Fragment: (props: { children?: unknown }) => props.children,
  jsx: () => null,
  jsxs: () => null,
}));
mock.module("solid-js", () => ({
  createMemo: (fn: () => unknown) => fn,
  createSignal: (initial: unknown) => [() => initial, () => undefined],
  onCleanup: () => undefined,
}));

const { formatCompressionDialogRows } = await import("../tui/index.tsx");

const compression = (overrides: Partial<StatusCompression> = {}): StatusCompression => ({
  project: {
    events: 1234,
    original_tokens: 567000,
    compressed_tokens: 234000,
    savings_tokens: 333000,
  },
  session: { events: 12, original_tokens: 5600, compressed_tokens: 2300, savings_tokens: 3300 },
  ...overrides,
});

describe("status dialog compression rows", () => {
  test("dialog_renders_compression_when_project_events_present", () => {
    const rows = formatCompressionDialogRows(compression());

    expect(rows.join("\n")).toContain("Session: 12 events");
    expect(rows.join("\n")).toContain("Project: 1.2k events");
  });

  test("dialog_hides_compression_when_undefined", () => {
    expect(formatCompressionDialogRows(undefined)).toEqual([]);
  });

  test("dialog_hides_compression_when_zero_events", () => {
    expect(
      formatCompressionDialogRows(
        compression({
          project: { events: 0, original_tokens: 0, compressed_tokens: 0, savings_tokens: 0 },
        }),
      ),
    ).toEqual([]);
  });

  test("dialog_formats_savings_percent_when_original_nonzero", () => {
    const rows = formatCompressionDialogRows(compression());

    expect(rows[0]).toContain("5.6k → 2.3k tokens (3.3k saved, 59%)");
  });

  test("dialog_omits_percent_when_original_zero", () => {
    const rows = formatCompressionDialogRows(
      compression({
        session: { events: 1, original_tokens: 0, compressed_tokens: 0, savings_tokens: 0 },
      }),
    );

    expect(rows[0]).toBe("Session: 1 events · 0 → 0 tokens (0 saved)");
  });
});
