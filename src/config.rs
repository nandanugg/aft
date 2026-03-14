use std::path::PathBuf;

/// Runtime configuration for the aft process.
///
/// Holds project-scoped settings and tuning knobs. Values are set at startup
/// and remain immutable for the lifetime of the process.
#[derive(Debug, Clone)]
pub struct Config {
    /// Root directory of the project being analyzed. `None` if not scoped.
    pub project_root: Option<PathBuf>,
    /// How many levels of call-graph edges to follow during validation (default: 1).
    pub validation_depth: u32,
    /// Hours before a checkpoint expires and is eligible for cleanup (default: 24).
    pub checkpoint_ttl_hours: u32,
    /// Maximum depth for recursive symbol resolution (default: 10).
    pub max_symbol_depth: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            project_root: None,
            validation_depth: 1,
            checkpoint_ttl_hours: 24,
            max_symbol_depth: 10,
        }
    }
}
