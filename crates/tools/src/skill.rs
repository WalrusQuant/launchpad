use std::{
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("skill.txt");

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "name": {"type": "string"} },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let name = input["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'name' field"))?;
        let found = find_skill(&ctx.cwd, name)
            .ok_or_else(|| anyhow::anyhow!("Skill \"{name}\" not found"))?;
        let content = fs::read_to_string(&found)?;
        let dir = found.parent().unwrap_or(Path::new("")).to_path_buf();
        let files = sample_files(&dir);
        Ok(ToolOutput::success(format!(
            "<skill_content name=\"{name}\">\n# Skill: {name}\n\n{content}\n\nBase directory for this skill: {}\nRelative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\nNote: file list is sampled.\n\n<skill_files>\n{}\n</skill_files>\n</skill_content>",
            dir.display(),
            files.join("\n")
        )))
    }
}

fn find_skill(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|x| x.to_str()) == Some("SKILL.md")
                    && path.parent()?.file_name().and_then(|x| x.to_str()) == Some(name)
                {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn sample_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|x| x.to_str()) == Some("SKILL.md") {
                continue;
            }
            files.push(format!("<file>{}</file>", path.display()));
            if files.len() >= 10 {
                break;
            }
        }
    }
    files
}
