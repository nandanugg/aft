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
  test("redirects to the grep tool when aft_search is unavailable (default)", () => {
    const output = "rg -n 'pattern' src/\nsrc/foo.ts:42:match\n";
    const result = maybeAppendGrepHint(output);
    expect(result).toContain("[Hint] DO NOT search code by running grep/rg in bash");
    expect(result).toContain("Use the `grep` tool");
    expect(result).not.toContain("aft_search");
  });

  test("redirects to aft_search when it is available (no grep-tool mention)", () => {
    const output = "grep -rn 'pattern' src/\nsrc/foo.ts:42:match\n";
    const result = maybeAppendGrepHint(output, undefined, true);
    expect(result).toContain("[Hint] DO NOT search code by running grep/rg in bash");
    expect(result).toContain("Use `aft_search`");
    expect(result).not.toContain("grep` tool");
  });

  test("both branches steer to one parallel wave", () => {
    const output = "grep -rn 'x' src/\n";
    expect(maybeAppendGrepHint(output)).toContain("one parallel wave");
    expect(maybeAppendGrepHint(output, undefined, true)).toContain("one parallel wave");
  });

  test("does NOT fire when grep token appears only on a later line", () => {
    const output = "running tests...\n  grep is used inside\n";
    expect(maybeAppendGrepHint(output)).toBe(output);
    expect(maybeAppendGrepHint(output, undefined, true)).toBe(output);
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
      "[Hint] DO NOT search code by running grep/rg in bash",
    );
    expect(maybeAppendGrepHint(output, "rg -n 'pattern' src/", true)).toContain("Use `aft_search`");
  });

  test("does NOT fire for piped/filtering grep (anchored at command start)", () => {
    // `ps aux | grep foo` is process filtering, not code search. The hint must
    // not nag here — the regex anchors at the start, so a non-leading grep/rg
    // is ignored.
    const output = "  501  12345  node\n";
    expect(maybeAppendGrepHint(output, "ps aux | grep foo")).toBe(output);
    expect(maybeAppendGrepHint(output, "git log | rg bar", true)).toBe(output);
  });

  test("DOES fire for a leading grep/rg with surrounding whitespace", () => {
    const output = "src/foo.ts:1:x\n";
    expect(maybeAppendGrepHint(output, "  grep -rn foo src/")).toContain("[Hint] DO NOT");
  });

  test("does NOT fire when the command is not grep/rg even if output mentions grep", () => {
    const output = "grep appears in this output line\n";
    expect(maybeAppendGrepHint(output, "cat notes.txt")).toBe(output);
  });

  test("empty command does not fire", () => {
    expect(maybeAppendGrepHint("some output", "")).toBe("some output");
  });
});
