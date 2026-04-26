import { describe, expect, test } from "bun:test";
import {
  coerceAftStatus,
  formatStatusDialogMessage,
  formatStatusMarkdown,
} from "../shared/status.js";

const baseResponse = Object.freeze({
  version: "0.0.0-test",
  project_root: "/tmp/project",
  features: {
    format_on_edit: false,
    validate_on_edit: "off",
    restrict_to_project_root: false,
    experimental_search_index: true,
    experimental_semantic_search: true,
  },
  search_index: { status: "ready", files: 4, trigrams: 400 },
  semantic_index: {
    status: "ready",
    entries: 128,
    dimension: 384,
  },
  go_overlay: {
    backend: "aft_go_sidecar",
    state: "ready",
    provider_id: "go-sidecar@sha256",
    provider_version: "0.0.1",
    schema_version: 2,
    feature_hash: "abc123",
    env_hash: "env999",
    source_fingerprint: "src123",
    produced_at: "2025-01-01T00:00:00Z",
    last_error: null,
  },
  disk: {
    storage_dir: "/tmp/storage",
    trigram_disk_bytes: 1024,
    semantic_disk_bytes: 2048,
  },
  lsp_servers: 2,
  symbol_cache: { local_entries: 3, warm_entries: 6 },
  storage_dir: "/tmp/storage",
  semantic: {
    backend: "openai_compatible",
    model: "text-embedding-3-small",
    api_key_env: "AFT_SEMANTIC_KEY",
  },
});

describe("coerceAftStatus", () => {
  test("adds backend and model when provided", () => {
    const status = coerceAftStatus(baseResponse as unknown as Record<string, unknown>);

    expect(status.semantic_index.backend).toBe("openai_compatible");
    expect(status.semantic_index.model).toBe("text-embedding-3-small");
    expect(status.semantic_index).not.toHaveProperty("api_key_env");
  });
});

describe("formatStatus* output", () => {
  test("formats backend and model without leaking api key", () => {
    const status = coerceAftStatus(baseResponse as unknown as Record<string, unknown>);
    const dialog = formatStatusDialogMessage(status);
    const markdown = formatStatusMarkdown(status);

    expect(dialog).toContain("backend: openai_compatible");
    expect(dialog).toContain("model: text-embedding-3-small");
    expect(markdown).toContain("**Backend:** openai_compatible");
    expect(markdown).toContain("**Model:** text-embedding-3-small");
    expect(markdown).toContain("## AFT Status");
    expect(markdown).toContain("**Go overlay backend:** `aft_go_sidecar`");
    expect(markdown).toContain("**Go overlay state:** `ready`");
    expect(dialog).toContain("Go overlay backend: aft_go_sidecar");
    expect(dialog).toContain("Go overlay state: ready");
    expect(dialog).not.toContain("AFT_SEMANTIC_KEY");
    expect(markdown).not.toContain("AFT_SEMANTIC_KEY");
  });

  test("supports go_overlay metadata in status formatting", () => {
    const status = coerceAftStatus(baseResponse as unknown as Record<string, unknown>);

    expect(status.go_overlay.backend).toBe("aft_go_sidecar");
    expect(status.go_overlay.state).toBe("ready");
    expect(status.go_overlay.schema_version).toBe(2);

    const markdown = formatStatusMarkdown(status);
    expect(markdown).toContain("### Go overlay metadata");
    expect(markdown).toContain("**Provider id:** go-sidecar@sha256");
    expect(markdown).toContain("**Schema version:** 2");
  });
});
