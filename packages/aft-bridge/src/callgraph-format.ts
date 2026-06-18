/**
 * Shared flat-text formatter for aft_callgraph responses (agent + themed TUI).
 */

import { homedir } from "node:os";

export interface CallgraphTheme {
  fg(role: string, text: string): string;
}

export const PLAIN_CALLGRAPH_THEME: CallgraphTheme = {
  fg: (_role, text) => text,
};

function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  return value as Record<string, unknown>;
}

function asRecords(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? (value.map(asRecord).filter(Boolean) as Record<string, unknown>[])
    : [];
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function asBoolean(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

function shortenPath(path: string): string {
  const home = homedir();
  if (path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

function joinNonEmpty(parts: Array<string | undefined>, separator = " · "): string {
  return parts.filter((part): part is string => Boolean(part && part.length > 0)).join(separator);
}

function treeLine(depth: number, text: string): string {
  return `${"  ".repeat(depth)}${depth === 0 ? "" : "↳ "}${text}`;
}

/** Marks edges resolved purely by callee name (may be the wrong homonym). */
function nameMatchEdgeMarker(record: Record<string, unknown>, theme: CallgraphTheme): string {
  return asString(record.resolved_by) === "name_match" ? ` ${theme.fg("warning", "~")}` : "";
}

function renderCallTreeNode(
  node: Record<string, unknown>,
  depth: number,
  lines: string[],
  theme: CallgraphTheme,
): void {
  const name = asString(node.name) ?? "(unknown)";
  const file = shortenPath(asString(node.file) ?? "(unknown file)");
  const line = asNumber(node.line);
  // `resolved:false` means the callee could not be resolved to a definition —
  // the file/line is the CALLSITE in the caller, not the callee's definition.
  // Mark it so the agent doesn't read the callsite as the definition location.
  // Only when explicitly false (resolved/legacy nodes omit the field).
  const unresolved = node.resolved === false ? ` ${theme.fg("warning", "[unresolved]")}` : "";
  const nameMatch = nameMatchEdgeMarker(node, theme);
  const location = line !== undefined ? `[${file}:${line}]` : `[${file}]`;
  lines.push(treeLine(depth, `${name} ${location}${unresolved}${nameMatch}`));
  asRecords(node.children).forEach((child) => {
    renderCallTreeNode(child, depth + 1, lines, theme);
  });
}

function depthWarning(
  response: Record<string, unknown>,
  theme: CallgraphTheme,
  depthField = "depth_limited",
  truncatedField = "truncated",
): string {
  const limited = asBoolean(response[depthField]);
  const truncated = asNumber(response[truncatedField]) ?? 0;
  if (!limited && truncated === 0) return "";
  const detail = truncated > 0 ? `, ${truncated} truncated` : "";
  return theme.fg("warning", `(depth limited${detail})`);
}

function renderTracePath(
  path: Record<string, unknown>,
  index: number,
  lines: string[],
  theme: CallgraphTheme,
): void {
  lines.push(`Path ${index + 1}`);
  asRecords(path.hops).forEach((hop, hopIndex) => {
    const symbol = asString(hop.symbol) ?? "(unknown)";
    const file = shortenPath(asString(hop.file) ?? "(unknown file)");
    const line = asNumber(hop.line);
    const entry = hop.is_entry_point === true ? " [entry]" : "";
    const nameMatch = nameMatchEdgeMarker(hop, theme);
    lines.push(
      treeLine(
        hopIndex + 1,
        `${symbol}${entry} ${line !== undefined ? `[${file}:${line}]` : `[${file}]`}${nameMatch}`,
      ),
    );
  });
}

function renderCallersGroupLines(group: Record<string, unknown>, theme: CallgraphTheme): string[] {
  const file = shortenPath(asString(group.file) ?? "(unknown file)");
  const lines = [theme.fg("accent", file)];
  const callers = asRecords(group.callers);

  const bySymbolProvenance = new Map<string, number[]>();
  for (const caller of callers) {
    const symbol = asString(caller.symbol) ?? "(unknown)";
    const provenanceKey =
      asString(caller.resolved_by) === "name_match" ? `${symbol}\0name_match` : `${symbol}\0exact`;
    const line = asNumber(caller.line);
    const bucket = bySymbolProvenance.get(provenanceKey) ?? [];
    if (line !== undefined) bucket.push(line);
    bySymbolProvenance.set(provenanceKey, bucket);
  }

  const keys = [...bySymbolProvenance.keys()].sort((a, b) => a.localeCompare(b));
  for (const key of keys) {
    const symbol = key.split("\0")[0] ?? "(unknown)";
    const isNameMatch = key.endsWith("\0name_match");
    const lineNums = (bySymbolProvenance.get(key) ?? []).sort((a, b) => a - b);
    const linePart = lineNums.length > 0 ? lineNums.map(String).join(", ") : "?";
    const marker = isNameMatch ? ` ${theme.fg("warning", "~")}` : "";
    lines.push(`  ↳ ${symbol}:${linePart}${marker}`);
  }

  return lines;
}

export function formatCallgraphSections(
  op: string,
  response: unknown,
  theme: CallgraphTheme = PLAIN_CALLGRAPH_THEME,
): string[] {
  const record = asRecord(response);
  if (!record) return [theme.fg("muted", "No navigation result.")];

  if (op === "call_tree") {
    const lines: string[] = [];
    renderCallTreeNode(record, 0, lines, theme);
    const warning = depthWarning(record, theme);
    if (warning) lines.push(warning);
    return lines.length > 0 ? lines : [theme.fg("muted", "No call tree available.")];
  }

  if (op === "callers") {
    const groups = asRecords(record.callers);
    const warning = depthWarning(record, theme);
    const total = asNumber(record.total_callers) ?? 0;
    const sections = [
      joinNonEmpty([
        theme.fg("success", `${total} caller${total === 1 ? "" : "s"}`),
        theme.fg("muted", `${groups.length} file group${groups.length === 1 ? "" : "s"}`),
        warning,
      ]),
    ];
    groups.forEach((group) => {
      sections.push(renderCallersGroupLines(group, theme).join("\n"));
    });
    return sections;
  }

  if (op === "trace_to_symbol") {
    const path = asRecords(record.path);
    const complete = asBoolean(record.complete);
    const reason = asString(record.reason);
    if (path.length === 0) {
      const prefix =
        complete === false ? theme.fg("warning", "No complete path") : theme.fg("muted", "No path");
      return [`${prefix}${reason ? ` (${reason})` : ""}`];
    }
    const lines = [theme.fg("success", `${path.length} hop${path.length === 1 ? "" : "s"}`)];
    path.forEach((hop, index) => {
      const symbol = asString(hop.symbol) ?? "(unknown)";
      const file = shortenPath(asString(hop.file) ?? "(unknown file)");
      const line = asNumber(hop.line);
      const nameMatch = nameMatchEdgeMarker(hop, theme);
      lines.push(
        treeLine(
          index + 1,
          `${symbol} ${line !== undefined ? `[${file}:${line}]` : `[${file}]`}${nameMatch}`,
        ),
      );
    });
    return lines;
  }

  if (op === "trace_to") {
    const paths = asRecords(record.paths);
    const warning = depthWarning(record, theme, "max_depth_reached", "truncated_paths");
    const totalPaths = asNumber(record.total_paths) ?? paths.length;
    const entryPoints = asNumber(record.entry_points_found) ?? 0;
    const sections = [
      joinNonEmpty([
        theme.fg("success", `${totalPaths} path${totalPaths === 1 ? "" : "s"}`),
        theme.fg("muted", `${entryPoints} entry point${entryPoints === 1 ? "" : "s"}`),
        warning,
      ]),
    ];
    if (paths.length === 0) sections.push(theme.fg("muted", "No entry paths found."));
    paths.forEach((path, index) => {
      const lines: string[] = [];
      renderTracePath(path, index, lines, theme);
      sections.push(lines.join("\n"));
    });
    return sections;
  }

  if (op === "impact") {
    const callers = asRecords(record.callers);
    const warning = depthWarning(record, theme);
    const totalAffected = asNumber(record.total_affected) ?? callers.length;
    const affectedFiles = asNumber(record.affected_files) ?? 0;
    const sections = [
      joinNonEmpty([
        theme.fg("warning", `${totalAffected} affected call site${totalAffected === 1 ? "" : "s"}`),
        theme.fg("muted", `${affectedFiles} file${affectedFiles === 1 ? "" : "s"}`),
        warning,
      ]),
    ];
    if (callers.length === 0) sections.push(theme.fg("muted", "No impacted callers found."));
    callers.forEach((caller) => {
      const file = shortenPath(asString(caller.caller_file) ?? "(unknown file)");
      const symbol = asString(caller.caller_symbol) ?? "(unknown)";
      const line = asNumber(caller.line) ?? 0;
      const entry = caller.is_entry_point === true ? ` ${theme.fg("warning", "[entry]")}` : "";
      const nameMatch = nameMatchEdgeMarker(caller, theme);
      const expression = asString(caller.call_expression);
      const params = Array.isArray(caller.parameters)
        ? caller.parameters.map(String).join(", ")
        : "";
      sections.push(
        [
          `${theme.fg("accent", file)}:${line}`,
          `  ↳ ${symbol}${entry}${nameMatch}`,
          expression ? `  ${theme.fg("muted", expression)}` : undefined,
          params ? `  ${theme.fg("muted", `params: ${params}`)}` : undefined,
        ]
          .filter(Boolean)
          .join("\n"),
      );
    });
    return sections;
  }

  const hops = asRecords(record.hops);
  const sections = [
    joinNonEmpty([
      theme.fg("success", `${hops.length} hop${hops.length === 1 ? "" : "s"}`),
      asBoolean(record.depth_limited) ? theme.fg("warning", "(depth limited)") : undefined,
    ]),
  ];
  if (hops.length === 0) sections.push(theme.fg("muted", "No data-flow hops found."));
  hops.forEach((hop, index) => {
    const file = shortenPath(asString(hop.file) ?? "(unknown file)");
    const symbol = asString(hop.symbol) ?? "(unknown)";
    const variable = asString(hop.variable) ?? "(unknown)";
    const line = asNumber(hop.line) ?? 0;
    const approximate = hop.approximate === true ? ` ${theme.fg("warning", "[approx]")}` : "";
    const nameMatch = nameMatchEdgeMarker(hop, theme);
    sections.push(
      treeLine(
        index,
        `${variable} ${theme.fg("muted", `${asString(hop.flow_type) ?? "flow"}`)} ${symbol} [${file}:${line}]${approximate}${nameMatch}`,
      ),
    );
  });
  return sections;
}
