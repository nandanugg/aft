import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { constants } from "node:fs";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";

import { SubcClient, type CatalogEntry } from "@cortexkit/subc-client";

const AFT_BINARY_NAME = process.platform === "win32" ? "aft.exe" : "aft";
const SUBC_BINARY_NAME = process.platform === "win32" ? "subc-core.exe" : "subc-core";
const PROJECT_ROOT = resolve(import.meta.dir, "../../../../..");
const TARGET_DEBUG_BINARY = resolve(PROJECT_ROOT, "target", "debug", AFT_BINARY_NAME);
const FALLBACK_AFT_BINARY = resolve(homedir(), ".cargo", "bin", AFT_BINARY_NAME);
const DEFAULT_SUBC_RELEASE = resolve(
  homedir(),
  "Work/Projects/CortexKit/subconscious/target/release",
  SUBC_BINARY_NAME,
);
const DEFAULT_SUBC_DEBUG = resolve(
  homedir(),
  "Work/Projects/CortexKit/subconscious/target/debug",
  SUBC_BINARY_NAME,
);
const START_TIMEOUT_MS = 20_000;

export interface PreparedSubcLane {
  aftBinaryPath: string | null;
  subcCorePath: string | null;
  skipReason?: string;
  buildAttempted: boolean;
}

export interface SubcRig {
  readonly tempDir: string;
  readonly homeDir: string;
  readonly configHome: string;
  readonly runtimeDir: string;
  readonly dataHome: string;
  readonly projectDir: string;
  readonly connectionFile: string;
  readonly daemonPid: number | undefined;
  cleanup(): Promise<void>;
}

let preparedPromise: Promise<PreparedSubcLane> | null = null;

export function prepareSubcLane(): Promise<PreparedSubcLane> {
  preparedPromise ??= prepareSubcLaneOnce();
  return preparedPromise;
}

export async function startSubcRig(prepared: PreparedSubcLane): Promise<SubcRig> {
  if (!prepared.subcCorePath || !prepared.aftBinaryPath) {
    throw new Error(prepared.skipReason ?? "subc e2e dependencies unavailable");
  }

  const tempDir = await mkdtemp(join(tmpdir(), "aft-subc-lane-"));
  const homeDir = join(tempDir, "home");
  const configHome = join(tempDir, "config");
  const runtimeDir = join(tempDir, "runtime");
  const dataHome = join(tempDir, "data");
  const projectDir = join(tempDir, "project");
  const cacheDir = join(tempDir, "cache");
  const storageDir = join(tempDir, "aft-storage");
  const connectionFile = join(runtimeDir, "subc-connection.json");
  const stderrPath = join(tempDir, "subc-core.stderr.log");

  await mkdir(join(configHome, "cortexkit"), { recursive: true });
  await mkdir(join(projectDir, ".cortexkit"), { recursive: true });
  await Promise.all([mkdir(homeDir, { recursive: true }), mkdir(runtimeDir, { recursive: true })]);
  await Promise.all([mkdir(dataHome, { recursive: true }), mkdir(cacheDir, { recursive: true })]);

  await writeFile(
    join(configHome, "cortexkit", "aft.jsonc"),
    JSON.stringify(
      {
        storage_dir: storageDir,
        search_index: false,
        semantic_search: false,
        experimental_bash_background: true,
        bash_permissions: false,
      },
      null,
      2,
    ),
    "utf8",
  );
  await writeFile(
    join(projectDir, ".cortexkit", "aft.jsonc"),
    JSON.stringify({ search_index: false, semantic_search: false }, null, 2),
    "utf8",
  );
  await writeFile(join(projectDir, "seed.txt"), "seed\n", "utf8");
  await runGit(projectDir, ["init", "-q", "-b", "main"]);
  await runGit(projectDir, ["config", "user.email", "subc-lane@example.com"]);
  await runGit(projectDir, ["config", "user.name", "Subc Lane"]);
  await runGit(projectDir, ["config", "commit.gpgsign", "false"]);
  await runGit(projectDir, ["add", "."]);
  await runGit(projectDir, ["commit", "-q", "-m", "seed"]);

  await writeFile(
    join(configHome, "cortexkit", "subc.jsonc"),
    JSON.stringify(
      {
        version: 1,
        port: 0,
        storage: { backend: "sqlite", data_home: dataHome },
        modules: {
          aft: {
            program: prepared.aftBinaryPath,
            args: [],
            env: {
              HOME: homeDir,
              XDG_CONFIG_HOME: configHome,
              XDG_DATA_HOME: dataHome,
              AFT_CACHE_DIR: cacheDir,
              AFT_CALLGRAPH_BUILD_WAIT_MS: "15000",
            },
            enabled: true,
          },
        },
      },
      null,
      2,
    ),
    "utf8",
  );

  let daemon: ChildProcessWithoutNullStreams | null = null;
  try {
    daemon = await spawnReadyDaemon(prepared.subcCorePath, {
      homeDir,
      configHome,
      runtimeDir,
      dataHome,
      stderrPath,
      connectionFile,
    });
  } catch (err) {
    await safeRemoveDir(tempDir);
    throw err;
  }

  let cleaned = false;
  return {
    tempDir,
    homeDir,
    configHome,
    runtimeDir,
    dataHome,
    projectDir,
    connectionFile,
    daemonPid: daemon.pid,
    cleanup: async () => {
      if (cleaned) return;
      cleaned = true;
      await stopDaemon(daemon);
      await safeRemoveDir(tempDir);
    },
  };
}

