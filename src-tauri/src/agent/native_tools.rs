//! Launchpad-native agent tools — `Box<dyn Tool>` entries that run in-process
//! and reach back into the UI via Tauri events. Each tool validates its input,
//! emits a single `launchpad:agent-tool-action` event with `{ tool, payload }`,
//! and returns a short confirmation string. The frontend listens for that
//! event and routes to the right surface (`createEditorTab`, `createDiffTab`,
//! etc.) — keeping all the UI logic on the JS side.
//!
//! Tools are registered alongside the lpa builtins in `host::build_runtime`.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

use lpa_tools::{Tool, ToolContext, ToolOutput, ToolRegistry};

const ACTION_EVENT: &str = "launchpad:agent-tool-action";

/// Envelope sent on every native-tool fire so the frontend can switch on
/// `tool` and trust the shape of `payload`.
#[derive(Serialize, Clone)]
struct ToolAction<'a> {
    tool: &'a str,
    payload: Value,
}

fn emit(app: &AppHandle, tool: &str, payload: Value) {
    let _ = app.emit(ACTION_EVENT, ToolAction { tool, payload });
}

fn resolve_path(ctx: &ToolContext, raw: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(raw);
    if p.is_absolute() { p } else { ctx.cwd.join(p) }
}

// ─── lp_open_in_editor ───────────────────────────────────────────────────

pub struct OpenInEditorTool {
    app: AppHandle,
}

#[async_trait]
impl Tool for OpenInEditorTool {
    fn name(&self) -> &str {
        "lp_open_in_editor"
    }

    fn description(&self) -> &str {
        "Open a file in a Launchpad editor tab. Optionally jump to a 1-indexed \
         line (and column). Use after Read / Grep / Lsp results to surface a \
         file to the user."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file (absolute or relative to cwd)." },
                "line": { "type": "integer", "description": "1-indexed line number." },
                "column": { "type": "integer", "description": "1-indexed column number." }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: &ToolContext, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing 'path' field"))?;
        let abs = resolve_path(ctx, path);
        if !abs.exists() {
            return Ok(ToolOutput::error(format!(
                "file does not exist: {}",
                abs.display()
            )));
        }
        let line = input.get("line").and_then(Value::as_u64);
        let column = input.get("column").and_then(Value::as_u64);

        emit(
            &self.app,
            "lp_open_in_editor",
            json!({
                "path": abs.to_string_lossy(),
                "line": line,
                "column": column,
            }),
        );
        let where_clause = match (line, column) {
            (Some(l), Some(c)) => format!(" (line {}, col {})", l, c),
            (Some(l), None) => format!(" (line {})", l),
            _ => String::new(),
        };
        Ok(ToolOutput::success(format!(
            "opened {} in editor{}",
            abs.display(),
            where_clause
        )))
    }

    fn is_read_only(&self) -> bool {
        // No filesystem mutation — opens a UI tab. Marking read-only lets the
        // orchestrator skip the approval round-trip.
        true
    }
}

// ─── lp_show_diff ────────────────────────────────────────────────────────

pub struct ShowDiffTool {
    app: AppHandle,
}

#[async_trait]
impl Tool for ShowDiffTool {
    fn name(&self) -> &str {
        "lp_show_diff"
    }

