/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
const tempRoots = new Set<string>();

function createConfigFixture() {
  const root = mkdtempSync(join(tmpdir(), "aft-pi-config-tests-"));
  tempRoots.add(root);

  const home = join(root, "home");
  const userConfigDir = join(home, ".pi", "agent");
  const projectDirectory = join(root, "project");
  const projectConfigDir = join(projectDirectory, ".pi");

  mkdirSync(userConfigDir, { recursive: true });
  mkdirSync(projectConfigDir, { recursive: true });

  return {
    root,
    home,
    projectDirectory,
    userConfigPath: join(userConfigDir, "aft.jsonc"),
    projectConfigPath: join(projectConfigDir, "aft.jsonc"),
  };
}

function runConfigLoader(projectDirectory: string, env: Record<string, string>) {
  const script = `
    import { loadAftConfig } from "./src/config.ts";
    console.log(JSON.stringify(loadAftConfig(process.env.PROJECT_DIR!)));
  `;
  const result = spawnSync(process.execPath, ["-e", script], {
    cwd: packageRoot,
    env: { ...process.env, AFT_LOG_STDERR: "1", ...env, PROJECT_DIR: projectDirectory },
    encoding: "utf8",
  });

  expect(result.error).toBeUndefined();
  expect(result.status).toBe(0);

  return {
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("loadAftConfig", () => {
  test("loads object-map lsp servers with entry defaults", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify(
        {
          lsp: {
            servers: {
              tinymist: {
                extensions: [".typ"],
                binary: "tinymist",
              },
            },
          },
        },
        null,
        2,
      ),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    expect(JSON.parse(result.stdout)).toEqual({
      lsp: {
        servers: {
          tinymist: {
            extensions: [".typ"],
            binary: "tinymist",
            args: [],
            root_markers: [".git"],
            disabled: false,
          },
        },
      },
    });
  });

  test("rejects malformed lsp servers but keeps other config sections", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify(
        {
          format_on_edit: false,
          lsp: {
            servers: {
              tinymist: {
                extensions: [".typ"],
              },
            },
          },
        },
        null,
        2,
      ),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout) as Record<string, unknown>;
    expect(config.format_on_edit).toBe(false);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain("Partial config loaded — invalid sections skipped");
  });

  test("merges lsp config maps and disabled ids across user and project config", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            tinymist: { extensions: [".typ"], binary: "tinymist" },
          },
          disabled: ["pyright"],
        },
      }),
    );
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            bashls: { extensions: ["sh"], binary: "bash-language-server" },
          },
          disabled: ["yamlls"],
          python: "ty",
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(Object.keys(config.lsp.servers).sort()).toEqual(["bashls", "tinymist"]);
    expect(config.lsp.disabled).toEqual(["pyright", "yamlls"]);
    expect(config.lsp.python).toBe("ty");
  });
});
