import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { HarnessAdapter } from "../adapters/types.js";
import { collectDiagnostics, renderDiagnosticsMarkdown, tailLogFile } from "../lib/diagnostics.js";
import { formatBytes } from "../lib/fs-util.js";
import { createGitHubIssue, isGhInstalled, openBrowser } from "../lib/github.js";
import { resolveAdaptersForCommand } from "../lib/harness-select.js";
import { intro, log, note, outro, text } from "../lib/prompts.js";
import { sanitizeContent } from "../lib/sanitize.js";

export interface DoctorOptions {
  force: boolean;
  issue: boolean;
  argv: string[];
}

export async function runDoctor(options: DoctorOptions): Promise<number> {
  if (options.issue) {
    return runIssueFlow(options.argv);
  }
  intro("AFT doctor");

  const adapters = await resolveAdaptersForCommand(options.argv, {
    allowMulti: true,
    verb: "diagnose",
  });

  const report = await collectDiagnostics(adapters);

  log.info(`CLI v${report.cliVersion}, binary ${report.binaryVersion ?? "unknown"}`);
  log.info(
    `Binary cache: ${report.binaryCache.versions.length} version(s), ${formatBytes(report.binaryCache.totalSize)} at ${report.binaryCache.path}`,
  );

  let hadProblems = false;
  for (const h of report.harnesses) {
    log.step(`${h.displayName}`);
    if (!h.hostInstalled) {
      log.warn(`  host not installed — install from: ${describeAdapterInstallHint(h.kind)}`);
      hadProblems = true;
      continue;
    }
    log.info(`  host: ${h.hostVersion ?? "unknown version"}`);
    log.info(`  plugin registered: ${h.pluginRegistered ? "yes" : "no"}`);
    if (!h.pluginRegistered) hadProblems = true;

    log.info(`  aft config: ${h.aftConfig.exists ? h.configPaths.aftConfig : "(not set)"}`);
    if (h.aftConfig.parseError) {
      log.error(`  aft config parse error: ${h.aftConfig.parseError}`);
      hadProblems = true;
    }

    log.info(
      `  storage: ${h.storageDir.exists ? h.storageDir.path : "(not created)"} (${formatStorageSizes(h.storageDir.sizesByKey)})`,
    );

    if (h.onnxRuntime.required) {
      const parts: string[] = [];
      parts.push(`required: yes (${h.onnxRuntime.platform})`);
      if (h.onnxRuntime.cachedPath) {
        parts.push(
          `cached: ${h.onnxRuntime.cachedVersion ?? "unknown"}${h.onnxRuntime.cachedCompatible === false ? " (incompatible)" : ""}`,
        );
      }
      if (h.onnxRuntime.systemPath) {
        parts.push(
          `system: ${h.onnxRuntime.systemVersion ?? "unknown"}${h.onnxRuntime.systemCompatible === false ? " (incompatible)" : ""}`,
        );
      }
      if (!h.onnxRuntime.cachedPath && !h.onnxRuntime.systemPath) {
        parts.push(`not installed — ${h.onnxRuntime.installHint}`);
        hadProblems = true;
      }
      log.info(`  onnx runtime: ${parts.join(" · ")}`);
    }

    log.info(
      `  log: ${h.logFile.exists ? `${h.logFile.path} (${h.logFile.sizeKb} KB)` : "(not written yet)"}`,
    );
  }

  // Apply automatic fixes where useful.
  for (const adapter of adapters) {
    await maybeFixPlugin(adapter, options.force);
  }

  if (hadProblems) {
    note(
      "Run `aft setup` to register AFT with any harness showing `plugin registered: no`.",
      "Tips",
    );
    outro("Done — some issues found.");
    return 1;
  }
  outro("Everything looks good.");
  return 0;
}

