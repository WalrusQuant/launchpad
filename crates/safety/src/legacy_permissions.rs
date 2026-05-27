use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::SandboxPolicyRecord;

/// The legacy permission mode controlling how the current runtime handles permission checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// Approve every request without asking.
    AutoApprove,
    /// Ask the user for confirmation on each request.
    Interactive,
    /// Deny all requests that require permission.
    Deny,
}

impl PermissionMode {
    /// Parse a user-facing string ("auto-approve", "interactive", "deny") into a
    /// mode. Returns `None` for unknown values so callers can fall back to a
    /// safe default.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto-approve" | "auto_approve" | "autoapprove" => Some(Self::AutoApprove),
            "interactive" | "ask" => Some(Self::Interactive),
            "deny" | "denyall" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// Per-session sandbox context attached to a `RuleBasedPolicy`. Carries the
/// declared sandbox record plus the session's canonical cwd so the policy can
/// scope FileWrite + ShellExec decisions to the workspace.
#[derive(Debug, Clone)]
pub struct SandboxContext {
    pub policy: SandboxPolicyRecord,
    pub cwd: PathBuf,
}

/// The legacy resource kind used by the current tool runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    /// A file-read request.
    FileRead,
    /// A file-write request.
    FileWrite,
    /// A shell-execution request.
    ShellExec,
    /// A network-access request.
    Network,
    /// A tool-specific custom resource kind.
    Custom(String),
}

/// The legacy permission request emitted by the current tool system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The originating tool name.
    pub tool_name: String,
    /// The kind of resource being accessed.
    pub resource: ResourceKind,
    /// The free-form human-readable description of the action.
    pub description: String,
    /// The optional target path, host, or command string.
    pub target: Option<String>,
}

/// The legacy result of one permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Allow the request immediately.
    Allow,
    /// Deny the request with a reason.
    Deny {
        /// The human-readable denial reason.
        reason: String,
    },
    /// Ask the user to approve the request.
    Ask {
        /// The human-readable approval prompt.
        message: String,
    },
}

/// The legacy pluggable permission-policy trait used by the current runtime.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Returns the legacy permission decision for one request.
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;
}

/// One legacy rule-based permission entry persisted in configuration or tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// The resource kind matched by the rule.
    pub resource: ResourceKind,
    /// The glob-like pattern matched against the target.
    pub pattern: String,
    /// Whether the rule allows or denies matching requests.
    pub allow: bool,
}

/// The legacy rule-based permission policy used by the current query loop and tools.
pub struct RuleBasedPolicy {
    /// The fallback permission mode used when no explicit rule matches.
    pub mode: PermissionMode,
    /// The explicit resource rules evaluated before the fallback mode.
    pub rules: Vec<PermissionRule>,
    /// Optional sandbox context that gates FileWrite + ShellExec independently
    /// of `mode`. Sandbox denials short-circuit both `Allow` and `Ask`.
    pub sandbox: Option<SandboxContext>,
}

impl RuleBasedPolicy {
    /// Creates a rule-based policy with no explicit rules.
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            rules: Vec::new(),
            sandbox: None,
        }
    }

    /// Creates a rule-based policy with an explicit rule list.
    pub fn with_rules(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self {
            mode,
            rules,
            sandbox: None,
        }
    }

    /// Creates a rule-based policy with an attached sandbox context.
    pub fn with_sandbox(mode: PermissionMode, sandbox: SandboxContext) -> Self {
        Self {
            mode,
            rules: Vec::new(),
            sandbox: Some(sandbox),
        }
    }

    fn match_rule(&self, request: &PermissionRequest) -> Option<&PermissionRule> {
        let target = request.target.as_deref().unwrap_or("");
        self.rules.iter().find(|rule| {
            rule.resource == request.resource && Self::pattern_matches(&rule.pattern, target)
        })
    }

    fn pattern_matches(pattern: &str, target: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if pattern.starts_with('*') && pattern.ends_with('*') {
            let inner = &pattern[1..pattern.len() - 1];
            return target.contains(inner);
        }
        if pattern.ends_with('*') {
            return target.starts_with(pattern.trim_end_matches('*'));
        }
        if pattern.starts_with('*') {
            return target.ends_with(pattern.trim_start_matches('*'));
        }
        if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                return target.starts_with(parts[0]) && target.ends_with(parts[1]);
            }
        }
        target == pattern
    }

    /// Evaluates the sandbox context against a request and returns a denial
    /// when the sandbox forbids the action. Returns `None` when the sandbox
    /// either doesn't apply or is permissive enough to delegate to the
    /// fallback mode / rules.
    fn evaluate_sandbox(&self, request: &PermissionRequest) -> Option<PermissionDecision> {
        let sb = self.sandbox.as_ref()?;
        use crate::SandboxMode;
        if matches!(sb.policy.mode, SandboxMode::Unrestricted) {
            return None;
        }

        let is_shell = matches!(request.resource, ResourceKind::ShellExec)
            || matches!(&request.resource, ResourceKind::Custom(name)
                if name.eq_ignore_ascii_case("bash") || name.eq_ignore_ascii_case("shell"));

        if is_shell && !sb.policy.workspace_write {
            return Some(PermissionDecision::Deny {
                reason: "sandbox forbids shell execution (read-only mode)".into(),
            });
        }

        if matches!(request.resource, ResourceKind::FileWrite) {
            if !sb.policy.workspace_write {
                return Some(PermissionDecision::Deny {
                    reason: "sandbox forbids file writes (read-only mode)".into(),
                });
            }
            if let Some(target) = request.target.as_deref() {
                let cwd_canon =
                    std::fs::canonicalize(&sb.cwd).unwrap_or_else(|_| sb.cwd.clone());
                let target_canon = canonicalize_or_walk_ancestors(
                    std::path::Path::new(target),
                    &cwd_canon,
                );
                if !target_canon.starts_with(&cwd_canon) {
                    return Some(PermissionDecision::Deny {
                        reason: format!(
                            "sandbox forbids writes outside workspace: {}",
                            target_canon.display()
                        ),
                    });
                }
            }
        }

        None
    }
}

