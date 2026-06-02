import { describe, expect, test } from "bun:test";
import { maybeAppendConflictsHint, maybeAppendGrepHint } from "../bash-hints.js";

describe("maybeAppendConflictsHint", () => {
  test("appends hint on real git-merge conflict output", () => {
    const output = [
      "Auto-merging packages/opencode-plugin/src/index.ts",
      "CONFLICT (content): Merge conflict in packages/opencode-plugin/src/index.ts",
      "Automatic merge failed; fix conflicts and then commit the result.",
    ].join("\n");
    expect(maybeAppendConflictsHint(output)).toContain("[Hint] Use aft_conflicts");
  });

  test("appends hint on rebase conflict output", () => {
    const output = [
      "error: could not apply 0e3f4a2... feat: add foo",
      "Automatic merge failed; fix conflicts and then commit the result.",
    ].join("\n");
    expect(maybeAppendConflictsHint(output)).toContain("[Hint] Use aft_conflicts");
  });

  // The trigger string appears verbatim in many docs/READMEs. The hint must NOT
  // fire just because someone cat'd a README in a non-git directory.
  test("does NOT append hint when marker appears alone (e.g. README excerpt)", () => {
    const output =
      "When git can't merge automatically, you'll see:\n\n" +
      "  Automatic merge failed; fix conflicts and then commit the result.\n\n" +
      "This means you need to resolve the conflict manually.";
    expect(maybeAppendConflictsHint(output)).toBe(output);
  });

  test("does NOT fire on mid-line CONFLICT substring", () => {
    const output = "we documented: Automatic merge failed; fix conflicts. (see CONFLICT (3) below)";
    expect(maybeAppendConflictsHint(output)).toBe(output);
  });

  test("does NOT append hint when output is unrelated to git", () => {
    const output = "hello world\n+0/-0\nlinting passed.";
    expect(maybeAppendConflictsHint(output)).toBe(output);
  });

  test("does NOT append hint when output is empty", () => {
    expect(maybeAppendConflictsHint("")).toBe("");
  });
});

describe("maybeAppendGrepHint (first-line fallback — OpenCode)", () => {
  test("appends hint when first line is a rg invocation", () => {
    const output = "rg -n 'pattern' src/\nsrc/foo.ts:42:match\n";
    expect(maybeAppendGrepHint(output)).toContain("[Hint] Use the grep tool");
  });

  test("appends hint when first line is a grep invocation", () => {
    const output = "grep -rn 'pattern' src/\nsrc/foo.ts:42:match\n";
    expect(maybeAppendGrepHint(output)).toContain("[Hint] Use the grep tool");
  });

  test("does NOT fire when grep token appears only on a later line", () => {
    const output = "running tests...\n  grep is used inside\n";
    expect(maybeAppendGrepHint(output)).toBe(output);
  });

  test("does NOT fire on unrelated output", () => {
    const output = "ls -la\ntotal 12\n-rw-r--r-- file.txt\n";
    expect(maybeAppendGrepHint(output)).toBe(output);
  });
});

describe("maybeAppendGrepHint (explicit command — Pi)", () => {
  test("matches the command directly, not the output's first line", () => {
    // Output's first line is NOT a grep command (no echo), but the command is.
    const output = "src/foo.ts:42:match\nsrc/bar.ts:7:match\n";
    expect(maybeAppendGrepHint(output, "rg -n 'pattern' src/")).toContain(
      "[Hint] Use the grep tool",
    );
  });

  test("does NOT fire when the command is not grep/rg even if output mentions grep", () => {
    const output = "grep appears in this output line\n";
    expect(maybeAppendGrepHint(output, "cat notes.txt")).toBe(output);
  });

  test("empty command does not fire", () => {
    expect(maybeAppendGrepHint("some output", "")).toBe("some output");
  });
});
