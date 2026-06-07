export interface PipeStripResult {
  command: string;
  stripped: boolean;
  note?: string;
}

const NOISE_FILTERS = new Set(["grep", "rg", "head", "tail", "cat", "less", "more"]);
const GREP_GUARD_FLAGS = new Set([
  "c",
  "count",
  "q",
  "quiet",
  "o",
  "only-matching",
  "l",
  "files-with-matches",
]);

export function maybeStripCompressorPipe(
  command: string,
  compressionEnabled: boolean,
): PipeStripResult {
  if (!compressionEnabled) return { command, stripped: false };

  // Peel a leading `cmd && ... &&` prefix (e.g. `cd dir && bun test | grep`).
  // Since `&&` binds looser than `|`, `A && B | C` means `A && (B | C)`, so the
  // pipeline to strip is the LAST `&&`-segment and the earlier segments are a
  // verbatim prefix to reattach. Bail on top-level `||`/`;` (ambiguous/risky).
  const chain = splitTopLevelAndChain(command);
  if (chain === null) return { command, stripped: false };
  const prefix = chain
    .slice(0, -1)
    .map((segment) => segment.trim())
    .filter(Boolean);
  const pipeline = chain[chain.length - 1] ?? "";

  const stages = splitTopLevelPipeline(pipeline);
  if (stages.length < 2) return { command, stripped: false };

  const firstStage = stages[0]?.trim() ?? "";
  if (!isCompressorHandledRunner(firstStage)) return { command, stripped: false };

  const filterStages = stages.slice(1).map((stage) => stage.trim());
  for (const stage of filterStages) {
    if (!isPlainNoiseFilter(stage)) return { command, stripped: false };
  }

  const filters = filterStages.join(" | ");
  const rebuilt = [...prefix, firstStage].join(" && ");
  return {
    command: rebuilt,
    stripped: true,
    note: `[AFT dropped \`| ${filters}\` (compressed:false to keep)]`,
  };
}

/**
 * Split a command into its top-level `&&`-joined segments, respecting quotes
 * and escapes. Returns `null` if the command contains a top-level `||` or `;`,
 * which make prefix-peeling ambiguous, so the caller bails. Single `&`
 * (redirects like `2>&1`, background) is left intact inside a segment.
 */
function splitTopLevelAndChain(command: string): string[] | null {
  const segments: string[] = [];
  let start = 0;
  let quote: "'" | '"' | null = null;
  let escaped = false;

  for (let index = 0; index < command.length; index++) {
    const char = command[index];
    const next = command[index + 1];

    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }
    if (quote) {
      if (char === quote) quote = null;
      continue;
    }
    if (char === "'" || char === '"') {
      quote = char;
      continue;
    }

    if (char === "&" && next === "&") {
      segments.push(command.slice(start, index));
      start = index + 2;
      index++;
      continue;
    }
    if (char === "|" && next === "|") return null;
    if (char === ";") return null;
  }

  segments.push(command.slice(start));
  return segments;
}

function splitTopLevelPipeline(command: string): string[] {
  const stages: string[] = [];
  let start = 0;
  let quote: "'" | '"' | null = null;
  let escaped = false;

  for (let index = 0; index < command.length; index++) {
    const char = command[index];
    const next = command[index + 1];
    const previous = command[index - 1];

    if (escaped) {
      escaped = false;
      continue;
    }

    if (char === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }

    if (quote) {
      if (char === quote) quote = null;
      continue;
    }

    if (char === "'" || char === '"') {
      quote = char;
      continue;
    }

    if (char === "|" && previous !== "|" && next !== "|") {
      stages.push(command.slice(start, index));
      start = index + 1;
    }
  }

  stages.push(command.slice(start));
  return stages;
}

function isCompressorHandledRunner(stage: string): boolean {
  const tokens = tokenizeStage(stage);
  if (tokens.length === 0) return false;

  const [first, second, third] = tokens;
  if (!first) return false;
  if (tokens.some((token) => token === "&&" || token === "||" || token.includes(";"))) {
    return false;
  }

  if (first === "bun") return second === "test" || (second === "run" && startsWithTest(third));
  if (first === "cargo") return ["test", "build", "check", "clippy"].includes(second ?? "");
  if (first === "go") return second === "test" || second === "build";
  if (["npm", "pnpm"].includes(first)) {
    return second === "test" || (second === "run" && startsWithTest(third));
  }
  if (first === "yarn") return second === "test";
  if (first === "playwright") return second === "test";
  if (first === "npx") return ["tsc", "eslint", "vitest", "jest"].includes(second ?? "");
  return ["vitest", "jest", "pytest", "tsc", "eslint", "biome", "ruff", "mypy"].includes(first);
}

function startsWithTest(token: string | undefined): boolean {
  return token?.startsWith("test") === true;
}

function isPlainNoiseFilter(stage: string): boolean {
  const tokens = tokenizeStage(stage);
  const head = tokens[0];
  if (!head) return false;
  if (head === "wc") return false;
  if (!NOISE_FILTERS.has(head)) return false;
  if ((head === "grep" || head === "rg") && hasIntentChangingGrepFlag(tokens.slice(1)))
    return false;
  return true;
}

function hasIntentChangingGrepFlag(args: string[]): boolean {
  for (const arg of args) {
    if (arg === "--") return false;
    if (!arg.startsWith("-") || arg === "-") continue;
    if (arg.startsWith("--")) {
      const flag = arg.slice(2).split("=", 1)[0];
      if (GREP_GUARD_FLAGS.has(flag)) return true;
      continue;
    }
    for (const flag of arg.slice(1)) {
      if (GREP_GUARD_FLAGS.has(flag)) return true;
    }
  }
  return false;
}

function tokenizeStage(stage: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quote: "'" | '"' | null = null;
  let escaped = false;

  for (let index = 0; index < stage.length; index++) {
    const char = stage[index];

    if (escaped) {
      current += char;
      escaped = false;
      continue;
    }

    if (char === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }

    if (quote) {
      if (char === quote) {
        quote = null;
      } else {
        current += char;
      }
      continue;
    }

    if (char === "'" || char === '"') {
      quote = char;
      continue;
    }

    if (/\s/.test(char)) {
      if (current.length > 0) {
        tokens.push(current);
        current = "";
      }
      continue;
    }

    current += char;
  }

  if (current.length > 0) tokens.push(current);
  return tokens;
}