async function prepareSubcLaneOnce(): Promise<PreparedSubcLane> {
  const subc = await resolveSubcCore();
  if (!subc.path) {
    return { aftBinaryPath: null, subcCorePath: null, skipReason: subc.skipReason, buildAttempted: false };
  }

  const aft = await prepareAftBinary();
  if (!aft.binaryPath) {
    return {
      aftBinaryPath: null,
      subcCorePath: subc.path,
      skipReason: aft.skipReason,
      buildAttempted: aft.buildAttempted,
    };
  }

  return {
    aftBinaryPath: aft.binaryPath,
    subcCorePath: subc.path,
    buildAttempted: aft.buildAttempted,
  };
}

async function resolveSubcCore(): Promise<{ path: string | null; skipReason?: string }> {
  const envPath = process.env.SUBC_CORE_BIN?.trim();
  if (envPath) {
    if (await isExecutable(envPath)) return { path: envPath };
    return { path: null, skipReason: `SUBC_CORE_BIN is not executable: ${envPath}` };
  }
  for (const candidate of [DEFAULT_SUBC_RELEASE, DEFAULT_SUBC_DEBUG]) {
    if (await isExecutable(candidate)) return { path: candidate };
  }
  return {
    path: null,
    skipReason: `subc-core binary not found at ${DEFAULT_SUBC_RELEASE} or ${DEFAULT_SUBC_DEBUG}; set SUBC_CORE_BIN`,
  };
}

async function prepareAftBinary(): Promise<{
  binaryPath: string | null;
  skipReason?: string;
  buildAttempted: boolean;
}> {
  if (await isExecutable(TARGET_DEBUG_BINARY)) {
    return { binaryPath: TARGET_DEBUG_BINARY, buildAttempted: false };
  }

  const build = await runCargoBuild();
  if (await isExecutable(TARGET_DEBUG_BINARY)) {
    return { binaryPath: TARGET_DEBUG_BINARY, buildAttempted: true };
  }
  if (await isExecutable(FALLBACK_AFT_BINARY)) {
    return { binaryPath: FALLBACK_AFT_BINARY, buildAttempted: true };
  }

  const searched = [TARGET_DEBUG_BINARY, FALLBACK_AFT_BINARY]
    .map((path) => relative(PROJECT_ROOT, path))
    .join(" or ");
  const skipReason = build.ok
    ? `aft binary not found at ${searched}`
    : `cargo build failed and no fallback aft binary was found\n${build.output}`;
  if (process.env.CI === "true") {
    throw new Error(`e2e setup failed: ${skipReason}`);
  }
  return { binaryPath: null, skipReason, buildAttempted: true };
}

