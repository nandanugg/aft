// Pure helpers for the bash-output hint nudges appended to bash tool results.
//
// Shared across harnesses (OpenCode applies it in `tool.execute.after`; Pi
// applies it inside its hoisted bash tool). Returns the new output string (or
// the original when no hint should fire). The appended "[Hint] ..." line is
// agent-visible and persists in the tool result.
//
// Note: the grep/rg code-search redirect lives in the Rust bash rewriter
// (`bash_rewrite::footer::add_grep_footer`), which owns the rewrite and reads
// `aft_search_registered` from config. It is not duplicated here.

const CONFLICT_HINT =
  "\n\n[Hint] Use aft_conflicts to see all conflict regions across files in a single call.";

/**
 * Append the `aft_conflicts` hint when the output indicates a real git merge
 * or rebase produced conflicts.
 *
 * Gated on BOTH:
 *  - the "Automatic merge failed; fix conflicts" marker, AND
 *  - a git-conflict signal (`CONFLICT (...)` line or `error: could not apply`)
 *
 * Both conditions are required because `aft_conflicts` calls `git ls-files -u`,
 * which fails with "not a git repository" outside a git working tree. The
 * marker string can legitimately appear in docs, READMEs, test fixtures, and
 * grep output, so we cannot key off it alone — a false-positive hint sends
 * agents into a confusing error.
 */
export function maybeAppendConflictsHint(output: string): string {
  if (!output.includes("Automatic merge failed; fix conflicts")) return output;
  // git merge prints "CONFLICT (content|file|...): ..." per file.
  // git rebase / git am print "error: could not apply <sha>" per failed pick.
  if (!/^CONFLICT \(|^error: could not apply /m.test(output)) return output;
  return output + CONFLICT_HINT;
}