/// Resolves a target path to its canonical form even when the file (and one
/// or more parent directories) doesn't yet exist on disk. Common case: the
/// agent writes `<cwd>/new-dir/new-file.txt` where `new-dir` doesn't exist
/// yet. We walk up the ancestors until `canonicalize` succeeds, then rejoin
/// the missing tail. For relative paths with no canonicalizable ancestor,
/// resolve against `cwd_canon` first. This is what makes the workspace-write
/// sandbox safe for normal agent file creation.
fn canonicalize_or_walk_ancestors(
    target: &std::path::Path,
    cwd_canon: &std::path::Path,
) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(target) {
        return canon;
    }

    // Resolve relative paths against cwd before walking — otherwise an input
    // like "foo/bar.txt" would have no canonicalizable ancestor at all.
    let absolute: PathBuf = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd_canon.join(target)
    };

    if let Ok(canon) = std::fs::canonicalize(&absolute) {
        return canon;
    }

    // Walk up: find the deepest existing ancestor that canonicalizes, then
    // rejoin the tail (the non-existent components) onto it. This is the
    // same shape `Path::canonicalize` would produce once the file exists.
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = absolute.as_path();
    loop {
        match current.parent() {
            Some(parent) => {
                if let Some(name) = current.file_name() {
                    tail.push(name);
                }
                if let Ok(parent_canon) = std::fs::canonicalize(parent) {
                    let mut resolved = parent_canon;
                    for component in tail.iter().rev() {
                        resolved.push(component);
                    }
                    return resolved;
                }
                current = parent;
            }
            None => {
                // Reached root with nothing canonicalizable — return the
                // resolved-but-not-canonical absolute path so the caller's
                // starts_with check still works against cwd_canon when the
                // target lives under it on a non-symlinked path.
                return absolute;
            }
        }
    }
}

