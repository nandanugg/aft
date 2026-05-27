export type AgentArm = "aft" | "codegraph";
export type AgentTaskKind = "answer" | "edit";

export interface AnswerContainsCheck {
  type: "answer_contains";
  values: string[];
}

export interface FileContainsCheck {
  type: "file_contains";
  path: string;
  value: string;
}

export interface FileNotContainsCheck {
  type: "file_not_contains";
  path: string;
  value: string;
}

export interface CommandCheck {
  type: "command";
  command: string;
}

export type AgentCheck = AnswerContainsCheck | FileContainsCheck | FileNotContainsCheck | CommandCheck;

export interface AgentTask {
  id: string;
  kind: AgentTaskKind;
  prompt: string;
  checks: AgentCheck[];
  timeoutMs?: number;
}

export interface AgentCorpus {
  name: string;
  description?: string;
  fixturePath: string;
  tasks: AgentTask[];
}

export interface TokenUsage {
  input: number;
  output: number;
  total: number;
}

export interface AgentRunResult {
  arm: AgentArm;
  taskId: string;
  kind: AgentTaskKind;
  success: boolean;
  model: string;
  attemptedModel: string;
  wallTimeMs: number;
  exitCode: number;
  tokens: TokenUsage;
  toolCalls: number;
  answerText: string;
  stdoutPath: string;
  stderrPath: string;
  repoPath: string;
  checks: Array<{ check: AgentCheck; pass: boolean; detail?: string }>;
  error?: string;
}

export interface AgentReport {
  benchmark: "codegraph-vs-aft-agent";
  timestamp: string;
  model: string;
  fallbackModel: string;
  corpus: string;
  arms: AgentArm[];
  taskCount: number;
  summary: Record<AgentArm, {
    runs: number;
    successRate: number;
    successes: number;
    tokensTotal: number;
    tokensMedian: number;
    wallTimeMsMedian: number;
    wallTimeMsP95: number;
    toolCallsMedian: number;
  }>;
  results: AgentRunResult[];
  metadata: Record<string, unknown>;
}
