/**
 * Renderer coverage for aft_delete + aft_move.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { renderFsCall, renderFsResult } from "../tools/fs.js";
import { makeContext, makeResult, mockTheme, renderToString } from "./render-test-helpers.js";

describe("fs renderers", () => {
  test("renderFsCall shows delete and move paths", () => {
    const del = renderToString(
      renderFsCall(
        "aft_delete",
        { filePath: "src/a.ts" },
        mockTheme,
        makeContext({ filePath: "src/a.ts" }),
      ),
    );
    const move = renderToString(
      renderFsCall(
        "aft_move",
        { filePath: "src/a.ts", destination: "src/b.ts" },
        mockTheme,
        makeContext({ filePath: "src/a.ts", destination: "src/b.ts" }),
      ),
    );

    expect(del).toContain("delete");
    expect(del).toContain("src/a.ts");
    expect(move).toContain("move");
    expect(move).toContain("src/b.ts");
  });

  test("renderFsResult shows delete and move success summaries", () => {
    const del = renderToString(
      renderFsResult(
        "aft_delete",
        { filePath: "src/a.ts" },
        makeResult("Deleted src/a.ts"),
        mockTheme,
        makeContext({ filePath: "src/a.ts" }),
      ),
    );
    const move = renderToString(
      renderFsResult(
        "aft_move",
        { filePath: "src/a.ts", destination: "src/b.ts" },
        makeResult("Moved src/a.ts → src/b.ts"),
        mockTheme,
        makeContext({ filePath: "src/a.ts", destination: "src/b.ts" }),
      ),
    );

    expect(del).toContain("deleted");
    expect(move).toContain("moved");
    expect(move).toContain("src/b.ts");
  });

  test("renderFsResult handles error and missing payloads", () => {
    const error = renderToString(
      renderFsResult(
        "aft_delete",
        { filePath: "src/a.ts" },
        makeResult("permission denied"),
        mockTheme,
        makeContext({ filePath: "src/a.ts" }, { isError: true }),
      ),
    );
    const empty = renderToString(
      renderFsResult(
        "aft_move",
        { filePath: "src/a.ts", destination: "src/b.ts" },
        makeResult(""),
        mockTheme,
        makeContext({ filePath: "src/a.ts", destination: "src/b.ts" }),
      ),
    );

    expect(error).toContain("permission denied");
    expect(empty).toContain("src/b.ts");
  });
});
