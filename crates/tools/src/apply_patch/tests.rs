use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lpa_safety::legacy_permissions::{PermissionMode, RuleBasedPolicy};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::ApplyPatchTool;
use super::apply::{apply_hunks, resolve_relative};
use super::parse::parse_patch;
use super::types::{HunkLine, PatchHunk, PatchKind};
use crate::{Tool, ToolContext};

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("lpa-apply-patch-{name}-{nanos}"));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn make_ctx(cwd: std::path::PathBuf) -> ToolContext {
    ToolContext {
        cwd,
        permissions: Arc::new(RuleBasedPolicy::new(PermissionMode::AutoApprove)),
        session_id: "test-session".into(),
    }
}

#[test]
fn parse_patchsupports_all_change_kinds() {
    let patch = parse_patch(
        "*** Begin Patch
*** Add File: add.txt
+hello
*** Update File: update.txt
@@
-old
+new
*** Delete File: delete.txt
*** Update File: from.txt
*** Move to: to.txt
@@
-before
+after
*** End Patch",
    )
    .expect("parse patch");

    assert_eq!(patch.len(), 4);
    assert_eq!(patch[0].path, "add.txt");
    assert_eq!(patch[0].kind, PatchKind::Add);
    assert_eq!(patch[0].content, "hello\n");

    assert_eq!(patch[1].path, "update.txt");
    assert_eq!(patch[1].kind, PatchKind::Update);
    assert!(patch[1].content.is_empty());
    assert_eq!(patch[1].hunks.len(), 1);
    assert_eq!(
        patch[1].hunks[0].lines,
        vec![
            HunkLine::Remove("old".to_string()),
            HunkLine::Add("new".to_string())
        ]
    );

    assert_eq!(patch[2].path, "delete.txt");
    assert_eq!(patch[2].kind, PatchKind::Delete);

    assert_eq!(patch[3].path, "from.txt");
    assert_eq!(patch[3].move_path.as_deref(), Some("to.txt"));
    assert_eq!(patch[3].kind, PatchKind::Move);
    assert!(patch[3].content.is_empty());
    assert_eq!(patch[3].hunks.len(), 1);
    assert_eq!(
        patch[3].hunks[0].lines,
        vec![
            HunkLine::Remove("before".to_string()),
            HunkLine::Add("after".to_string())
        ]
    );
}

#[test]
fn parse_patch_tolerates_git_diff_headers_before_hunk() {
    let patch = parse_patch(
        "*** Begin Patch
*** Update File: read.rs
diff --git a/read.rs b/read.rs
index 1234567..89abcde 100644
--- a/read.rs
+++ b/read.rs
@@ -10,11 +10,6 @@ use serde_json::json;
 use crate::{Tool, ToolContext, ToolOutput};
 
 const DESCRIPTION: &str = include_str!(\"read.txt\");
-const MAX_LINE_LENGTH: usize = 2000;
+const MAX_BYTES: usize = 50 * 1024;
*** End Patch",
    )
    .expect("parse patch with git diff headers");

    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0].path, "read.rs");
    assert_eq!(patch[0].kind, PatchKind::Update);
    assert_eq!(patch[0].hunks.len(), 1);
    assert_eq!(
        patch[0].hunks[0].lines,
        vec![
            HunkLine::Context("use crate::{Tool, ToolContext, ToolOutput};".to_string()),
            HunkLine::Context(String::new()),
            HunkLine::Context("const DESCRIPTION: &str = include_str!(\"read.txt\");".to_string()),
            HunkLine::Remove("const MAX_LINE_LENGTH: usize = 2000;".to_string()),
            HunkLine::Add("const MAX_BYTES: usize = 50 * 1024;".to_string()),
        ]
    );
}

#[test]
fn parse_patch_requires_end_marker() {
    let error = parse_patch(
        "*** Begin Patch
*** Update File: README.md
@@
 **If you find this project useful, please consider giving it a ⭐**
+Bye",
    )
    .expect_err("patch without end marker should fail");

    assert!(error.to_string().contains("*** End Patch"));
}

#[test]
fn parse_patch_rejects_surrounding_log_text() {
    let error = parse_patch(
        "request tool=\"apply_patch\"\ninput={...}\n*** Begin Patch
*** Update File: README.md
@@
 **If you find this project useful, please consider giving it a ⭐**
+Bye
*** End Patch",
    )
    .expect_err("surrounding log text should fail");

    assert!(error.to_string().contains("*** Begin Patch"));
}

