use crate::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct DiffLine {
    pub(crate) old_line_no: i32,
    pub(crate) new_line_no: i32,
    pub(crate) content: String,
    pub(crate) origin: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct HunkDiff {
    pub(crate) old_start: u32,
    pub(crate) new_start: u32,
    pub(crate) old_lines: u32,
    pub(crate) new_lines: u32,
    pub(crate) lines: Vec<DiffLine>,
}

#[derive(Clone, Serialize)]
pub(crate) struct FileDiff {
    pub(crate) old_path: Option<String>,
    pub(crate) new_path: Option<String>,
    pub(crate) hunks: Vec<HunkDiff>,
}

pub(crate) fn collect_structured_diff(diff: &git2::Diff) -> Result<FileDiff, String> {
    let old_path = diff.deltas().next()
        .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()));
    let new_path = diff.deltas().next()
        .and_then(|d| d.new_file().path().map(|p| p.to_string_lossy().to_string()));

    let hunks = std::cell::RefCell::new(Vec::<HunkDiff>::new());
    let current_lines = std::cell::RefCell::new(Vec::<DiffLine>::new());
    let current_hunk = std::cell::RefCell::new(Option::<(u32, u32, u32, u32)>::None);

    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |_delta, hunk| {
            if let Some((old_start, new_start, old_lines, new_lines)) = current_hunk.borrow_mut().take() {
                hunks.borrow_mut().push(HunkDiff {
                    old_start, new_start, old_lines, new_lines,
                    lines: current_lines.borrow_mut().drain(..).collect(),
                });
            }
            *current_hunk.borrow_mut() = Some((
                hunk.old_start(), hunk.new_start(), hunk.old_lines(), hunk.new_lines(),
            ));
            true
        }),
        Some(&mut |_delta, _hunk, line| {
            // libgit2 emits a synthetic "\ No newline at end of file" line with an
            // EOFNL origin ('=', '>', '<'). It carries no real file content — folding
            // it into a hunk inflates the reconstructed text and breaks length-based
            // hunk revert (the new-side text no longer matches the buffer). Skip it.
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
                '=' | '>' | '<' => return true,
                _ => "context",
            }.to_string();
            current_lines.borrow_mut().push(DiffLine {
                old_line_no: line.old_lineno().map(|n| n as i32).unwrap_or(-1),
                new_line_no: line.new_lineno().map(|n| n as i32).unwrap_or(-1),
                content: String::from_utf8_lossy(line.content()).to_string(),
                origin,
            });
            true
        }),
    ).map_err(|e| e.to_string())?;

    // Flush the last hunk
    if let Some((old_start, new_start, old_lines, new_lines)) = current_hunk.borrow_mut().take() {
        hunks.borrow_mut().push(HunkDiff {
            old_start, new_start, old_lines, new_lines,
            lines: current_lines.borrow_mut().drain(..).collect(),
        });
    }

    Ok(FileDiff { old_path, new_path, hunks: hunks.into_inner() })
}

// Walks a libgit2 Diff that may span multiple files (e.g. tree-to-tree
// between two refs) and returns one FileDiff per delta. Same line/hunk
// shape as collect_structured_diff so the frontend renderer is unchanged.
pub(crate) fn collect_structured_files_diff(diff: &git2::Diff) -> Result<Vec<FileDiff>, String> {
    use std::cell::RefCell;

    let files: RefCell<Vec<FileDiff>> = RefCell::new(Vec::new());
    let current_file: RefCell<Option<FileDiff>> = RefCell::new(None);
    let current_hunk: RefCell<Option<(u32, u32, u32, u32)>> = RefCell::new(None);
    let current_lines: RefCell<Vec<DiffLine>> = RefCell::new(Vec::new());

    diff.foreach(
        &mut |delta, _progress| {
            // file_cb fires once per file, before any hunk_cb for that file.
            // Flush any pending hunk into the previous file, push it, then
            // open a new accumulator for this delta.
            let mut cur = current_file.borrow_mut();
            if let Some(mut prev) = cur.take() {
                if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
                    prev.hunks.push(HunkDiff {
                        old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                        lines: current_lines.borrow_mut().drain(..).collect(),
                    });
                }
                files.borrow_mut().push(prev);
            }
            *cur = Some(FileDiff {
                old_path: delta.old_file().path().map(|p| p.to_string_lossy().to_string()),
                new_path: delta.new_file().path().map(|p| p.to_string_lossy().to_string()),
                hunks: Vec::new(),
            });
            true
        },
        None,
        Some(&mut |_delta, hunk| {
            // Flush previous hunk into the current file, then open new one.
            if let Some(file) = current_file.borrow_mut().as_mut() {
                if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
                    file.hunks.push(HunkDiff {
                        old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                        lines: current_lines.borrow_mut().drain(..).collect(),
                    });
                }
            }
            *current_hunk.borrow_mut() = Some((
                hunk.old_start(), hunk.new_start(), hunk.old_lines(), hunk.new_lines(),
            ));
            true
        }),
        Some(&mut |_delta, _hunk, line| {
            // Skip the synthetic EOFNL marker line (see collect_structured_diff).
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
                '=' | '>' | '<' => return true,
                _ => "context",
            }.to_string();
            current_lines.borrow_mut().push(DiffLine {
                old_line_no: line.old_lineno().map(|n| n as i32).unwrap_or(-1),
                new_line_no: line.new_lineno().map(|n| n as i32).unwrap_or(-1),
                content: String::from_utf8_lossy(line.content()).to_string(),
                origin,
            });
            true
        }),
    ).map_err(|e| e.to_string())?;

    // Flush trailing hunk + file
    if let Some(mut last) = current_file.borrow_mut().take() {
        if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
            last.hunks.push(HunkDiff {
                old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                lines: current_lines.borrow_mut().drain(..).collect(),
            });
        }
        files.borrow_mut().push(last);
    }

    Ok(files.into_inner())
}

