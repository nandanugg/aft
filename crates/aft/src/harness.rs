use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Harness {
    Opencode,
    Pi,
    Runner,
    Mcp { client: String },
}

impl Harness {
    pub fn storage_segment(&self) -> String {
        match self {
            Harness::Opencode => "opencode".to_string(),
            Harness::Pi => "pi".to_string(),
            Harness::Runner => "runner".to_string(),
            Harness::Mcp { client } => format!("mcp--{}", sanitize_client(client)),
        }
    }

    pub fn wire_label(&self) -> String {
        match self {
            Harness::Opencode => "opencode".to_string(),
            Harness::Pi => "pi".to_string(),
            Harness::Runner => "runner".to_string(),
            Harness::Mcp { client } => format!("mcp:{client}"),
        }
    }
}

/// Max length of the readable (pre-hash) slug portion. The full segment is
/// `mcp--<readable>--<32 hex>`, so the readable part is capped to keep directory
/// names bounded while the hash guarantees uniqueness.
const MCP_SLUG_READABLE_MAX: usize = 40;
const MCP_SLUG_HASH_HEX_LEN: usize = 32;

/// Build the storage slug for an MCP client. The readable portion is a
/// sanitized, length-capped rendering of the raw client; a short hash of the
/// RAW (un-sanitized) client is appended so that distinct clients that sanitize
/// to the same readable string (e.g. `a/b`, `a:b`, `a b`, casing variants, or
/// non-ASCII that collapses to `unknown`) still get distinct directories. The
/// hash is over the raw bytes, so it is collision-resistant where the readable
/// slug is not.
fn sanitize_client(client: &str) -> String {
    let lower = client.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_dash = false;
    for ch in lower.chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if keep {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    let mut readable = if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    };
    if readable.len() > MCP_SLUG_READABLE_MAX {
        readable.truncate(MCP_SLUG_READABLE_MAX);
        // Truncation can leave a trailing separator; trim it for tidiness.
        readable = readable.trim_end_matches(['-', '.']).to_string();
        if readable.is_empty() {
            readable = "unknown".to_string();
        }
    }

    // A 128-bit hash suffix prevents hostile same-readable slugs from sharing
    // storage while keeping directory names short enough for common filesystems.
    let hash = blake3::hash(client.as_bytes()).to_hex();
    format!("{readable}--{}", &hash.as_str()[..MCP_SLUG_HASH_HEX_LEN])
}

impl Serialize for Harness {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.wire_label())
    }
}

impl<'de> Deserialize<'de> for Harness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HarnessVisitor;

        impl<'de> Visitor<'de> for HarnessVisitor {
            type Value = Harness;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a harness string: 'opencode', 'pi', 'runner', or 'mcp:<client>'")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Harness::from_str(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(HarnessVisitor)
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.wire_label())
    }
}

