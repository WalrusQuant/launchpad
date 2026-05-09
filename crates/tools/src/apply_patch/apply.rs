use std::path::{Component, Path, PathBuf};

use tokio::fs;

use super::hunk_match::{find_hunk_start, lines_match_mode, normalized_lines};
use super::types::{HunkLine, PatchChange, PatchHunk, PatchKind};

pub(super) fn resolve_relative(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(anyhow::anyhow!(
            "file references can only be relative, NEVER ABSOLUTE."
        ));
    }

    let mut out = base.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => out.push(".."),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow::anyhow!(
                    "file references can only be relative, NEVER ABSOLUTE."
                ));
            }
        }
    }
    Ok(out)
}

pub(super) fn relative_worktree_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(super) async fn read_file(path: &Path) -> anyhow::Result<String> {
    Ok(fs::read_to_string(path).await?)
}

pub(super) async fn apply_change(base: &Path, change: &PatchChange) -> anyhow::Result<()> {
    let source = resolve_relative(base, &change.path)?;
    match change.kind {
        PatchKind::Add => {
            if let Some(parent) = source.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::write(&source, &change.content).await?;
        }
        PatchKind::Update => {
            let old_content = read_file(&source).await?;
            let new_content = apply_hunks(&old_content, &change.hunks)?;
            fs::write(&source, &new_content).await?;
        }
        PatchKind::Delete => {
            let _ = fs::remove_file(&source).await;
        }
        PatchKind::Move => {
            if let Some(dest) = &change.move_path {
                let dest = resolve_relative(base, dest)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).await?;
                }
                let old_content = read_file(&source).await?;
                let new_content = apply_hunks(&old_content, &change.hunks)?;
                fs::write(&dest, &new_content).await?;
                let _ = fs::remove_file(&source).await;
            }
        }
    }
    Ok(())
}

pub(super) fn apply_hunks(old_content: &str, hunks: &[PatchHunk]) -> anyhow::Result<String> {
    let old_lines = normalized_lines(old_content);
    let mut output = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let matched_hunk = find_hunk_start(&old_lines, cursor, hunk)?;
        let start = matched_hunk.start;
        output.extend_from_slice(&old_lines[cursor..start]);
        let mut position = start;
        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    let actual = old_lines.get(position).ok_or_else(|| {
                        anyhow::anyhow!("context line beyond end of file: {expected}")
                    })?;
                    if !lines_match_mode(expected, actual, matched_hunk.mode) {
                        return Err(anyhow::anyhow!(
                            "context mismatch while applying patch: expected {expected:?}, got {actual:?}"
                        ));
                    }
                    output.push(actual.clone());
                    position += 1;
                }
                HunkLine::Remove(expected) => {
                    let actual = old_lines.get(position).ok_or_else(|| {
                        anyhow::anyhow!("removed line beyond end of file: {expected}")
                    })?;
                    if !lines_match_mode(expected, actual, matched_hunk.mode) {
                        return Err(anyhow::anyhow!(
                            "remove mismatch while applying patch: expected {expected:?}, got {actual:?}"
                        ));
                    }
                    position += 1;
                }
                HunkLine::Add(line) => output.push(line.clone()),
            }
        }
        cursor = position;
    }

    output.extend_from_slice(&old_lines[cursor..]);
    Ok(if output.is_empty() {
        String::new()
    } else {
        format!("{}\n", output.join("\n"))
    })
}
