/**
 * Renderer coverage for hoisted write/edit call + result summaries.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { renderMutationCall, renderMutationResult } from "../tools/hoisted.js";
import { makeContext, makeResult, mockTheme, renderToString } from "./render-test-helpers.js";

describe("hoisted renderers", () => {
  test("renderMutationCall shows the tool name and path", () => {
    const out = renderToString(
      renderMutationCall(
        "edit",
        "src/batch.ts",
        mockTheme,
        makeContext({ filePath: "src/batch.ts" }),
      ),
    );

    expect(out).toContain("edit");
    expect(out).toContain("src/batch.ts");
  });

  test("renderMutationResult keeps batch edit counts when only summary counts are available", () => {
    const out = renderToString(
      renderMutationResult(
        makeResult("Edited (+4/-4, 2 edits).", {
          additions: 4,
          deletions: 4,
          editsApplied: 2,
          truncated: true,
        }),
        mockTheme,
        makeContext({ filePath: "src/batch.ts" }),
      ),
    );

    expect(out).toContain("+4/-4, 2 edits");
    expect(out).toContain("diff truncated");
  });
});
