/// <reference path="../../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { homedir, userInfo } from "node:os";

describe("sanitizeLogContent", () => {
  test("redacts usernames and home paths", async () => {
    const { sanitizeLogContent } = await import("../../cli/logs.js");
    const username = userInfo().username;
    const home = homedir();

    const input = [
      `user=${username}`,
      `home=${home}/project`,
      "/Users/alice/workspace/file.ts",
      "/home/bob/workspace/file.ts",
      `C:\\Users\\${username}\\repo\\file.ts`,
    ].join("\n");

    const sanitized = sanitizeLogContent(input);

    expect(sanitized).not.toContain(username);
    expect(sanitized).toContain("user=<USER>");
    expect(sanitized).toContain("home=~/project");
    expect(sanitized).toContain("/Users/<USER>/workspace/file.ts");
    expect(sanitized).toContain("/home/<USER>/workspace/file.ts");
    expect(sanitized).toContain("C:\\Users\\<USER>\\repo\\file.ts");
  });
});
