use super::types::{HunkLine, PatchChange, PatchHunk, PatchKind};

pub(super) fn is_file_header_line(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

pub(super) fn parse_patch(patch_text: &str) -> anyhow::Result<Vec<PatchChange>> {
    let normalized = patch_text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines().peekable();

    let Some(first_line) = lines.peek().copied() else {
        return Ok(Vec::new());
    };

    let mut wrapped = false;
    if first_line == "*** Begin Patch" {
        wrapped = true;
        lines.next();
    } else if !is_file_header_line(first_line) {
        return Err(anyhow::anyhow!(
            "patch must start with *** Begin Patch or a file operation header"
        ));
    }

    let mut changes = Vec::new();
    let mut saw_end_patch = false;

    while let Some(line) = lines.next() {
        if line == "*** End Patch" {
            saw_end_patch = true;
            break;
        }

        if line == "*** End of File" {
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let contents = collect_plus_block(&mut lines)?;
            changes.push(PatchChange {
                path: path.to_string(),
                move_path: None,
                content: contents,
                hunks: Vec::new(),
                kind: PatchKind::Add,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            changes.push(PatchChange {
                path: path.to_string(),
                move_path: None,
                content: String::new(),
                hunks: Vec::new(),
                kind: PatchKind::Delete,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let mut move_path = None;
            if matches!(lines.peek(), Some(next) if next.starts_with("*** Move to: ")) {
                let next = lines.next().unwrap_or_default();
                move_path = Some(next.trim_start_matches("*** Move to: ").to_string());
            }
            let hunks = collect_hunk_block(&mut lines)?;
            let kind = if move_path.is_some() {
                PatchKind::Move
            } else {
                PatchKind::Update
            };
            changes.push(PatchChange {
                path: path.to_string(),
                move_path,
                content: String::new(),
                hunks,
                kind,
            });
            continue;
        }

        return Err(anyhow::anyhow!(
            "expected file operation header, got: {line}"
        ));
    }

    if changes.is_empty() {
        return Err(anyhow::anyhow!("no patch operations found"));
    }

    if wrapped && !saw_end_patch {
        return Err(anyhow::anyhow!("patch must end with *** End Patch"));
    }

    Ok(changes)
}

pub(super) fn is_hunk_header_line(line: &str) -> bool {
    line == "@@" || line.starts_with("@@ ")
}

pub(super) fn is_git_diff_metadata_line(line: &str) -> bool {
    line.starts_with("diff --git ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
}

pub(super) fn collect_plus_block(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> anyhow::Result<String> {
    let mut content = String::new();
    while let Some(next) = lines.peek() {
        if next.starts_with("*** ") {
            break;
        }
        let line = lines.next().unwrap_or_default();
        if let Some(rest) = line.strip_prefix('+') {
            content.push_str(rest);
            content.push('\n');
        } else {
            return Err(anyhow::anyhow!(
                "add file lines must start with +, got: {line}"
            ));
        }
    }
    Ok(content)
}

pub(super) fn collect_hunk_block(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> anyhow::Result<Vec<PatchHunk>> {
    let mut hunks = Vec::new();
    let mut current_hunk: Option<PatchHunk> = None;
    let mut saw_hunk = false;

    while let Some(next) = lines.peek() {
        if next.starts_with("*** ") && !next.starts_with("*** End of File") {
            break;
        }
        let line = lines.next().unwrap_or_default();
        if line == "*** End of File" {
            break;
        }
        if current_hunk.is_none() && is_git_diff_metadata_line(line) {
            continue;
        }
        if is_hunk_header_line(line) {
            saw_hunk = true;
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            current_hunk = Some(PatchHunk { lines: Vec::new() });
            continue;
        }
        let Some(hunk) = current_hunk.as_mut() else {
            return Err(anyhow::anyhow!(
                "encountered patch lines before a hunk header"
            ));
        };
        match line.chars().next() {
            Some('+') => hunk.lines.push(HunkLine::Add(line[1..].to_string())),
            Some(' ') => hunk.lines.push(HunkLine::Context(line[1..].to_string())),
            Some('-') => {
                saw_hunk = true;
                hunk.lines.push(HunkLine::Remove(line[1..].to_string()));
            }
            None => {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
            _ => return Err(anyhow::anyhow!("unsupported hunk line: {line}")),
        };
    }

    if let Some(hunk) = current_hunk.take() {
        hunks.push(hunk);
    }

    if !saw_hunk && hunks.iter().all(|hunk| hunk.lines.is_empty()) {
        return Err(anyhow::anyhow!("no hunks found"));
    }

    Ok(hunks)
}
