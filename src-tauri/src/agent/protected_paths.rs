//! Hardcoded deny rules for sensitive paths. Even in auto-approve mode,
//! writes to these paths always require explicit approval.

use lpa_safety::legacy_permissions::{PermissionRule, ResourceKind};

const PROTECTED_DIR_PREFIXES: &[&str] = &[
    ".git/",
    ".ssh/",
    ".vscode/",
    ".idea/",
    ".husky/",
    ".claude/",
];

const PROTECTED_FILE_NAMES: &[&str] = &[
    ".env",
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".mcp.json",
    "credentials.json",
];

const PROTECTED_EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx"];

const PROTECTED_ENV_PREFIXES: &[&str] = &[".env.", ".env.local"];

/// Generates deny rules for file writes targeting sensitive paths. These
/// rules should be prepended to user rules so they take priority (the
/// rule matcher returns the first match).
pub fn protected_write_rules() -> Vec<PermissionRule> {
    let mut rules = Vec::new();

    for prefix in PROTECTED_DIR_PREFIXES {
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("write".into()),
            pattern: format!("*{prefix}*"),
            allow: false,
        });
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("apply_patch".into()),
            pattern: format!("*{prefix}*"),
            allow: false,
        });
    }

    for name in PROTECTED_FILE_NAMES {
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("write".into()),
            pattern: format!("*/{name}"),
            allow: false,
        });
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("apply_patch".into()),
            pattern: format!("*/{name}"),
            allow: false,
        });
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("write".into()),
            pattern: name.to_string(),
            allow: false,
        });
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("apply_patch".into()),
            pattern: name.to_string(),
            allow: false,
        });
    }

    for ext in PROTECTED_EXTENSIONS {
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("write".into()),
            pattern: format!("*{ext}"),
            allow: false,
        });
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("apply_patch".into()),
            pattern: format!("*{ext}"),
            allow: false,
        });
    }

    for prefix in PROTECTED_ENV_PREFIXES {
        rules.push(PermissionRule {
            resource: ResourceKind::Custom("write".into()),
            pattern: format!("*/{prefix}*"),
            allow: false,
        });
    }

    rules
}
