use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use crate::{Tool, ToolContext, ToolOutput};

use apply::apply_change;
use apply::apply_hunks;
use parse::parse_patch;
use types::PatchKind;

mod apply;
mod hunk_match;
mod parse;
mod types;

const DESCRIPTION: &str = include_str!("../apply_patch.txt");

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "patchText": {
                    "type": "string",
                    "description": "The full patch text that describes all changes to be made"
                }
            },
            "required": ["patchText"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let patch_text = input["patchText"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'patchText' field"))?;
        debug!(
            tool = self.name(),
            cwd = %ctx.cwd.display(),
            session_id = %ctx.session_id,
            input = %input,
            patch_text = patch_text,
            patch_text_len = patch_text.len(),
            "received apply_patch request"
        );
        if patch_text.trim().is_empty() {
            debug!("rejecting apply_patch request because patchText is empty");
            return Ok(ToolOutput::error("patchText is required"));
        }

        let patch = parse_patch(patch_text)?;
        debug!(change_count = patch.len(), "parsed apply_patch request");
        if patch.is_empty() {
            let normalized = patch_text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .trim()
                .to_string();
            if normalized == "*** Begin Patch\n*** End Patch" {
                debug!("rejecting apply_patch request because patch contained no changes");
                return Ok(ToolOutput::error("patch rejected: empty patch"));
            }
            debug!("rejecting apply_patch request because no hunks were found");
            return Ok(ToolOutput::error(
                "apply_patch verification failed: no hunks found",
            ));
        }

        let mut files = Vec::with_capacity(patch.len());
        let mut summary = Vec::with_capacity(patch.len());
        let mut total_diff = String::new();

        for change in &patch {
            let source_path = apply::resolve_relative(&ctx.cwd, &change.path)?;
            let target_path = change
                .move_path
                .as_deref()
                .map(|path| apply::resolve_relative(&ctx.cwd, path))
                .transpose()?;
            debug!(
                kind = %change.kind.as_str(),
                source_path = %source_path.display(),
                target_path = ?target_path.as_ref().map(|path| path.display().to_string()),
                content_len = change.content.len(),
                "prepared apply_patch change"
            );

            let old_content = match change.kind {
                PatchKind::Add => String::new(),
                _ => apply::read_file(&source_path).await?,
            };
            let new_content = match change.kind {
                PatchKind::Add => change.content.clone(),
                PatchKind::Update | PatchKind::Move => apply_hunks(&old_content, &change.hunks)?,
                PatchKind::Delete => String::new(),
            };

            let additions = new_content.lines().count();
            let deletions = old_content.lines().count();
            let relative_path = apply::relative_worktree_path(
                target_path.as_ref().unwrap_or(&source_path),
                &ctx.cwd,
            );
            let kind_name = change.kind.as_str();
            let diff = format!("--- {}\n+++ {}\n", relative_path, relative_path);

            files.push(json!({
                "filePath": source_path,
                "relativePath": relative_path,
                "type": kind_name,
                "patch": diff,
                "additions": additions,
                "deletions": deletions,
                "movePath": target_path,
            }));
            total_diff.push_str(&diff);
            total_diff.push('\n');

            summary.push(match change.kind {
                PatchKind::Add => format!(
                    "A {}",
                    apply::relative_worktree_path(&source_path, &ctx.cwd)
                ),
                PatchKind::Delete => {
                    format!(
                        "D {}",
                        apply::relative_worktree_path(&source_path, &ctx.cwd)
                    )
                }
                PatchKind::Update | PatchKind::Move => {
                    format!(
                        "M {}",
                        apply::relative_worktree_path(
                            target_path.as_ref().unwrap_or(&source_path),
                            &ctx.cwd
                        )
                    )
                }
            });
        }

        for change in &patch {
            debug!(
                kind = %change.kind.as_str(),
                path = %change.path,
                move_path = ?change.move_path,
                "applying patch change"
            );

            apply_change(&ctx.cwd, change).await?;
        }

        debug!(
            updated_files = summary.len(),
            summary = ?summary,
            "apply_patch completed successfully"
        );
        Ok(ToolOutput {
            content: format!(
                "Success. Updated the following files:\n{}",
                summary.join("\n")
            ),
            is_error: false,
            metadata: Some(json!({
                "diff": total_diff,
                "files": files,
                "diagnostics": {},
            })),
        })
    }
}

#[cfg(test)]
mod tests;