impl std::str::FromStr for Harness {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "opencode" => Ok(Harness::Opencode),
            "pi" => Ok(Harness::Pi),
            "runner" => Ok(Harness::Runner),
            other if other.starts_with("mcp:") => {
                let client = &other[4..];
                if client.is_empty() {
                    Err(
                        "unsupported harness 'mcp:'; mcp client name must be non-empty".to_string(),
                    )
                } else {
                    Ok(Harness::Mcp {
                        client: client.to_string(),
                    })
                }
            }
            other => Err(format!(
                "unsupported harness '{other}'; expected 'opencode', 'pi', 'runner', or 'mcp:<client>'"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize_client, Harness};
    use std::str::FromStr;

    #[test]
    fn harness_enum_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&Harness::Opencode).unwrap(),
            "\"opencode\""
        );
        assert_eq!(serde_json::to_string(&Harness::Pi).unwrap(), "\"pi\"");

        assert_eq!(
            serde_json::from_str::<Harness>("\"opencode\"").unwrap(),
            Harness::Opencode
        );
        assert_eq!(
            serde_json::from_str::<Harness>("\"pi\"").unwrap(),
            Harness::Pi
        );
        assert!(serde_json::from_str::<Harness>("\"claude_code\"").is_err());
    }

    #[test]
    fn opencode_pi_storage_segment_unchanged() {
        assert_eq!(Harness::Opencode.storage_segment(), "opencode");
        assert_eq!(Harness::Pi.storage_segment(), "pi");
    }

    #[test]
    fn runner_round_trips() {
        assert_eq!(Harness::from_str("runner").unwrap(), Harness::Runner);
        assert_eq!(Harness::Runner.storage_segment(), "runner");
        assert_eq!(
            serde_json::to_string(&Harness::Runner).unwrap(),
            "\"runner\""
        );
        assert_eq!(
            serde_json::from_str::<Harness>("\"runner\"").unwrap(),
            Harness::Runner
        );
    }

    #[test]
    fn mcp_round_trips() {
        let h = Harness::Mcp {
            client: "claude-code".to_string(),
        };
        assert_eq!(serde_json::to_string(&h).unwrap(), "\"mcp:claude-code\"");
        assert_eq!(
            serde_json::from_str::<Harness>("\"mcp:claude-code\"").unwrap(),
            h
        );
        assert_eq!(
            Harness::from_str("mcp:cursor").unwrap(),
            Harness::Mcp {
                client: "cursor".to_string(),
            }
        );
        assert!(Harness::from_str("mcp:").is_err());
    }

    #[test]
    fn storage_segment_hostile_clients_are_path_safe() {
        let cases = ["../../etc", "a/b", r"a\b", "a:b", "", "Claude.Code"];
        for client in cases {
            let seg = Harness::Mcp {
                client: client.to_string(),
            }
            .storage_segment();
            assert!(
                !seg.is_empty(),
                "segment must be non-empty for client {client:?}"
            );
            assert!(
                !seg.contains(['/', '\\', ':']),
                "segment {seg:?} must not contain path separators for client {client:?}"
            );
            assert!(
                !seg.contains(".."),
                "segment {seg:?} must not contain '..' for client {client:?}"
            );
            assert!(
                seg.starts_with("mcp--"),
                "segment {seg:?} must use mcp-- prefix"
            );
        }
        // Readable portion preserved, hash suffix appended.
        let claude = Harness::Mcp {
            client: "Claude.Code".to_string(),
        }
        .storage_segment();
        assert!(
            claude.starts_with("mcp--claude.code--"),
            "expected readable slug with hash suffix, got {claude:?}"
        );
        // Empty client → readable "unknown" plus a (stable) hash of empty bytes.
        let empty = sanitize_client("");
        assert!(
            empty.starts_with("unknown--"),
            "empty client must render unknown-- plus hash, got {empty:?}"
        );
    }

    #[test]
    fn storage_segment_disambiguates_clients_that_sanitize_to_same_slug() {
        // a/b, a:b, a b, A-B all collapse to the readable slug "a-b" but are
        // DISTINCT clients — the raw-bytes hash suffix must keep their storage
        // directories distinct so two different MCP clients never share state.
        let seg = |c: &str| {
            Harness::Mcp {
                client: c.to_string(),
            }
            .storage_segment()
        };
        let variants = [seg("a/b"), seg("a:b"), seg("a b"), seg("A-B")];
        for s in &variants {
            assert!(
                s.starts_with("mcp--a-b--"),
                "expected shared readable slug a-b, got {s:?}"
            );
            let (_readable, suffix) = s.rsplit_once("--").expect("hash suffix");
            assert_eq!(
                suffix.len(),
                super::MCP_SLUG_HASH_HEX_LEN,
                "hash suffix must carry 128 bits of disambiguation: {s:?}"
            );
            assert!(
                suffix.chars().all(|ch| ch.is_ascii_hexdigit()),
                "hash suffix must be hex: {s:?}"
            );
        }
        let unique: std::collections::HashSet<_> = variants.iter().collect();
        assert_eq!(
            unique.len(),
            variants.len(),
            "distinct clients must get distinct storage segments: {variants:?}"
        );

        // Same raw client → same segment (deterministic, stable across calls).
        assert_eq!(seg("cursor"), seg("cursor"));

        // Very long client: readable portion is capped, segment stays bounded.
        let long = seg(&"x".repeat(500));
        assert!(
            long.len()
                <= "mcp--".len()
                    + super::MCP_SLUG_READABLE_MAX
                    + "--".len()
                    + super::MCP_SLUG_HASH_HEX_LEN,
            "long client segment must be length-bounded, got len {}",
            long.len()
        );
    }
}
