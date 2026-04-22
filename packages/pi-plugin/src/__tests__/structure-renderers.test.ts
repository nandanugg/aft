/**
 * Renderer coverage for aft_transform.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { renderTransformCall, renderTransformResult } from "../tools/structure.js";
import { makeContext, makeResult, mockTheme, renderToString } from "./render-test-helpers.js";

describe("transform renderer", () => {
  test("renderTransformCall shows op and target", () => {
    const output = renderToString(
      renderTransformCall(
        { op: "add_member", filePath: "src/a.ts", container: "Service" },
        mockTheme,
        makeContext({ op: "add_member", filePath: "src/a.ts", container: "Service" }),
      ),
    );
    expect(output).toContain("transform");
    expect(output).toContain("add_member");
    expect(output).toContain("Service");
  });

  test("renderTransformResult shows structured summary", () => {
    const output = renderToString(
      renderTransformResult(
        makeResult("", { file: "src/a.ts", scope: "Service" }),
        { op: "add_member", filePath: "src/a.ts", container: "Service" },
        mockTheme,
        makeContext({ op: "add_member", filePath: "src/a.ts", container: "Service" }),
      ),
    );
    expect(output).toContain("transformed add_member");
    expect(output).toContain("target Service");
  });

  test("renderTransformResult handles dry-run and error paths", () => {
    const dryRun = renderToString(
      renderTransformResult(
        makeResult("", { dry_run: true, diff: "--- a/src/a.ts\n+++ b/src/a.ts" }),
        { op: "add_member", filePath: "src/a.ts", container: "Service", dryRun: true },
        mockTheme,
        makeContext({ op: "add_member", filePath: "src/a.ts", container: "Service", dryRun: true }),
      ),
    );
    const error = renderToString(
      renderTransformResult(
        makeResult("parse failed"),
        { op: "add_member", filePath: "src/a.ts", container: "Service" },
        mockTheme,
        makeContext(
          { op: "add_member", filePath: "src/a.ts", container: "Service" },
          { isError: true },
        ),
      ),
    );

    expect(dryRun).toContain("[dry run]");
    expect(error).toContain("parse failed");
  });
});