async function spawnReadyDaemon(
  subcCorePath: string,
  dirs: {
    homeDir: string;
    configHome: string;
    runtimeDir: string;
    dataHome: string;
    stderrPath: string;
    connectionFile: string;
  },
): Promise<ChildProcessWithoutNullStreams> {
  let lastError: unknown;
  for (let attempt = 0; attempt < 2; attempt++) {
    const daemon = spawn(subcCorePath, [], {
      env: {
        ...process.env,
        HOME: dirs.homeDir,
        XDG_CONFIG_HOME: dirs.configHome,
        XDG_RUNTIME_DIR: dirs.runtimeDir,
        XDG_DATA_HOME: dirs.dataHome,
        SUBC_PORT: "",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stderrChunks: string[] = [];
    daemon.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString("utf8");
      stderrChunks.push(text);
      void writeFile(dirs.stderrPath, stderrChunks.join(""), "utf8").catch(() => undefined);
    });
    daemon.stdout.resume();

    try {
      await waitForAftCatalog(dirs.connectionFile, daemon, stderrChunks, START_TIMEOUT_MS);
      return daemon;
    } catch (err) {
      lastError = err;
      await stopDaemon(daemon);
      await rm(dirs.connectionFile, { force: true }).catch(() => undefined);
    }
  }
  throw lastError instanceof Error ? lastError : new Error(String(lastError));
}

async function waitForAftCatalog(
  connectionFile: string,
  daemon: ChildProcessWithoutNullStreams,
  stderrChunks: string[],
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError = "connection file not ready";
  while (Date.now() < deadline) {
    if (daemon.exitCode !== null) {
      throw new Error(
        `subc-core exited before aft registered (exit ${daemon.exitCode}); stderr:\n${stderrChunks.join("")}`,
      );
    }
    let client: SubcClient | null = null;
    try {
      client = await SubcClient.connect({ connectionFile, handshakeTimeoutMs: 1_000 });
      const entries = await client.catalogList("aft");
      if (entries.some(isAftToolProvider)) return;
      lastError = `catalog missing aft tool_provider; saw ${JSON.stringify(entries)}`;
    } catch (err) {
      lastError = err instanceof Error ? err.message : String(err);
    } finally {
      client?.close();
    }
    await sleep(200);
  }
  throw new Error(`timed out waiting for aft in subc catalog: ${lastError}`);
}

function isAftToolProvider(entry: CatalogEntry): boolean {
  return (
    entry.module_id === "aft" &&
    Array.isArray(entry.roles) &&
    entry.roles.some((role) => {
      if (!role || typeof role !== "object") return false;
      return (role as { role?: unknown }).role === "tool_provider";
    })
  );
}

async function stopDaemon(daemon: ChildProcessWithoutNullStreams | null): Promise<void> {
  if (!daemon || daemon.exitCode !== null) return;
  const close = new Promise<void>((resolveClose) => daemon.once("close", () => resolveClose()));
  daemon.kill("SIGTERM");
  const stopped = await Promise.race([close.then(() => true), sleep(2_000).then(() => false)]);
  if (!stopped && daemon.exitCode === null) {
    daemon.kill("SIGKILL");
    await Promise.race([close, sleep(2_000)]);
  }
}

async function runGit(cwd: string, args: string[]): Promise<void> {
  const result = await runProcess("git", args, cwd);
  if (result.code !== 0) {
    throw new Error(`git ${args.join(" ")} failed (${result.code}): ${result.output}`);
  }
}

async function runCargoBuild(): Promise<{ ok: boolean; output: string }> {
  const result = await runProcess("cargo", ["build"], PROJECT_ROOT);
  return { ok: result.code === 0, output: result.output };
}

async function runProcess(
  command: string,
  args: string[],
  cwd: string,
): Promise<{ code: number; output: string }> {
  return new Promise((resolveRun) => {
    const child = spawn(command, args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => resolveRun({ code: 1, output: error.message }));
    child.on("close", (code) => resolveRun({ code: code ?? 1, output: `${stdout}${stderr}`.trim() }));
  });
}

async function isExecutable(filePath: string): Promise<boolean> {
  try {
    const mode = process.platform === "win32" ? constants.F_OK : constants.X_OK;
    await access(filePath, mode);
    return true;
  } catch {
    return false;
  }
}

async function safeRemoveDir(dir: string): Promise<void> {
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      await rm(dir, { recursive: true, force: true });
      return;
    } catch {
      await sleep(100);
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}
