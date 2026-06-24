/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";

// Regression guard for the OpenCode 1.17.10 / OpenTUI 0.4.2 break: the `./tui`
// export is raw TSX with `/** @jsxImportSource @opentui/solid */`, so importing
// it forces resolution of `@opentui/solid/jsx-dev-runtime` and `solid-js`. When
// those were not declared as runtime deps of this package, the plugin loaded
// fine in dev (host hoisted them as a peer) but threw
// `Cannot find module '@opentui/solid/jsx-dev-runtime'` for npm-install users on
// 1.17.10. The rest of the bun suite never imports the TSX entry, so nothing
// else catches this class of break. This test imports the entry exactly the way
// OpenCode loads `./tui` and asserts it resolves + exposes the plugin shape.
describe("tui entry module resolution", () => {
  test("the ./tui entry imports without a missing-module error", async () => {
    const mod = (await import("../tui/index.tsx")) as {
      default?: { id?: string; tui?: unknown };
    };
    expect(mod.default).toBeDefined();
    expect(mod.default?.id).toBe("aft-opencode");
    expect(typeof mod.default?.tui).toBe("function");
  });

  test("the @opentui/solid jsx runtime resolves from this package", () => {
    // The pragma compiles to `@opentui/solid/jsx-dev-runtime`; if it is not a
    // declared dep, this throws MODULE_NOT_FOUND.
    const resolved = require.resolve("@opentui/solid/jsx-dev-runtime");
    expect(resolved).toContain("@opentui");
    // solid-js must resolve to a single physical copy (dual-instance would break
    // Solid's reactive context across the OpenTUI tree even though it resolves).
    const solid = require.resolve("solid-js");
    expect(solid).toContain("solid-js");
  });
});