#[async_trait]
impl PermissionPolicy for RuleBasedPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        // Sandbox denials win over rules + mode — a restricted sandbox cannot
        // be approved away with AutoApprove or an Ask round-trip.
        if let Some(decision) = self.evaluate_sandbox(request) {
            return decision;
        }

        if let Some(rule) = self.match_rule(request) {
            return if rule.allow {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny {
                    reason: format!("blocked by rule: {}", rule.pattern),
                }
            };
        }

        match self.mode {
            PermissionMode::AutoApprove => PermissionDecision::Allow,
            PermissionMode::Deny => PermissionDecision::Deny {
                reason: "permission mode is Deny".into(),
            },
            PermissionMode::Interactive => PermissionDecision::Ask {
                message: format!(
                    "{} wants to access {:?}: {}",
                    request.tool_name, request.resource, request.description
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PermissionDecision, PermissionMode, PermissionPolicy, PermissionRequest, PermissionRule,
        ResourceKind, RuleBasedPolicy,
    };

    fn file_write_request(target: Option<&str>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "file_write".into(),
            resource: ResourceKind::FileWrite,
            description: "write a file".into(),
            target: target.map(|value| value.into()),
        }
    }

    #[test]
    fn permission_mode_serde_roundtrip() {
        for mode in [
            PermissionMode::AutoApprove,
            PermissionMode::Interactive,
            PermissionMode::Deny,
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let restored: PermissionMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(restored, mode);
        }
    }

    #[test]
    fn pattern_matches_prefix_and_exact() {
        assert!(RuleBasedPolicy::pattern_matches("/tmp/*", "/tmp/file.txt"));
        assert!(RuleBasedPolicy::pattern_matches(
            "/etc/passwd",
            "/etc/passwd"
        ));
        assert!(!RuleBasedPolicy::pattern_matches(
            "/tmp/*",
            "/var/tmp/file.txt"
        ));
    }

    #[test]
    fn pattern_matches_suffix() {
        assert!(RuleBasedPolicy::pattern_matches("*.pem", "server.pem"));
        assert!(RuleBasedPolicy::pattern_matches("*.pem", "/etc/ssl/cert.pem"));
        assert!(!RuleBasedPolicy::pattern_matches("*.pem", "file.txt"));
    }

    #[test]
    fn pattern_matches_contains() {
        assert!(RuleBasedPolicy::pattern_matches("*.git/*", "/repo/.git/config"));
        assert!(!RuleBasedPolicy::pattern_matches("*.git/*", "/repo/legit.gitignore"));
    }

    #[test]
    fn pattern_matches_prefix_suffix() {
        assert!(RuleBasedPolicy::pattern_matches("npm run *", "npm run build"));
        assert!(RuleBasedPolicy::pattern_matches("npm run *", "npm run test:watch"));
        assert!(!RuleBasedPolicy::pattern_matches("npm run *", "yarn test"));
    }

    #[tokio::test]
    async fn explicit_allow_rule_overrides_deny_mode() {
        let policy = RuleBasedPolicy::with_rules(
            PermissionMode::Deny,
            vec![PermissionRule {
                resource: ResourceKind::FileWrite,
                pattern: "/tmp/*".into(),
                allow: true,
            }],
        );

        assert!(matches!(
            policy.check(&file_write_request(Some("/tmp/file"))).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn interactive_mode_asks() {
        let policy = RuleBasedPolicy::new(PermissionMode::Interactive);
        assert!(matches!(
            policy.check(&file_write_request(Some("/tmp/file"))).await,
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn permission_mode_parse_accepts_aliases() {
        assert_eq!(
            PermissionMode::parse("auto-approve"),
            Some(PermissionMode::AutoApprove)
        );
        assert_eq!(
            PermissionMode::parse("auto_approve"),
            Some(PermissionMode::AutoApprove)
        );
        assert_eq!(
            PermissionMode::parse("Interactive"),
            Some(PermissionMode::Interactive)
        );
        assert_eq!(
            PermissionMode::parse("ask"),
            Some(PermissionMode::Interactive)
        );
        assert_eq!(PermissionMode::parse("deny"), Some(PermissionMode::Deny));
        assert_eq!(PermissionMode::parse("nonsense"), None);
    }

    #[tokio::test]
    async fn sandbox_read_only_denies_file_write_under_autoapprove() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: false,
                },
                cwd: std::env::temp_dir(),
            },
        );
        let decision = policy
            .check(&file_write_request(Some("/tmp/whatever")))
            .await;
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn sandbox_workspace_write_denies_paths_outside_cwd() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        let tmp = std::env::temp_dir();
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: true,
                },
                cwd: tmp.clone(),
            },
        );
        // A path well outside the workspace (use the user's home root, which
        // certainly doesn't live under the OS tmp dir).
        let outside = "/etc/hosts";
        let decision = policy.check(&file_write_request(Some(outside))).await;
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn sandbox_unrestricted_does_not_deny() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Unrestricted,
                    workspace_write: true,
                },
                cwd: std::env::temp_dir(),
            },
        );
        let decision = policy.check(&file_write_request(Some("/etc/hosts"))).await;
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn sandbox_workspace_write_allows_new_file_in_new_dir() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        // Real-world scenario from dogfooding: agent wants to create
        // `<cwd>/new-subdir/file.txt` where `new-subdir` doesn't exist yet.
        // canonicalize fails on both target and its parent; the old fallback
        // returned a non-canonical path that didn't starts_with(cwd_canon),
        // denying valid writes.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let cwd = tmp.path().to_path_buf();
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: true,
                },
                cwd: cwd.clone(),
            },
        );
        let new_file_path = cwd.join("new-subdir").join("hello.txt");
        let request = PermissionRequest {
            tool_name: "write".into(),
            resource: ResourceKind::FileWrite,
            description: "write a new file in a new dir".into(),
            target: Some(new_file_path.to_string_lossy().into_owned()),
        };
        let decision = policy.check(&request).await;
        assert!(
            matches!(decision, PermissionDecision::Allow),
            "expected Allow for new file in new dir under workspace, got {decision:?}",
        );
    }

    #[tokio::test]
    async fn sandbox_workspace_write_allows_relative_path_inside_workspace() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        // Some tools emit relative target paths. Sandbox should resolve them
        // against cwd before deciding.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let cwd = tmp.path().to_path_buf();
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: true,
                },
                cwd,
            },
        );
        let request = PermissionRequest {
            tool_name: "write".into(),
            resource: ResourceKind::FileWrite,
            description: "write a relative path".into(),
            target: Some("src/main.rs".into()),
        };
        let decision = policy.check(&request).await;
        assert!(
            matches!(decision, PermissionDecision::Allow),
            "expected Allow for relative path resolved into workspace, got {decision:?}",
        );
    }

    #[tokio::test]
    async fn sandbox_read_only_denies_shell() {
        use super::SandboxContext;
        use crate::{SandboxMode, SandboxPolicyRecord};
        let policy = RuleBasedPolicy::with_sandbox(
            PermissionMode::AutoApprove,
            SandboxContext {
                policy: SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: false,
                },
                cwd: std::env::temp_dir(),
            },
        );
        let bash_request = PermissionRequest {
            tool_name: "bash".into(),
            resource: ResourceKind::Custom("bash".into()),
            description: "run a shell command".into(),
            target: Some("ls".into()),
        };
        let decision = policy.check(&bash_request).await;
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }
}