#[test]
fn parse_patch_rejects_non_prefixed_add_file_content() {
    let error = parse_patch(
        "*** Begin Patch
*** Add File: hello.txt
hello
*** End Patch",
    )
    .expect_err("non-prefixed add content should fail");

    assert!(error.to_string().contains("must start with +"));
}

#[test]
fn apply_hunks_matches_trimmed_lines_without_rewriting_context_whitespace() {
    let old_content = "start\n  keep me  \nold\nend\n";
    let hunks = vec![PatchHunk {
        lines: vec![
            HunkLine::Context("start".to_string()),
            HunkLine::Context("keep me".to_string()),
            HunkLine::Remove("old".to_string()),
            HunkLine::Add("new".to_string()),
            HunkLine::Context("end".to_string()),
        ],
    }];

    let new_content = apply_hunks(old_content, &hunks).expect("apply hunks");

    assert_eq!(new_content, "start\n  keep me  \nnew\nend\n");
}

#[test]
fn apply_hunks_matches_lines_with_normalized_whitespace() {
    let old_content = "alpha   beta\nold value\nomega\n";
    let hunks = vec![PatchHunk {
        lines: vec![
            HunkLine::Context("alpha beta".to_string()),
            HunkLine::Remove("old value".to_string()),
            HunkLine::Add("new value".to_string()),
            HunkLine::Context("omega".to_string()),
        ],
    }];

    let new_content = apply_hunks(old_content, &hunks).expect("apply hunks");

    assert_eq!(new_content, "alpha   beta\nnew value\nomega\n");
}

#[test]
fn resolve_relative_rejects_absolute_paths() {
    let base = std::path::Path::new("C:\\workspace");

    #[cfg(windows)]
    let path = "C:\\absolute\\file.txt";
    #[cfg(unix)]
    let path = "/absolute/file.txt";

    let error = resolve_relative(base, path).expect_err("absolute path should fail");
    assert!(error.to_string().contains("NEVER ABSOLUTE"));
}

#[tokio::test]
async fn execute_applies_changes_and_returns_summary() {
    let cwd = unique_temp_dir("execute");
    std::fs::write(cwd.join("update.txt"), "old\n").expect("write update file");
    std::fs::write(cwd.join("from.txt"), "before\n").expect("write move source");
    std::fs::write(cwd.join("delete.txt"), "remove me\n").expect("write delete source");
    let ctx = make_ctx(cwd.clone());

    let output = ApplyPatchTool
        .execute(
            &ctx,
            json!({
                "patchText": "*** Begin Patch
*** Add File: add.txt
+hello
*** Update File: update.txt
@@
-old
+new
*** Delete File: delete.txt
*** Update File: from.txt
*** Move to: moved/to.txt
@@
-before
+after
*** End Patch"
            }),
        )
        .await
        .expect("execute apply_patch");

    assert!(!output.is_error);
    assert!(
        output
            .content
            .contains("Success. Updated the following files:")
    );
    assert!(output.content.contains("A add.txt"));
    assert!(output.content.contains("M update.txt"));
    assert!(output.content.contains("D delete.txt"));
    assert!(output.content.contains("M moved/to.txt"));

    assert_eq!(
        std::fs::read_to_string(cwd.join("add.txt")).expect("read added file"),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("update.txt")).expect("read updated file"),
        "new\n"
    );
    assert!(!cwd.join("delete.txt").exists());
    assert!(!cwd.join("from.txt").exists());
    assert_eq!(
        std::fs::read_to_string(cwd.join("moved").join("to.txt")).expect("read moved file"),
        "after\n"
    );

    let metadata = output.metadata.expect("metadata");
    let files = metadata["files"].as_array().expect("files metadata");
    assert_eq!(files.len(), 4);
    assert_eq!(files[0]["additions"], 1);
    assert_eq!(files[0]["deletions"], 0);
    assert_eq!(files[1]["additions"], 1);
    assert_eq!(files[1]["deletions"], 1);
    assert_eq!(files[2]["additions"], 0);
    assert_eq!(files[2]["deletions"], 1);
    assert_eq!(files[3]["additions"], 1);
    assert_eq!(files[3]["deletions"], 1);
}

