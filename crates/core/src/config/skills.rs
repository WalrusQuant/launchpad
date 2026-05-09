use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Stores runtime configuration for skill discovery and change tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    /// Whether skill discovery is enabled at all.
    pub enabled: bool,
    /// User-level roots scanned for skills.
    pub user_roots: Vec<PathBuf>,
    /// Workspace-level roots scanned for skills.
    pub workspace_roots: Vec<PathBuf>,
    /// Whether the runtime should watch skill roots for changes.
    pub watch_for_changes: bool,
}