#[derive(Clone, Serialize)]
pub(crate) struct RefDiffStats {
    pub(crate) files_changed: usize,
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
}

#[derive(Clone, Serialize)]
pub(crate) struct RefDiff {
    pub(crate) from_ref: String,
    pub(crate) to_ref: String,
    pub(crate) files: Vec<FileDiff>,
    pub(crate) stats: RefDiffStats,
}

#[tauri::command]
pub(crate) async fn get_diff_between_refs(
    path: String,
    from_ref: String,
    to_ref: String,
) -> Result<RefDiff, String> {
    blocking(move || {
        if !(is_valid_git_ref(&from_ref) || is_valid_git_oid(&from_ref)) {
            return Err("Invalid `from` ref".into());
        }
        if !(is_valid_git_ref(&to_ref) || is_valid_git_oid(&to_ref)) {
            return Err("Invalid `to` ref".into());
        }

        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let from_obj = repo.revparse_single(&from_ref).map_err(|e| format!("from `{}`: {}", from_ref, e))?;
        let to_obj = repo.revparse_single(&to_ref).map_err(|e| format!("to `{}`: {}", to_ref, e))?;
        let from_tree = from_obj.peel_to_tree().map_err(|e| format!("from `{}` peel: {}", from_ref, e))?;
        let to_tree = to_obj.peel_to_tree().map_err(|e| format!("to `{}` peel: {}", to_ref, e))?;

        let mut diff_opts = git2::DiffOptions::new();
        let diff = repo
            .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut diff_opts))
            .map_err(|e| e.to_string())?;

        let files = collect_structured_files_diff(&diff)?;
        let mut additions = 0usize;
        let mut deletions = 0usize;
        for f in &files {
            for h in &f.hunks {
                for l in &h.lines {
                    match l.origin.as_str() {
                        "add" => additions += 1,
                        "remove" => deletions += 1,
                        _ => {}
                    }
                }
            }
        }
        let files_changed = files.len();
        Ok(RefDiff {
            from_ref,
            to_ref,
            files,
            stats: RefDiffStats { files_changed, additions, deletions },
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_file_diff(path: String, file_path: String, staged: Option<bool>) -> Result<FileDiff, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let mut diff_opts = git2::DiffOptions::new();
        diff_opts.pathspec(&file_path);

        if staged.unwrap_or(false) {
            // Staged diff: index vs HEAD
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            collect_structured_diff(&diff)
        } else {
            // Unstaged diff: workdir vs index
            let diff = repo
                .diff_index_to_workdir(None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            let result = collect_structured_diff(&diff)?;
            if !result.hunks.is_empty() {
                return Ok(result);
            }
            // Fall back to staged if no unstaged changes (backwards compat)
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            collect_structured_diff(&diff)
        }
    })
    .await
}

// Working-tree-vs-HEAD diff for a single file, used by the editor change
// gutter. Deliberately distinct from `get_file_diff` (workdir-vs-index with a
// staged fallback): the gutter wants "everything different from the last
// commit" so a hunk that gets staged still shows a marker. `diff_tree_to_
// workdir_with_index` folds the index in, giving exactly that view. Returns an
// empty FileDiff (no hunks) when the file matches HEAD or there is no HEAD yet
// (unborn branch) so the frontend can no-op cleanly.
#[tauri::command]
pub(crate) async fn get_file_diff_vs_head(path: String, file_path: String) -> Result<FileDiff, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let mut diff_opts = git2::DiffOptions::new();
        diff_opts.pathspec(&file_path);

        // No HEAD (fresh repo, unborn branch): everything is "new", but there's
        // no tree to diff against — treat as no tracked changes for the gutter.
        let head_tree = match repo.head().ok().and_then(|h| h.peel_to_tree().ok()) {
            Some(t) => t,
            None => return Ok(FileDiff { old_path: None, new_path: None, hunks: vec![] }),
        };

        let diff = repo
            .diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut diff_opts))
            .map_err(|e| e.to_string())?;
        collect_structured_diff(&diff)
    })
    .await
}