#[tokio::test]
async fn execute_given_patch() {
    let content = r#"use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("read.txt");
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_BYTES: usize = 50 * 1024;
const MAX_BYTES_LABEL: &str = "50 KB";

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute path to the file or directory to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from (1-indexed, default 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read (no limit by default)"
                }
            },
            "required": ["filePath"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let mut filepath = input["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'filePath' field"))?
            .to_string();
        let offset = input["offset"].as_u64().map(|value| value as usize);
        let limit = input["limit"].as_u64().map(|value| value as usize);

        if let Some(offset) = offset {
            if offset < 1 {
                return Ok(ToolOutput::error(
                    "offset must be greater than or equal to 1",
                ));
            }
        }

        if !Path::new(&filepath).is_absolute() {
            filepath = ctx.cwd.join(&filepath).to_string_lossy().to_string();
        }

        let path = PathBuf::from(&filepath);
        if !path.exists() {
            return Ok(ToolOutput::error(missing_file_message(&filepath)));
        }

        if path.is_dir() {
            return read_directory(
                &path, limit.unwrap_or(usize::MAX),
                offset.unwrap_or(1),
            );
        }

        if is_binary_file(&path)? {
            return Ok(ToolOutput::error(format!(
                "Cannot read binary file: {}",
                path.display()
            )));
        }

        read_file(
            &path,
            limit.unwrap_or(usize::MAX),
            offset.unwrap_or(1),
        )
    }
}

fn read_directory(path: &Path, limit: usize, offset: usize) -> anyhow::Result<ToolOutput> {
    let mut items = std::fs::read_dir(path)?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir { format!("{name}/") } else { name }
        })
        .collect::<Vec<_>>();
    items.sort_unstable_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    let start = offset.saturating_sub(1);
    let sliced = items
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = start + sliced.len() < items.len();
    let preview = sliced
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    let output = [
        format!("<path>{}</path>", path.display()),
        "<type>directory</type>".to_string(),
        "<entries>".to_string(),
        sliced.join("\n"),
        if truncated {
            format!("\n(Showing {} of {} entries. Use 'offset' parameter to read beyond entry {})", sliced.len(), items.len(), offset + sliced.len())
        } else {
            format!("\n({} entries)", items.len())
        },
        "</entries>".to_string(),
    ]
    .join("\n");

    Ok(ToolOutput {
        content: output,
        is_error: false,
        metadata: Some(json!({
            "preview": preview,
            "truncated": truncated,
            "loaded": []
        })),
    })
}

fn read_file(path: &Path, limit: usize, offset: usize) -> anyhow::Result<ToolOutput> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let start = offset.saturating_sub(1);
    let mut raw = Vec::new();
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut cut = false;
    let mut more = false;

    for line in reader.lines() {
        let mut line = line?;
        count += 1;
        if count <= start {
            continue;
        }
        if raw.len() >= limit {
            more = true;
            continue;
        }
        if line.len() > MAX_LINE_LENGTH {
            line.truncate(MAX_LINE_LENGTH);
            line.push_str(MAX_LINE_SUFFIX);
        }
        let size = line.len() + if raw.is_empty() { 0 } else { 1 };
        if bytes + size > MAX_BYTES {
            cut = true;
            more = true;
            break;
        }
        raw.push(line);
        bytes += size;
    }

    if count < offset && !(count == 0 && offset == 1) {
        return Ok(ToolOutput::error(format!(
            "Offset {} is out of range for this file ({} lines)",
            offset, count
        )));
    }

    let mut output = format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n",
        path.display()
    );
    for (index, line) in raw.iter().enumerate() {
        output.push_str(&format!("{}: {}\n", offset + index, line));
    }

    let last = offset + raw.len().saturating_sub(1);
    let next = last + 1;
    if cut {
        output.push_str(&format!(
            "\n(Output capped at {}. Showing lines {}-{}. Use offset={} to continue.)",
            MAX_BYTES_LABEL, offset, last, next
        ));
    } else if more {
        output.push_str(&format!("\n(Showing lines {}-{} of {}. Use offset={} to continue.)", offset, last, count, next))
    } else {
        output.push_str(&format!("\n(End of file - total {} lines)", count))
    }
    output.push_str("\n</content>");

    Ok(ToolOutput {
        content: output,
        is_error: false,
        metadata: Some(json!({
            "preview": raw.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
            "truncated": cut || more,
            "loaded": []
        })),
    })
}