async function maybeFixPlugin(adapter: HarnessAdapter, force: boolean): Promise<void> {
  if (force) {
    const result = await adapter.clearPluginCache(true);
    if (result.action === "cleared") {
      log.success(`${adapter.displayName}: cleared plugin cache at ${result.path}`);
    } else if (result.action === "not_applicable") {
      log.info(`${adapter.displayName}: no user-managed plugin cache to clear`);
    } else if (result.action === "not_found") {
      log.info(`${adapter.displayName}: no plugin cache found at ${result.path}`);
    } else if (result.action === "error") {
      log.error(`${adapter.displayName}: cache clear failed: ${result.error ?? "unknown"}`);
    }
  }

  if (!adapter.hasPluginEntry() && adapter.isInstalled()) {
    log.info(`${adapter.displayName}: attempting to register plugin…`);
    const r = await adapter.ensurePluginEntry();
    if (r.ok) {
      log.success(`${adapter.displayName}: ${r.message}`);
    } else {
      log.error(`${adapter.displayName}: ${r.message}`);
    }
  }
}

function describeAdapterInstallHint(kind: string): string {
  if (kind === "opencode") return "https://opencode.ai/docs/install";
  if (kind === "pi") return "https://github.com/badlogic/pi-mono";
  return "(unknown harness)";
}

function formatStorageSizes(sizes: Record<string, number>): string {
  const parts = Object.entries(sizes)
    .filter(([, size]) => size > 0)
    .map(([key, size]) => `${key}: ${formatBytes(size)}`);
  return parts.length > 0 ? parts.join(", ") : "empty";
}

/**
 * `aft doctor --issue` flow — collect diagnostics, sanitize user paths,
 * prompt for an issue description, optionally file via `gh`.
 */
async function runIssueFlow(argv: string[]): Promise<number> {
  intro("AFT doctor --issue");

  const adapters = await resolveAdaptersForCommand(argv, {
    allowMulti: true,
    verb: "include in the issue",
  });

  const description = await text("Describe the problem you're running into:", {
    placeholder: "What happened? What did you expect? Steps to reproduce…",
    validate: (value) =>
      value.trim().length === 0 ? "Please enter a short description." : undefined,
  });

  const report = await collectDiagnostics(adapters);

  const logSections = adapters
    .map((adapter) => {
      const path = adapter.getLogFile();
      const tail = tailLogFile(path, 200);
      return `#### ${adapter.displayName} log (${path})\n\n\`\`\`\n${tail || "<no log output>"}\n\`\`\`\n`;
    })
    .join("\n");

  const rawBody = [
    "## Description",
    description,
    "",
    "## Environment",
    `- CLI: v${report.cliVersion}`,
    `- Binary: ${report.binaryVersion ?? "unknown"}`,
    `- OS: ${report.platform} ${report.arch}`,
    `- Node: ${report.nodeVersion}`,
    "",
    "## Diagnostics",
    renderDiagnosticsMarkdown(report),
    "",
    "## Logs (last 200 lines per harness)",
    logSections,
    "_Usernames and home paths have been stripped from this report._",
  ].join("\n");
  const body = sanitizeContent(rawBody);

  const title = `AFT issue: ${description.slice(0, 72)}`;
  const outPath = join(process.cwd(), `aft-issue-${Date.now()}.md`);
  writeFileSync(outPath, `${body}\n`);
  log.success(`Wrote sanitized issue body to ${outPath}`);

  if (isGhInstalled()) {
    log.info("Opening GitHub issue via `gh`…");
    const result = createGitHubIssue("cortexkit/aft", title, body);
    if (result.url) {
      log.success(`Issue filed: ${result.url}`);
      openBrowser(result.url);
      outro("Done.");
      return 0;
    }
    log.warn(`gh failed: ${result.stderr ?? "unknown error"}. Falling back to browser.`);
  }

  const fallback = `https://github.com/cortexkit/aft/issues/new?title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;
  log.info("Opening GitHub issue form in your browser…");
  openBrowser(fallback);
  note(
    `If the browser didn't open, the sanitized body is at ${outPath}. Copy it into a new issue at https://github.com/cortexkit/aft/issues/new.`,
    "Fallback",
  );
  outro("Done.");
  return 0;
}
