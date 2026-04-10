import { execSync } from "node:child_process";

export function isOpenCodeInstalled(): boolean {
  try {
    execSync("opencode --version", { stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}

export function getOpenCodeVersion(): string | null {
  try {
    return execSync("opencode --version", { stdio: "pipe" }).toString().trim();
  } catch {
    return null;
  }
}

export function isGhInstalled(): boolean {
  try {
    execSync("gh --version", { stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}
