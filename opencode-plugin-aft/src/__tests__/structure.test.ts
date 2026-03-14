import { describe, test, expect, afterEach } from "bun:test";
import { BinaryBridge } from "../bridge.js";
import { structureTools } from "../tools/structure.js";
import { editingTools } from "../tools/editing.js";
import { resolve } from "node:path";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";

const BINARY_PATH = resolve(import.meta.dir, "../../../target/debug/aft");
const PROJECT_CWD = resolve(import.meta.dir, "../../..");

const TEST_TIMEOUT_MS = 10_000;

describe("Structure tool registrations", () => {
  test("structureTools returns all 5 tool definitions", () => {
    // Use a dummy bridge — we're only checking registration, not execution
    const fakeBridge = {} as BinaryBridge;
    const tools = structureTools(fakeBridge);

    const names = Object.keys(tools);
    expect(names).toContain("add_member");
    expect(names).toContain("add_derive");
    expect(names).toContain("wrap_try_catch");
    expect(names).toContain("add_decorator");
    expect(names).toContain("add_struct_tags");
    expect(names.length).toBe(5);
  });

  test("each tool has a description and args", () => {
    const fakeBridge = {} as BinaryBridge;
    const tools = structureTools(fakeBridge);

    for (const [name, def] of Object.entries(tools)) {
      expect(def.description).toBeTruthy();
      expect(typeof def.description).toBe("string");
      expect(def.args).toBeTruthy();
      expect(typeof def.execute).toBe("function");
    }
  });

  test("add_member args include file, scope, code, and optional position", () => {
    const fakeBridge = {} as BinaryBridge;
    const tools = structureTools(fakeBridge);
    const args = tools.add_member.args;

    expect(args.file).toBeDefined();
    expect(args.scope).toBeDefined();
    expect(args.code).toBeDefined();
    expect(args.position).toBeDefined();
  });

  test("add_derive args include file, target, derives", () => {
    const fakeBridge = {} as BinaryBridge;
    const tools = structureTools(fakeBridge);
    const args = tools.add_derive.args;

    expect(args.file).toBeDefined();
    expect(args.target).toBeDefined();
    expect(args.derives).toBeDefined();
  });

  test("add_struct_tags args include file, target, field, tag, value", () => {
    const fakeBridge = {} as BinaryBridge;
    const tools = structureTools(fakeBridge);
    const args = tools.add_struct_tags.args;

    expect(args.file).toBeDefined();
    expect(args.target).toBeDefined();
    expect(args.field).toBeDefined();
    expect(args.tag).toBeDefined();
    expect(args.value).toBeDefined();
  });
});

describe("Structure tool round-trips", () => {
  let bridge: BinaryBridge;
  let tmpDir: string | null = null;

  const createBridge = () => {
    bridge = new BinaryBridge(BINARY_PATH, PROJECT_CWD, {
      timeoutMs: TEST_TIMEOUT_MS,
    });
    return bridge;
  };

  afterEach(async () => {
    if (bridge) {
      await bridge.shutdown();
    }
    if (tmpDir) {
      await rm(tmpDir, { recursive: true, force: true });
      tmpDir = null;
    }
  });

  test("add_member inserts a method into a TypeScript class", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "example.ts");
    const original = `export class Greeter {\n  name: string;\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.add_member.execute({
      file: filePath,
      scope: "Greeter",
      code: "greet() { return 'hello'; }",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.scope).toBe("Greeter");
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("greet()");
  });

  test("add_member with position=first inserts at top of class", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "pos.ts");
    const original = `class Foo {\n  existing() {}\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.add_member.execute({
      file: filePath,
      scope: "Foo",
      code: "first() {}",
      position: "first",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);

    const content = await readFile(filePath, "utf-8");
    const firstIdx = content.indexOf("first()");
    const existingIdx = content.indexOf("existing()");
    expect(firstIdx).toBeLessThan(existingIdx);
  });

  test("add_derive adds a derive to a Rust struct", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "example.rs");
    const original = `#[derive(Debug)]\nstruct Foo {\n    x: i32,\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.add_derive.execute({
      file: filePath,
      target: "Foo",
      derives: ["Clone", "PartialEq"],
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.derives).toContain("Debug");
    expect(result.derives).toContain("Clone");
    expect(result.derives).toContain("PartialEq");

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("Clone");
    expect(content).toContain("PartialEq");
  });

  test("wrap_try_catch wraps a function body", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "wrap.ts");
    const original = `function doWork() {\n  const x = 1;\n  return x;\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.wrap_try_catch.execute({
      file: filePath,
      target: "doWork",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("try {");
    expect(content).toContain("catch");
  });

  test("wrap_try_catch with custom catch_body", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "wrap2.ts");
    const original = `function risky() {\n  throw new Error("boom");\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.wrap_try_catch.execute({
      file: filePath,
      target: "risky",
      catch_body: 'console.error("failed:", error);',
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("console.error");
  });

  test("add_decorator inserts a Python decorator", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "example.py");
    const original = `class MyClass:\n    def method(self):\n        pass\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.add_decorator.execute({
      file: filePath,
      target: "method",
      decorator: "staticmethod",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("@staticmethod");
  });

  test("add_struct_tags adds a Go struct tag", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "example.go");
    const original = `package main\n\ntype User struct {\n\tName string\n\tAge  int\n}\n`;
    await writeTools.write.execute({ file: filePath, content: original });

    const resultStr = await tools.add_struct_tags.execute({
      file: filePath,
      target: "User",
      field: "Name",
      tag: "json",
      value: "name,omitempty",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.tag_string).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("json");
    expect(content).toContain("name,omitempty");
  });

  test("add_member returns scope_not_found for missing scope", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "missing.ts");
    await writeTools.write.execute({
      file: filePath,
      content: `class Real {\n  x: number;\n}\n`,
    });

    const resultStr = await tools.add_member.execute({
      file: filePath,
      scope: "NonExistent",
      code: "y: string;",
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(false);
    expect(result.code).toBe("scope_not_found");
  });

  test("add_derive returns target_not_found for missing struct", async () => {
    createBridge();
    const writeTools = editingTools(bridge);
    const tools = structureTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));

    const filePath = resolve(tmpDir, "missing.rs");
    await writeTools.write.execute({
      file: filePath,
      content: `struct Real {\n    x: i32,\n}\n`,
    });

    const resultStr = await tools.add_derive.execute({
      file: filePath,
      target: "Fake",
      derives: ["Clone"],
    });
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(false);
    expect(result.code).toBe("target_not_found");
  });
});
