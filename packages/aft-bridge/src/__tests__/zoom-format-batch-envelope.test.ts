import { describe, expect, test } from "bun:test";
import {
  formatZoomText,
  isRustZoomBatchEnvelope,
  unwrapRustZoomBatchEnvelope,
} from "../zoom-format.js";

describe("Rust zoom batch envelope", () => {
  test("isRustZoomBatchEnvelope accepts valid batch shape", () => {
    const response = {
      success: true,
      complete: true,
      symbols: [
        { name: "a", response: { success: true, name: "a", content: "x" } },
        { name: "b", response: { success: false, message: "nope" } },
      ],
    };
    expect(isRustZoomBatchEnvelope(response)).toBe(true);
    expect(unwrapRustZoomBatchEnvelope(response)).toEqual({
      names: ["a", "b"],
      responses: [
        { success: true, name: "a", content: "x" },
        { success: false, message: "nope" },
      ],
    });
  });

  test("isRustZoomBatchEnvelope rejects single-symbol zoom shape", () => {
    expect(
      isRustZoomBatchEnvelope({
        success: true,
        name: "foo",
        content: "body",
      }),
    ).toBe(false);
  });
});

describe("formatZoomText call annotations", () => {
  test("renders folded call-site counts compactly", () => {
    const text = formatZoomText("src/calls.ts", {
      name: "caller",
      kind: "function",
      range: { start_line: 10, end_line: 12 },
      content: `function caller() {
  helper();
}`,
      annotations: {
        calls_out: [{ name: "helper", line: 11, extra_count: 1 }],
        called_by: [{ name: "orchestrate", line: 20 }],
      },
    });

    expect(text).toContain("helper (line 11) +1");
    expect(text).toContain("orchestrate (line 20)");
  });
});