fn is_binary_file(path: &Path) -> anyhow::Result<bool> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "zip"
            | "tar"
            | "gz"
            | "exe"
            | "dll"
            | "so"
            | "class"
            | "jar"
            | "war"
            | "7z"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "bin"
            | "dat"
            | "obj"
            | "o"
            | "a"
            | "lib"
            | "wasm"
            | "pyc"
            | "pyo"
    ) {
        return Ok(true);
    }

    let mut file = File::open(path)?;
    let size = file.metadata()?.len() as usize;
    if size == 0 {
        return Ok(false);
    }

    let sample_size = size.min(4096);
    let mut bytes = vec![0u8; sample_size];
    let read = file.read(&mut bytes)?;
    if read == 0 {
        return Ok(false);
    }

    let mut non_printable = 0usize;
    for byte in bytes.iter().take(read) {
        if *byte == 0 {
            return Ok(true);
        }
        if *byte < 9 || (*byte > 13 && *byte < 32) {
            non_printable += 1;
        }
    }

    Ok((non_printable as f64) / (read as f64) > 0.3)
}

fn missing_file_message(filepath: &str) -> String {
    let path = Path::new(filepath);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let base = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(filepath);

    let suggestions = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| {
                    name.to_lowercase().contains(&base.to_lowercase())
                        || base.to_lowercase().contains(&name.to_lowercase())
                })
                .take(3)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if suggestions.is_empty() {
        format!("File not found: {filepath}")
    } else {
        format!(
            "File not found: {filepath}\n\nDid you mean one of these?\n{}",
            suggestions
                .into_iter()
                .map(|item| dir.join(item).to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn create_temp_dir(prefix: &str) -> PathBuf {
        let mut path = env::temp_dir();
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("lpa-tools-read-{prefix}-{ticks}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn read_directory_sorts_entries_and_reports_truncation() {
        let dir = create_temp_dir("dir");
        File::create(dir.join("b.txt")).unwrap();
        File::create(dir.join("a.txt")).unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();

        let output = read_directory(&dir, 1, 2).unwrap();
        assert!(output.content.contains("<type>directory</type>"));
        assert!(output.content.contains("b.txt"));
        assert!(
            output.content.contains(
                "(Showing 1 of 3 entries. Use 'offset' parameter to read beyond entry 3)"
            )
        );

        let metadata = output.metadata.unwrap();
        assert!(metadata.get("truncated").and_then(|value| value.as_bool()) == Some(true));
    }

    #[test]
    fn read_file_applies_limit_and_reports_more() {
        let dir = create_temp_dir("file");
        let path = dir.join("sample.txt");
        write_lines(&path, &["line1", "line2", "line3", "line4", "line5"]);

        let output = read_file(&path, 2, 2).unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("2: line2"));
        assert!(output.content.contains("3: line3"));
        assert!(
            output
                .content
                .contains("(Showing lines 2-3 of 5. Use offset=4 to continue.)")
        );

        let metadata = output.metadata.unwrap();
        assert!(metadata.get("truncated").and_then(|value| value.as_bool()) == Some(true));
    }

    #[test]
    fn read_file_reports_offset_out_of_range() {
        let dir = create_temp_dir("error");
        let path = dir.join("short.txt");
        write_lines(&path, &["hello", "world"]);

        let output = read_file(&path, 10, 5).unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Offset 5 is out of range"));
    }

    #[test]
    fn is_binary_file_detects_null_bytes() {
        let dir = create_temp_dir("binary");
        let path = dir.join("payload.bin");
        fs::write(&path, &[0u8, 1, 2]).unwrap();

        assert!(is_binary_file(&path).unwrap());
    }

    #[test]
    fn missing_file_message_includes_suggestions() {
        let dir = create_temp_dir("missing");
        let target = dir.join("example.txt");
        write_lines(&target, &["content"]);

        let missing = dir.join("example");
        let message = missing_file_message(&missing.to_string_lossy());
        assert!(message.contains("Did you mean"));
        assert!(message.contains("example.txt"));
    }
}
"#;
    let cwd = unique_temp_dir("execute");
    std::fs::write(cwd.join("read.rs"), content).expect("write update file");

    let ctx = make_ctx(cwd);

    let patch = r#"*** Begin Patch
*** Update File: read.rs
@@ use std::{
     fs::File,
     io::{BufRead, BufReader, Read},
     path::{Path, PathBuf},
 };
 
 use async_trait::async_trait;
 use serde_json::json;
 
 use crate::{Tool, ToolContext, ToolOutput};
 
 const DESCRIPTION: &str = include_str!("read.txt");
-const MAX_LINE_LENGTH: usize = 2000;
-const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
-const MAX_BYTES: usize = 50 * 1024;
-const MAX_BYTES_LABEL: &str = "50 KB";
 
 pub struct ReadTool;
*** End Patch"#;

    let output = ApplyPatchTool
        .execute(
            &ctx,
            json!({
                "patchText": patch
            }),
        )
        .await
        .expect("execute apply_patch");

    assert_eq!(output.is_error, false);
}
