#!/usr/bin/env bun
/**
 * Generates subc_tool_schemas.json for the agent-file-tools crate.
 *
 * Run: bun run build:tool-schemas
 * Output: crates/aft/src/subc_tool_schemas.json
 */

import * as path from "node:path";
import { buildSubcToolSchemasJson } from "../src/subc-tool-schemas.js";

async function main() {
  const pluginRoot = path.resolve(import.meta.dir, "..");
  const repoRoot = path.resolve(pluginRoot, "..", "..");
  const outputPath = path.join(repoRoot, "crates", "aft", "src", "subc_tool_schemas.json");

  const json = buildSubcToolSchemasJson();
  await Bun.write(outputPath, json);

  const count = Object.keys(JSON.parse(json) as Record<string, unknown>).length;
  console.log(`✓ subc tool schemas (${count} tools) written: ${outputPath}`);
}

main();