    fn description(&self) -> &str {
        "Open a Launchpad diff tab comparing two git refs in the active project. \
         Refs may be branch names, commit OIDs (4-40 hex), or rev expressions \
         (e.g. \"HEAD\", \"HEAD^\", \"main~2\")."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from_ref": { "type": "string", "description": "Base ref." },
                "to_ref": { "type": "string", "description": "Compare ref. Defaults to HEAD." }
            },
            "required": ["from_ref"]
        })
    }

    async fn execute(&self, _ctx: &ToolContext, input: Value) -> anyhow::Result<ToolOutput> {
        let from_ref = input
            .get("from_ref")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing 'from_ref' field"))?;
        let to_ref = input
            .get("to_ref")
            .and_then(Value::as_str)
            .unwrap_or("HEAD");

        // Defense-in-depth: agent-supplied refs go straight to the
        // frontend's diff-tab helper which calls get_diff_between_refs;
        // that command validates too, but rejecting nonsense here gives
        // the model a useful error instead of a silent no-op.
        if !crate::is_valid_git_ref(from_ref) {
            return Ok(ToolOutput::error(format!(
                "invalid from_ref: {} (must match [A-Za-z0-9._/-^~], no leading '-', no '..')",
                from_ref
            )));
        }
        if !crate::is_valid_git_ref(to_ref) {
            return Ok(ToolOutput::error(format!(
                "invalid to_ref: {} (must match [A-Za-z0-9._/-^~], no leading '-', no '..')",
                to_ref
            )));
        }

        emit(
            &self.app,
            "lp_show_diff",
            json!({ "from_ref": from_ref, "to_ref": to_ref }),
        );
        Ok(ToolOutput::success(format!(
            "opened diff tab: {} → {}",
            from_ref, to_ref
        )))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ─── lp_open_merge_tab ───────────────────────────────────────────────────

pub struct OpenMergeTabTool {
    app: AppHandle,
}

#[async_trait]
impl Tool for OpenMergeTabTool {
    fn name(&self) -> &str {
        "lp_open_merge_tab"
    }

    fn description(&self) -> &str {
        "Open Launchpad's 3-pane merge tab (ours / merged / theirs) for a file \
         currently in conflict. Useful when a tool produced merge conflicts the \
         user needs to resolve interactively."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to the conflicted file (absolute or relative to cwd)." }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, ctx: &ToolContext, input: Value) -> anyhow::Result<ToolOutput> {
        let file_path = input
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing 'file_path' field"))?;
        let abs = resolve_path(ctx, file_path);
        if !abs.exists() {
            return Ok(ToolOutput::error(format!(
                "file does not exist: {}",
                abs.display()
            )));
        }
        emit(
            &self.app,
            "lp_open_merge_tab",
            json!({ "file_path": abs.to_string_lossy() }),
        );
        Ok(ToolOutput::success(format!(
            "opened merge tab for {}",
            abs.display()
        )))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ─── lp_refresh_git_panel ────────────────────────────────────────────────

pub struct RefreshGitPanelTool {
    app: AppHandle,
}

#[async_trait]
impl Tool for RefreshGitPanelTool {
    fn name(&self) -> &str {
        "lp_refresh_git_panel"
    }

    fn description(&self) -> &str {
        "Force the git panel to refresh now (otherwise polled every 3s). Useful \
         right after running git commands via Bash so the user sees the new \
         state without delay."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _ctx: &ToolContext, _input: Value) -> anyhow::Result<ToolOutput> {
        emit(&self.app, "lp_refresh_git_panel", json!({}));
        Ok(ToolOutput::success("git panel refresh triggered"))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ─── lp_reveal_in_finder ─────────────────────────────────────────────────

pub struct RevealInFinderTool {
    app: AppHandle,
}

#[async_trait]
impl Tool for RevealInFinderTool {
    fn name(&self) -> &str {
        "lp_reveal_in_finder"
    }

    fn description(&self) -> &str {
        "Reveal a file or folder in macOS Finder. Useful for handing off a \
         result to the user outside the agent surface."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to reveal (absolute or relative to cwd)." }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: &ToolContext, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing 'path' field"))?;
        let abs = resolve_path(ctx, path);
        emit(
            &self.app,
            "lp_reveal_in_finder",
            json!({ "path": abs.to_string_lossy() }),
        );
        Ok(ToolOutput::success(format!(
            "revealed {} in Finder",
            abs.display()
        )))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

// ─── registration helper ─────────────────────────────────────────────────

/// Register every Launchpad-native tool into the shared `ToolRegistry`.
pub fn register_native_tools(registry: &mut ToolRegistry, app: AppHandle) {
    registry.register(Arc::new(OpenInEditorTool { app: app.clone() }));
    registry.register(Arc::new(ShowDiffTool { app: app.clone() }));
    registry.register(Arc::new(OpenMergeTabTool { app: app.clone() }));
    registry.register(Arc::new(RefreshGitPanelTool { app: app.clone() }));
    registry.register(Arc::new(RevealInFinderTool { app }));
}
