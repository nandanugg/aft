//! Bridge to the optional `aft-go-helper` binary.
//!
//! AFT's tree-sitter parser handles syntax across all supported languages,
//! but Go programs need type information to resolve interface dispatch and
//! method calls correctly. The companion Go helper (`go-helper/`) uses the
//! standard toolchain's SSA + class-hierarchy analysis to produce a list of
//! resolved call edges, which AFT merges into its reverse index for Go
//! files only.
//!
//! This module owns the deserialization side of the contract. The schema
//! mirrors `go-helper/main.go` exactly — keep them in sync. A `version`
//! field is included so future schema changes can be detected and old
//! cached outputs ignored without crashing.
//!
//! When the helper is unavailable (no `go` on PATH, helper binary missing,
//! helper exits non-zero), the rest of AFT must continue to work — the
//! integration is strictly additive.
//
// Schema version. Bump when the on-disk JSON format changes in a way old
// readers cannot tolerate. Cached outputs with a different version are
// discarded rather than parsed.
pub const HELPER_SCHEMA_VERSION: u32 = 1;

use serde::{Deserialize, Serialize};

/// Top-level document returned by `aft-go-helper -root <dir>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperOutput {
    /// Schema version (see `HELPER_SCHEMA_VERSION`).
    pub version: u32,
    /// Absolute project root the helper was invoked against.
    pub root: String,
    /// Resolved call edges. Empty if the project has no in-project edges
    /// (e.g. a single file with only stdlib calls).
    #[serde(default)]
    pub edges: Vec<HelperEdge>,
    /// Packages skipped due to load errors. Reported for diagnostics; AFT
    /// falls back to tree-sitter for these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// A single resolved call edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperEdge {
    /// Where the call site is (file + line + enclosing symbol).
    pub caller: HelperCaller,
    /// What the call resolves to.
    pub callee: HelperCallee,
    /// Classification of the edge. See `EdgeKind`.
    pub kind: EdgeKind,
}

/// Caller-side position for an edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperCaller {
    /// File path relative to the helper's `root`.
    pub file: String,
    /// 1-based line number of the call expression.
    pub line: u32,
    /// Enclosing top-level function/method name. Closures collapse to
    /// their containing named function so AFT can find the symbol via
    /// tree-sitter.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub symbol: String,
}

/// Callee-side description of a resolved target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperCallee {
    /// File path relative to the helper's `root`.
    pub file: String,
    /// Function or method name (without receiver).
    pub symbol: String,
    /// Receiver type as Go renders it, e.g. `"*example.com/pkg.T"`.
    /// Empty for non-methods.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub receiver: String,
    /// Full package import path, e.g. `"example.com/pkg"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pkg: String,
}

/// What sort of call this edge represents. Drives AFT's display of the
/// caller (e.g. "interface" sites get a marker so users know multiple
/// concrete callees are possible).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// Package-level function call: `pkg.Foo()` or bare `Foo()`.
    Static,
    /// Method on a concrete type: `(&T{}).Method()`.
    Concrete,
    /// Interface dispatch resolved by class-hierarchy analysis. One
    /// `HelperEdge` is emitted per concrete implementation.
    Interface,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sample matches the actual helper output for the fixture used during
    // development — keeps the deserializer locked to the wire format.
    const SAMPLE_OUTPUT: &str = r#"{
      "version": 1,
      "root": "/tmp/go-fixture",
      "edges": [
        {
          "caller": {"file": "go_resolution.go", "line": 42, "symbol": "interfaceCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "Do", "receiver": "*example.com/fixture.doerA", "pkg": "example.com/fixture"},
          "kind": "interface"
        },
        {
          "caller": {"file": "go_resolution.go", "line": 24, "symbol": "concreteMethodCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "concreteMethod", "receiver": "*example.com/fixture.concreteSvc", "pkg": "example.com/fixture"},
          "kind": "concrete"
        },
        {
          "caller": {"file": "go_resolution.go", "line": 10, "symbol": "barePkgCaller"},
          "callee": {"file": "go_resolution.go", "symbol": "barePkgTarget", "pkg": "example.com/fixture"},
          "kind": "static"
        }
      ]
    }"#;

    #[test]
    fn deserializes_sample_output() {
        let out: HelperOutput = serde_json::from_str(SAMPLE_OUTPUT).unwrap();
        assert_eq!(out.version, HELPER_SCHEMA_VERSION);
        assert_eq!(out.root, "/tmp/go-fixture");
        assert_eq!(out.edges.len(), 3);
        assert!(out.skipped.is_empty());

        let iface = &out.edges[0];
        assert_eq!(iface.kind, EdgeKind::Interface);
        assert_eq!(iface.caller.symbol, "interfaceCaller");
        assert_eq!(iface.callee.symbol, "Do");
        assert_eq!(iface.callee.receiver, "*example.com/fixture.doerA");

        let stat = &out.edges[2];
        assert_eq!(stat.kind, EdgeKind::Static);
        assert_eq!(stat.callee.receiver, "");
    }

    #[test]
    fn missing_optional_fields_default_to_empty() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1},
                    "callee": {"file": "b.go", "symbol": "F"},
                    "kind": "static"
                }
            ]
        }"#;
        let out: HelperOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.edges[0].caller.symbol, "");
        assert_eq!(out.edges[0].callee.pkg, "");
    }

    #[test]
    fn round_trips_through_serde() {
        let out: HelperOutput = serde_json::from_str(SAMPLE_OUTPUT).unwrap();
        let s = serde_json::to_string(&out).unwrap();
        let again: HelperOutput = serde_json::from_str(&s).unwrap();
        assert_eq!(out, again);
    }

    #[test]
    fn unknown_edge_kind_is_rejected() {
        let json = r#"{
            "version": 1,
            "root": "/x",
            "edges": [
                {
                    "caller": {"file": "a.go", "line": 1, "symbol": "f"},
                    "callee": {"file": "b.go", "symbol": "g"},
                    "kind": "telepathy"
                }
            ]
        }"#;
        let err = serde_json::from_str::<HelperOutput>(json).unwrap_err();
        assert!(err.to_string().contains("telepathy") || err.to_string().contains("variant"));
    }
}
