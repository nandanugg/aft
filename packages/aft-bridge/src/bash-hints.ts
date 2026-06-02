// Pure helpers for the bash-output hint nudges appended to bash tool results.
//
// Shared across harnesses (OpenCode applies them in `tool.execute.after`; Pi
// applies them inside its hoisted bash tool, where the command is known). Each
// function returns the new output string (or the original when no hint should
// fire) so callers compose them without managing state. The appended
// "[Hint] ..." lines are agent-visible and persist in the tool result.

const CONFLICT_HINT =
  "\n\n[Hint] Use aft_conflicts to see all conflict regions across files in a single call.";
const GREP_HINT = "\n\n[Hint] Use the grep tool instead of bash for faster indexed search.";

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

/**
 * Append the grep-tool hint when the bash command was a grep/rg invocation.
 *
 * When `command` is provided (Pi knows the exact command), it is matched
 * directly. Otherwise (OpenCode's `tool.execute.after`, where only the output
 * is available) the FIRST LINE of output is examined — on most shells in
 * foreground mode this is the echoed command line. The slice is capped at 300
 * chars so a single huge first line doesn't slow this hook.
 */
export function maybeAppendGrepHint(output: string, command?: string): string {
  const probe = command !== undefined ? command : (output.slice(0, 300).split("\n")[0] ?? "");
  if (!/\b(rg|grep)\s/.test(probe)) return output;
  return output + GREP_HINT;
}
