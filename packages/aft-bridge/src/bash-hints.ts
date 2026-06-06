// Pure helpers for the bash-output hint nudges appended to bash tool results.
//
// Shared across harnesses (OpenCode applies them in `tool.execute.after`; Pi
// applies them inside its hoisted bash tool, where the command is known). Each
// function returns the new output string (or the original when no hint should
// fire) so callers compose them without managing state. The appended
// "[Hint] ..." lines are agent-visible and persist in the tool result.

const CONFLICT_HINT =
  "\n\n[Hint] Use aft_conflicts to see all conflict regions across files in a single call.";
// When aft_search is registered, push it alone — it auto-routes concepts,
// identifiers, regex, AND literals, so naming the grep tool here would only
// dilute the redirect. When aft_search is unavailable (no semantic / minimal
// surface), the grep tool is the indexed+ranked alternative to raw bash grep.
const GREP_HINT_SEARCH =
  "\n\n[Hint] DO NOT search code by running grep/rg in bash — it is unindexed, unranked, and serial. Use `aft_search` instead (it auto-routes concepts, identifiers, regex, and literals), and fire independent searches in one parallel wave.";
const GREP_HINT_GREP_TOOL =
  "\n\n[Hint] DO NOT search code by running grep/rg in bash. Use the `grep` tool instead — indexed and ranked — and fire independent searches in one parallel wave.";

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
 * Append the code-search redirect hint when the bash command was a grep/rg
 * invocation. The message steers to `aft_search` when it's available (it covers
 * literals too), otherwise to the `grep` tool — never back to bash grep.
 *
 * `aftSearchAvailable` is decided by each plugin from the registered tool
 * surface (semantic_search on + not minimal + aft_search not disabled).
 *
 * When `command` is provided (Pi knows the exact command), it is matched
 * directly. Otherwise (OpenCode's `tool.execute.after`, where only the output
 * is available) the FIRST LINE of output is examined — on most shells in
 * foreground mode this is the echoed command line. The slice is capped at 300
 * chars so a single huge first line doesn't slow this hook. Piped grep
 * (`ps aux | grep foo`) starts with the upstream command, so the first token
 * isn't grep/rg and no hint fires — only a leading grep/rg matches.
 */
export function maybeAppendGrepHint(
  output: string,
  command?: string,
  aftSearchAvailable = false,
): string {
  const probe = command !== undefined ? command : (output.slice(0, 300).split("\n")[0] ?? "");
  // Anchor at the start of the command so piped/filtering grep (`ps aux | grep
  // foo`, `git log | rg bar`) does NOT trigger the code-search redirect — only a
  // leading grep/rg, which is an actual file-search invocation. Mirrors the
  // first-token check the Rust bash rewriter uses for grep_request.
  if (!/^\s*(rg|grep)\b/.test(probe)) return output;
  return output + (aftSearchAvailable ? GREP_HINT_SEARCH : GREP_HINT_GREP_TOOL);
}
