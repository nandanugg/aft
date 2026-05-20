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
  createEffect: () => undefined,
  createMemo: (fn: () => unknown) => fn,
  createSignal: (initial: unknown) => [() => initial, () => undefined],
  on: (_source: unknown, fn: unknown) => fn,
  onCleanup: () => undefined,
}));

const { formatCompressionSidebarRows } = await import("../tui/sidebar.tsx");

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

describe("sidebar compression rows", () => {
  test("sidebar_renders_compression_when_project_events_present", () => {
    const rows = formatCompressionSidebarRows(compression());

    expect(rows.join("\n")).toContain("Session");
    expect(rows.join("\n")).toContain("Project");
    expect(rows.join("\n")).toContain("333k saved");
  });

  test("sidebar_hides_compression_when_undefined", () => {
    expect(formatCompressionSidebarRows(undefined)).toEqual([]);
  });

  test("sidebar_hides_compression_when_zero_events", () => {
    expect(
      formatCompressionSidebarRows(
        compression({
          project: { events: 0, original_tokens: 0, compressed_tokens: 0, savings_tokens: 0 },
        }),
      ),
    ).toEqual([]);
  });

  test("sidebar_hides_session_row_when_session_events_zero", () => {
    const rows = formatCompressionSidebarRows(
      compression({
        session: { events: 0, original_tokens: 0, compressed_tokens: 0, savings_tokens: 0 },
      }),
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]).toContain("Project");
    expect(rows.join("\n")).not.toContain("Session");
  });
});
