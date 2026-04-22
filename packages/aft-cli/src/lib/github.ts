import { execSync, spawnSync } from "node:child_process";

export function isGhInstalled(): boolean {
  try {
    execSync("gh --version", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

export function openBrowser(url: string): void {
  const commands =
    process.platform === "darwin"
      ? ["open", [url]]
      : process.platform === "win32"
        ? ["cmd", ["/c", "start", "", url]]
        : ["xdg-open", [url]];

  try {
    const [cmd, args] = commands as [string, string[]];
    spawnSync(cmd, args, { stdio: "ignore" });
  } catch {
    // no-op — caller can fall back to printing the URL
  }
}

/**
 * Create a GitHub issue via `gh issue create`. Returns the issue URL on
 * success or null on failure.
 */
export function createGitHubIssue(
  repo: string,
  title: string,
  body: string,
): { url: string | null; stderr?: string } {
  if (!isGhInstalled()) {
    return { url: null, stderr: "gh CLI not installed" };
  }
  try {
    const result = execSync(
      `gh issue create --repo ${repo} --title ${JSON.stringify(title)} --body-file -`,
      {
        input: body,
        encoding: "utf-8",
        stdio: ["pipe", "pipe", "pipe"],
      },
    );
    const url = result.trim().split(/\r?\n/).pop();
    return { url: url || null };
  } catch (error) {
    return { url: null, stderr: error instanceof Error ? error.message : String(error) };
  }
}
