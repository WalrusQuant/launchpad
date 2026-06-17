// Git: status, branches, commits, diffs, staging, commit/amend, stash, discard,
// conflict resolution, the cancellable network-op machinery (push/pull/fetch/
// merge), cherry-pick, and interactive rebase. Local operations use the git2
// (libgit2) crate; network operations shell out to system `git` so the user's
// SSH keys and credential helpers apply. Heavier calls run off the IPC thread
// via `blocking`.
use crate::*;
use git2::{BranchType, StatusOptions};
// Re-exported at the crate root (via `pub(crate) use git::*`) so the test
// module — which does `use super::*` and constructs `Repository` directly —
// keeps the same crate-root name it had before git code was split out.
pub(crate) use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;

#[derive(Clone, Serialize)]
pub(crate) struct GitFileStatus {
    pub(crate) path: String,
    pub(crate) status: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct GitInfo {
    pub(crate) is_repo: bool,
    pub(crate) branch: Option<String>,
    pub(crate) files: Vec<GitFileStatus>,
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
    pub(crate) has_upstream: bool,
}

pub(crate) fn get_git_status_inner(path: &str) -> Result<GitInfo, String> {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => {
            return Ok(GitInfo {
                is_repo: false,
                branch: None,
                files: vec![],
                ahead: 0,
                behind: 0,
                has_upstream: false,
            });
        }
    };

    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(String::from));

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;

    let mut files: Vec<GitFileStatus> = Vec::new();
    for entry in statuses.iter() {
        let path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let s = entry.status();

        if s.is_conflicted() {
            files.push(GitFileStatus { path, status: "conflict".into() });
            continue;
        }

        // Index (staged) changes
        if s.is_index_new() {
            files.push(GitFileStatus { path: path.clone(), status: "index_new".into() });
        } else if s.is_index_modified() {
            files.push(GitFileStatus { path: path.clone(), status: "index_modified".into() });
        } else if s.is_index_deleted() {
            files.push(GitFileStatus { path: path.clone(), status: "index_deleted".into() });
        } else if s.is_index_renamed() {
            files.push(GitFileStatus { path: path.clone(), status: "index_renamed".into() });
        }

        // Worktree changes
        if s.is_wt_new() {
            files.push(GitFileStatus { path: path.clone(), status: "new".into() });
        } else if s.is_wt_modified() {
            files.push(GitFileStatus { path: path.clone(), status: "modified".into() });
        } else if s.is_wt_deleted() {
            files.push(GitFileStatus { path: path.clone(), status: "deleted".into() });
        } else if s.is_wt_renamed() {
            files.push(GitFileStatus { path: path.clone(), status: "renamed".into() });
        }
    }

    let ab = get_ahead_behind(&repo);
    let has_upstream = ab.is_some();
    let ahead = ab.map(|(a, _)| a).unwrap_or(0);
    let behind = ab.map(|(_, b)| b).unwrap_or(0);

    Ok(GitInfo {
        is_repo: true,
        branch,
        files,
        ahead,
        behind,
        has_upstream,
    })
}

#[tauri::command]
pub(crate) async fn get_git_status(path: String) -> Result<GitInfo, String> {
    blocking(move || get_git_status_inner(&path)).await
}

pub(crate) fn get_ahead_behind(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    let local_oid = head.target()?;
    let branch_name = head.shorthand()?;

    let upstream_ref_name = repo.branch_upstream_name(&format!("refs/heads/{}", branch_name)).ok()?;
    let upstream_name = upstream_ref_name.as_str()?;
    let upstream_ref = repo.find_reference(upstream_name).ok()?;
    let upstream_oid = upstream_ref.target()?;

    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}

#[derive(Clone, Serialize)]
pub(crate) struct BranchInfo {
    pub(crate) name: String,
    pub(crate) is_current: bool,
    pub(crate) last_commit_msg: Option<String>,
    pub(crate) last_commit_time: Option<i64>,
    pub(crate) upstream: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct CommitInfo {
    pub(crate) oid: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) parent_count: usize,
}

#[tauri::command]
pub(crate) async fn list_branches(path: String) -> Result<Vec<BranchInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let branches = repo
            .branches(Some(BranchType::Local))
            .map_err(|e| e.to_string())?;

        let current_branch = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from));

        let mut result: Vec<BranchInfo> = branches
            .filter_map(|b| {
                let (branch, _) = b.ok()?;
                let name = branch.name().ok()??.to_string();
                let is_current = current_branch.as_deref() == Some(&name);

                let commit = branch.get().peel_to_commit().ok();
                let last_commit_msg = commit
                    .as_ref()
                    .and_then(|c| c.summary().map(String::from));
                let last_commit_time = commit.as_ref().map(|c| c.time().seconds());

                let upstream = branch
                    .upstream()
                    .ok()
                    .and_then(|u| u.name().ok().flatten().map(String::from));

                Some(BranchInfo {
                    name,
                    is_current,
                    last_commit_msg,
                    last_commit_time,
                    upstream,
                })
            })
            .collect();

        result.sort_by(|a, b| {
            b.is_current
                .cmp(&a.is_current)
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(result)
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_commits(path: String, count: Option<usize>) -> Result<Vec<CommitInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let head = match repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(vec![]),
        };
        let head_oid = match head.target() {
            Some(oid) => oid,
            None => return Ok(vec![]),
        };

        let mut revwalk = repo.revwalk().map_err(|e| e.to_string())?;
        revwalk.push(head_oid).map_err(|e| e.to_string())?;

        let limit = count.unwrap_or(20);

        let commits: Vec<CommitInfo> = revwalk
            .take(limit)
            .filter_map(|oid| {
                let oid = oid.ok()?;
                let commit = repo.find_commit(oid).ok()?;
                let short_id = oid.to_string()[..7].to_string();
                let message = commit
                    .summary()
                    .unwrap_or("(no message)")
                    .to_string();
                let author = commit.author().name().unwrap_or("Unknown").to_string();
                let timestamp = commit.time().seconds();

                Some(CommitInfo {
                    oid: short_id,
                    message,
                    author,
                    timestamp,
                    parent_count: commit.parent_count(),
                })
            })
            .collect();

        Ok(commits)
    })
    .await
}

#[tauri::command]
pub(crate) async fn checkout_branch(path: String, branch_name: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        checkout_branch_inner(&repo, &branch_name)
    })
    .await
}

#[tauri::command]
pub(crate) async fn create_branch(path: String, branch_name: String, checkout: Option<bool>) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let head = repo.head().map_err(|e| e.to_string())?;
        let commit = head.peel_to_commit().map_err(|e| e.to_string())?;

        repo.branch(&branch_name, &commit, false)
            .map_err(|e| e.to_string())?;

        if checkout.unwrap_or(true) {
            checkout_branch_inner(&repo, &branch_name)?;
        }

        Ok(())
    })
    .await
}

pub(crate) fn checkout_branch_inner(repo: &Repository, branch_name: &str) -> Result<(), String> {
    let branch = repo
        .find_branch(branch_name, BranchType::Local)
        .map_err(|e| e.to_string())?;

    let refname = branch
        .get()
        .name()
        .ok_or("Invalid branch reference")?
        .to_string();

    // Resolve the target tree and attempt a safe checkout first. libgit2 will
    // error if the operation would overwrite uncommitted changes — matching
    // `git checkout <branch>` behavior. Only move HEAD after the working tree
    // is successfully updated so a failure here doesn't leave HEAD pointing
    // at a branch whose tree we never actually checked out.
    let target_commit = branch.get().peel_to_commit().map_err(|e| e.to_string())?;
    let target_tree = target_commit.tree().map_err(|e| e.to_string())?;

    repo.checkout_tree(target_tree.as_object(), None)
        .map_err(|e| e.to_string())?;

    repo.set_head(&refname).map_err(|e| e.to_string())?;

    Ok(())
}
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
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
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
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
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

#[tauri::command]
pub(crate) async fn git_stage_all(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut index = repo.index().map_err(|e| e.to_string())?;
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .map_err(|e| e.to_string())?;
        index.write().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stage_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut index = repo.index().map_err(|e| e.to_string())?;
        index
            .add_path(std::path::Path::new(&file_path))
            .map_err(|e| e.to_string())?;
        index.write().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_commit(path: String, message: String) -> Result<String, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let mut index = repo.index().map_err(|e| e.to_string())?;
        let tree_oid = index.write_tree().map_err(|e| e.to_string())?;
        let tree = repo.find_tree(tree_oid).map_err(|e| e.to_string())?;

        let sig = repo.signature().map_err(|e| e.to_string())?;

        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok());

        let parents: Vec<&git2::Commit> = parent.iter().collect();

        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)
            .map_err(|e| e.to_string())?;

        Ok(oid.to_string()[..7].to_string())
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct AmendResult {
    pub(crate) oid: String,
    pub(crate) requires_force_push: bool,
}

// True when HEAD's commit is reachable from any refs/remotes/* — i.e. an
// amend will rewrite a commit that already exists on a remote, so the user
// will need a force-push to share the new version.
pub(crate) fn head_is_on_any_remote(repo: &Repository, head_oid: git2::Oid) -> Result<bool, String> {
    let refs = repo.references_glob("refs/remotes/*").map_err(|e| e.to_string())?;
    for r in refs.flatten() {
        let target = match r.target() {
            Some(t) => t,
            None => continue,
        };
        if target == head_oid {
            return Ok(true);
        }
        if matches!(repo.graph_descendant_of(target, head_oid), Ok(true)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tauri::command]
pub(crate) async fn git_head_on_remote(path: String) -> Result<bool, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let head = match repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(false), // unborn HEAD
        };
        let head_oid = head.peel_to_commit().map_err(|e| e.to_string())?.id();
        head_is_on_any_remote(&repo, head_oid)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_amend_commit(
    path: String,
    message: Option<String>,
    include_staged: bool,
) -> Result<AmendResult, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        // Refuse on unborn or detached HEAD — both leave no clear branch to
        // amend onto, and silently rewriting a detached HEAD is dangerous.
        if matches!(repo.head_detached(), Ok(true)) {
            return Err("HEAD is detached — switch to a branch before amending".into());
        }
        let head_ref = repo
            .head()
            .map_err(|_| "HEAD is unborn — nothing to amend".to_string())?;
        let head_commit = head_ref.peel_to_commit().map_err(|e| e.to_string())?;
        let head_oid = head_commit.id();

        let requires_force_push = head_is_on_any_remote(&repo, head_oid)?;

        // Tree: from index when include_staged, else keep HEAD's tree.
        let new_tree_oid = if include_staged {
            let mut index = repo.index().map_err(|e| e.to_string())?;
            index.write_tree().map_err(|e| e.to_string())?
        } else {
            head_commit.tree().map_err(|e| e.to_string())?.id()
        };
        let new_tree = repo.find_tree(new_tree_oid).map_err(|e| e.to_string())?;

        let sig = repo.signature().map_err(|e| e.to_string())?;
        let new_oid = head_commit
            .amend(
                Some("HEAD"),
                Some(&sig),
                Some(&sig),
                None,
                message.as_deref(),
                Some(&new_tree),
            )
            .map_err(|e| e.to_string())?;

        Ok(AmendResult {
            oid: new_oid.to_string(),
            requires_force_push,
        })
    })
    .await
}

pub(crate) struct GitOpSlot {
    pub(crate) op_id: u64,
    pub(crate) pid: Option<u32>,
    pub(crate) cancelled: bool,
}

// Per-window git op tracking, keyed by `tauri::Window::label()`. At most one
// entry per window; absent key means no in-flight op for that window.
pub(crate) type GitOpSlots = HashMap<String, GitOpSlot>;

// SIGKILL, not SIGTERM. A git process blocked on a stalled TCP recv() often
// ignores SIGTERM — which is the exact situation cancel is supposed to
// escape — so we skip the polite signal and go straight to -9.
pub(crate) fn kill_pid_hard(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

// SSH_AUTH_SOCK resolution. Tauri .app bundles launched from Finder/Dock
// don't inherit SSH_AUTH_SOCK from the user's login shell, so SSH git
// remotes fail with "Permission denied (publickey)". On macOS we fall back
// to `launchctl getenv` which returns the per-user value the system
// ssh-agent advertises.
//
// Positive results are cached for the process lifetime (the socket path
// doesn't change after login). Negative results are cached only for
// SSH_AUTH_SOCK_NEGATIVE_TTL so a user whose agent registers late (or who
// adds their first key after launch) recovers without an app restart.
pub(crate) static SSH_AUTH_SOCK_CACHE: Mutex<Option<(Option<String>, Instant)>> = Mutex::new(None);
pub(crate) const SSH_AUTH_SOCK_NEGATIVE_TTL: Duration = Duration::from_secs(60);

pub(crate) fn resolve_ssh_auth_sock() -> Option<String> {
    let mut guard = SSH_AUTH_SOCK_CACHE.lock_safe();
    if let Some((cached, at)) = guard.as_ref() {
        if cached.is_some() || at.elapsed() < SSH_AUTH_SOCK_NEGATIVE_TTL {
            return cached.clone();
        }
    }
    let resolved = compute_ssh_auth_sock();
    *guard = Some((resolved.clone(), Instant::now()));
    resolved
}

pub(crate) fn compute_ssh_auth_sock() -> Option<String> {
    if let Ok(v) = std::env::var("SSH_AUTH_SOCK") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("launchctl")
            .args(["getenv", "SSH_AUTH_SOCK"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

// Apply the standard env every spawned `git` Command should carry:
// - GIT_TERMINAL_PROMPT=0 / GIT_ASKPASS=echo: disable interactive credential
//   prompts that would block forever in a GUI bundle with no TTY. Auth errors
//   now return immediately instead of hanging.
// - SSH_AUTH_SOCK: forwarded via resolve_ssh_auth_sock() so SSH remotes work
//   when the .app is launched from Finder/Dock.
pub(crate) fn apply_git_env(cmd: &mut std::process::Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_ASKPASS", "echo");
    if let Some(sock) = resolve_ssh_auth_sock() {
        cmd.env("SSH_AUTH_SOCK", sock);
    }
}

// Spawn a git child under an already-reserved slot (caller owns the
// reservation and the final cleanup). Records the PID on the per-window slot,
// honors a cancel that arrived during the spawn window by killing
// immediately, and on return clears the PID on the slot so a subsequent call
// under the same reservation gets a fresh pid field and can't accidentally
// target the wrong child. The cancelled flag persists across calls on the
// same slot.
pub(crate) fn spawn_git_under_slot(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    label: &str,
    op_id: u64,
) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    apply_git_env(&mut command);

    let child = command.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();

    let kill_now = {
        let mut guard = slots.lock_safe();
        match guard.get_mut(label) {
            Some(s) if s.op_id == op_id => {
                s.pid = Some(pid);
                s.cancelled
            }
            // Slot for this window was claimed by a later op (concurrent
            // push/pull from two clicks in the same window). Treat as
            // not-ours; the other op owns cancellation now.
            _ => false,
        }
    };
    if kill_now {
        kill_pid_hard(pid);
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;

    // Clear PID on the slot — but NOT the slot itself. The caller may run
    // another spawn under the same reservation (git_push's auto-upstream
    // retry does this), and we want the shared `cancelled` flag to persist.
    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get_mut(label) {
            if s.op_id == op_id {
                s.pid = None;
            }
        }
    }

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(if stdout.trim().is_empty() { stderr } else { stdout })
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

// Reserve a slot for `label`, spawn a single git command, clean up. For
// multi-step ops (e.g. push-then-retry-with-upstream) that need a cancel to
// span the full sequence, reserve a slot manually and call
// spawn_git_under_slot directly.
pub(crate) fn run_git_cancellable(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    label: &str,
) -> Result<String, String> {
    // Reserve the slot BEFORE spawn. If cancel arrives in the window between
    // spawn() and PID-record, it sets `cancelled=true` on our slot, which
    // spawn_git_under_slot observes and acts on by killing the child.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = spawn_git_under_slot(args, path, slots, label, op_id);

    // Only clear the slot if it still matches our op. A subsequent operation
    // in the same window may already have replaced it — in that case leave
    // it alone.
    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get(label) {
            if s.op_id == op_id {
                guard.remove(label);
            }
        }
    }

    result
}

// Returns (has_upstream, current_branch_name). Determined via libgit2 so
// we don't depend on parsing localized stderr text from git(1).
pub(crate) fn git_upstream_status(path: &str) -> (bool, Option<String>) {
    let Ok(repo) = Repository::discover(path) else {
        return (false, None);
    };
    let Ok(head) = repo.head() else {
        return (false, None);
    };
    let Some(branch_name) = head.shorthand().map(str::to_string) else {
        return (false, None);
    };
    let Ok(branch) = repo.find_branch(&branch_name, BranchType::Local) else {
        return (false, Some(branch_name));
    };
    let has = branch.upstream().is_ok();
    (has, Some(branch_name))
}

#[tauri::command]
pub(crate) async fn git_push(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        let (has_upstream, branch) = git_upstream_status(&path);

        // Reserve ONE slot that spans both the initial push and the optional
        // auto-upstream retry. A cancel arriving in the window between the two
        // attempts would otherwise find an empty slot and be lost — with one
        // shared reservation, the cancelled flag persists and the retry's child
        // is killed as soon as it's spawned.
        let op_id = counter.fetch_add(1, Ordering::SeqCst);
        slots.lock_safe().insert(
            label.to_string(),
            GitOpSlot {
                op_id,
                pid: None,
                cancelled: false,
            },
        );

        let result = if has_upstream {
            spawn_git_under_slot(&["push"], &path, &slots, &label, op_id)
        } else {
            match branch {
                Some(b) if !b.is_empty() => spawn_git_under_slot(
                    &["push", "--set-upstream", "origin", &b],
                    &path,
                    &slots,
                    &label,
                    op_id,
                ),
                _ => Err("Could not determine current branch for upstream push".to_string()),
            }
        };

        // Release the reservation.
        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get(&label) {
                if s.op_id == op_id {
                    guard.remove(&label);
                }
            }
        }

        result
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_pull(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["pull", "--prune"], &path, &slots, &counter, &label)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_fetch(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["fetch", "--all", "--prune"], &path, &slots, &counter, &label)
    })
    .await
}

// Mark the active git slot for `label` cancelled and return the PID (if any)
// that the caller should kill. Split out so tests can exercise the state
// machine without spawning a real git process.
//
// Slot is intentionally NOT removed here — run_git_cancellable's cleanup
// only removes when its own op_id matches, so a stale cancel can't wipe a
// newer op's registration. The cancelled flag persists so a still-spawning
// op can observe it after recording its PID.
pub(crate) fn cancel_git_op_inner(slots: &Mutex<GitOpSlots>, label: &str) -> Option<u32> {
    let mut guard = slots.lock_safe();
    if let Some(slot) = guard.get_mut(label) {
        slot.cancelled = true;
        return slot.pid;
    }
    None
}

#[tauri::command]
pub(crate) fn cancel_git_op(state: State<AppState>, window: tauri::Window) -> Result<(), String> {
    if let Some(pid) = cancel_git_op_inner(&state.git_op_slots, window.label()) {
        kill_pid_hard(pid);
    }
    Ok(())
}

pub(crate) fn is_valid_git_ref(name: &str) -> bool {
    // Reject empties and leading `-` (option injection: `--no-verify`).
    // Reject `..` (git ref-format forbids it; also blocks range syntax sneaking through).
    // Allow [A-Za-z0-9._/-] plus `^` and `~` for parent/ancestor rev syntax
    // (HEAD^, abc1234~3). These are not shell-special at argv level — we
    // pass refs as discrete Command::arg arguments, never through `sh -c`.
    if name.is_empty() || name.starts_with('-') {
        return false;
    }
    if name.contains("..") {
        return false;
    }
    name.chars()
        .all(|c| c.is_alphanumeric() || "._/-^~".contains(c))
}

pub(crate) fn is_valid_git_oid(s: &str) -> bool {
    // Accepts 4–40 hex chars (matches `git`'s short-OID accept range; SHA-1 is 40).
    let len = s.len();
    if !(4..=40).contains(&len) {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

#[tauri::command]
pub(crate) async fn git_merge_branch(
    path: String,
    branch_name: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    if !is_valid_git_ref(&branch_name) {
        return Err("Invalid branch name".into());
    }
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        // Merge can block on network I/O if the user's git config pulls on merge
        // or the operation touches a remote tracking ref. Route it through
        // run_git_cancellable so cancel_git_op can kill it.
        run_git_cancellable(&["merge", &branch_name], &path, &slots, &counter, &label)
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct CherryPickResult {
    pub(crate) ok: bool,
    pub(crate) conflicted_files: Vec<String>,
}

// List paths in working tree marked as conflicted by libgit2's status walk.
pub(crate) fn list_conflict_files(path: &str) -> Vec<String> {
    match get_git_status_inner(path) {
        Ok(info) => info.files.into_iter()
            .filter(|f| f.status == "conflict")
            .map(|f| f.path)
            .collect(),
        Err(_) => Vec::new(),
    }
}

// Path to `.git` for `path`. Repository::path() includes the trailing slash.
pub(crate) fn git_dir(path: &str) -> Result<std::path::PathBuf, String> {
    let repo = Repository::discover(path).map_err(|e| e.to_string())?;
    Ok(repo.path().to_path_buf())
}

#[tauri::command]
pub(crate) async fn git_cherry_pick(
    path: String,
    oid: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<CherryPickResult, String> {
    if !is_valid_git_oid(&oid) {
        return Err("Invalid commit OID".into());
    }
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_cancellable(
            &["cherry-pick", &oid],
            &path,
            &slots,
            &counter,
            &label,
        );
        match result {
            Ok(_) => Ok(CherryPickResult { ok: true, conflicted_files: Vec::new() }),
            Err(stderr) => {
                // Non-zero exit could mean conflict (state file present) OR a
                // real failure (bad oid, repo state, etc.). Distinguish via
                // CHERRY_PICK_HEAD so the frontend can route to the conflict UI.
                let cp_head = git_dir(&path)?.join("CHERRY_PICK_HEAD");
                if cp_head.exists() {
                    Ok(CherryPickResult {
                        ok: false,
                        conflicted_files: list_conflict_files(&path),
                    })
                } else {
                    Err(stderr)
                }
            }
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_cherry_pick_abort(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["cherry-pick", "--abort"], &path, &slots, &counter, &label)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_cherry_pick_continue(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["cherry-pick", "--continue"], &path, &slots, &counter, &label)
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct PendingOpState {
    pub(crate) kind: String,
    pub(crate) current_step: Option<u32>,
    pub(crate) total_steps: Option<u32>,
    pub(crate) head_message: Option<String>,
}

// Single libgit2-driven query the frontend polls each cycle. Detects the
// three rebase-family operations that leave state files on disk:
//   merge        → MERGE_HEAD
//   cherry-pick  → CHERRY_PICK_HEAD
//   rebase       → rebase-merge/ or rebase-apply/ directory
// Returns "none" when the working tree is clean of pending state. The
// frontend banner is the only consumer.
#[tauri::command]
pub(crate) async fn get_pending_op_state(path: String) -> Result<PendingOpState, String> {
    blocking(move || {
        let none = PendingOpState {
            kind: "none".into(),
            current_step: None,
            total_steps: None,
            head_message: None,
        };
        let gd = match git_dir(&path) {
            Ok(d) => d,
            Err(_) => return Ok(none),
        };

        let head_message = (|| -> Option<String> {
            let repo = Repository::discover(&path).ok()?;
            let head = repo.head().ok()?;
            let commit = head.peel_to_commit().ok()?;
            commit.summary().map(|s| s.to_string())
        })();

        if gd.join("MERGE_HEAD").exists() {
            return Ok(PendingOpState {
                kind: "merge".into(),
                current_step: None,
                total_steps: None,
                head_message,
            });
        }

        if gd.join("CHERRY_PICK_HEAD").exists() {
            return Ok(PendingOpState {
                kind: "cherry_pick".into(),
                current_step: None,
                total_steps: None,
                head_message,
            });
        }

        let rebase_dir = if gd.join("rebase-merge").is_dir() {
            Some(gd.join("rebase-merge"))
        } else if gd.join("rebase-apply").is_dir() {
            Some(gd.join("rebase-apply"))
        } else {
            None
        };

        if let Some(rd) = rebase_dir {
            // rebase-merge/msgnum + end give "interactive rebase" progress.
            // rebase-apply/next + last give "git am"-style rebase progress.
            // Either layout, we read the two text files and parse u32.
            let read_num = |name: &str| -> Option<u32> {
                std::fs::read_to_string(rd.join(name))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
            };
            let (current_step, total_steps) = if rd.file_name() == Some(std::ffi::OsStr::new("rebase-merge")) {
                (read_num("msgnum"), read_num("end"))
            } else {
                (read_num("next"), read_num("last"))
            };
            return Ok(PendingOpState {
                kind: "rebase".into(),
                current_step,
                total_steps,
                head_message,
            });
        }

        Ok(none)
    })
    .await
}

// ═══════════════════════════════════════════════════════════════════════════
// Interactive rebase (PR4 / Phase 5a)
//
// Driver pattern: we generate two tiny Python scripts in a per-run temp dir
// and point GIT_SEQUENCE_EDITOR / GIT_EDITOR at them. They read the user's
// drag-reordered todo list (state.json) and the per-step counter from the
// state dir, no argv plumbing through git. The state dir + a pre-rebase
// backup tag survive conflict pauses; cleanup is gated on a TERMINAL op
// (clean completion / abort / final continue or skip), not on the initial
// apply returning. See specs/git-features-plan.md Phase 5 for the rationale.
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize)]
pub(crate) struct RebaseCandidateCommit {
    pub(crate) oid: String,
    pub(crate) short_oid: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) on_remote: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct RebaseCandidates {
    pub(crate) commits: Vec<RebaseCandidateCommit>,
    pub(crate) upstream_known: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct RebaseTodoEntry {
    pub(crate) action: String, // "pick" | "reword" | "squash" | "fixup" | "drop" | "edit"
    pub(crate) oid: String,
    pub(crate) new_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct RebaseResult {
    pub(crate) ok: bool,
    pub(crate) stopped_at: Option<String>,
    pub(crate) conflicted_files: Vec<String>,
    pub(crate) completed: bool,
    pub(crate) backup_tag: String,
}

pub(crate) const VALID_REBASE_ACTIONS: &[&str] = &["pick", "reword", "squash", "fixup", "drop", "edit"];

#[tauri::command]
pub(crate) async fn get_rebase_candidate_commits(
    path: String,
    count: usize,
) -> Result<RebaseCandidates, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        if matches!(repo.head_detached(), Ok(true)) {
            // Use a recognizable typed prefix so the frontend can show a useful
            // error toast without parsing free-form text.
            return Err("detached_head".into());
        }

        let head = repo.head().map_err(|e| format!("HEAD: {}", e))?;
        let head_oid = head
            .target()
            .ok_or_else(|| "HEAD has no target".to_string())?;

        // Determine the upstream cutoff: walk forward from `head` until we hit
        // a commit reachable from the upstream's tip. That commit (and earlier)
        // are "shared", so we don't include them in the rebase candidate list.
        let upstream_oid = (|| -> Option<git2::Oid> {
            let head_branch_name = head.shorthand()?;
            let local = repo.find_branch(head_branch_name, BranchType::Local).ok()?;
            let upstream = local.upstream().ok()?;
            upstream.get().target()
        })();

        let upstream_known = upstream_oid.is_some();

        // Build an "on any remote ref" set so we can flag commits as on_remote.
        // For typical repos this is a small set; we materialize it once.
        let mut remote_tips: Vec<git2::Oid> = Vec::new();
        if let Ok(refs) = repo.references_glob("refs/remotes/*") {
            for r in refs.flatten() {
                if let Some(t) = r.target() {
                    remote_tips.push(t);
                }
            }
        }

        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.push(head_oid).map_err(|e| e.to_string())?;

        let mut commits: Vec<RebaseCandidateCommit> = Vec::new();
        for oid_res in walk.take(count) {
            let oid = match oid_res {
                Ok(o) => o,
                Err(_) => break,
            };
            // Stop walking once we hit the upstream-shared frontier — anything
            // beyond is published history we shouldn't be rewriting silently.
            if let Some(up) = upstream_oid {
                if oid == up {
                    break;
                }
                if matches!(repo.graph_descendant_of(up, oid), Ok(true)) {
                    break;
                }
            }

            let on_remote = remote_tips.iter().any(|tip| {
                *tip == oid || matches!(repo.graph_descendant_of(*tip, oid), Ok(true))
            });

            let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
            commits.push(RebaseCandidateCommit {
                oid: oid.to_string(),
                short_oid: oid.to_string()[..7].to_string(),
                message: commit.summary().unwrap_or("(no message)").to_string(),
                author: commit.author().name().unwrap_or("Unknown").to_string(),
                timestamp: commit.time().seconds(),
                on_remote,
            });
        }

        Ok(RebaseCandidates { commits, upstream_known })
    })
    .await
}

// Lightweight tag at HEAD before the rebase mutates anything. Tag name is
// timestamp-based so two consecutive rebases don't collide.
pub(crate) fn create_rebase_backup_tag(repo: &Repository) -> Result<String, String> {
    let head_oid = repo
        .head()
        .map_err(|e| format!("HEAD: {}", e))?
        .target()
        .ok_or_else(|| "HEAD has no target".to_string())?;
    let target = repo.find_object(head_oid, None).map_err(|e| e.to_string())?;
    // Nanos avoid the within-same-second collision two consecutive rebases
    // would otherwise hit (tag_lightweight refuses to overwrite without
    // force=true, surfacing as a confusing "backup tag" error).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tag_name = format!("launchpad/pre-rebase/{}", nanos);
    repo.tag_lightweight(&tag_name, &target, false)
        .map_err(|e| format!("backup tag: {}", e))?;
    Ok(tag_name)
}

pub(crate) fn delete_rebase_backup_tag(repo_path: &str, tag_name: &str) {
    if let Ok(repo) = Repository::discover(repo_path) {
        let _ = repo.tag_delete(tag_name);
    }
}

// Generate the per-run temp dir holding state.json + the two editor scripts.
// Uses tempfile::Builder so the directory name has a secure random suffix
// (no TOCTOU window where a local attacker can pre-create the path) and is
// created atomically. Directory mode is set to 0o700 immediately after
// creation; state.json is written at 0o600 so other users on shared
// machines can't read commit-message contents.
pub(crate) fn create_rebase_state_dir(todo: &[RebaseTodoEntry]) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    // tempfile::Builder uses O_EXCL with a securely random suffix — no
    // pre-creation race possible. .into_path() detaches the cleanup guard
    // because we need the directory to outlive this function (cleanup is
    // owned by the rebase command lifecycle in run_git_with_rebase_env's
    // callers; see finalize_rebase).
    let temp = tempfile::Builder::new()
        .prefix("launchpad-rebase-")
        .tempdir()
        .map_err(|e| format!("create state dir: {}", e))?;
    let dir = temp.into_path();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod state dir: {}", e))?;

    let state_json = serde_json::json!({
        "version": 1,
        "todo": todo,
    });
    let state_path = dir.join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_string(&state_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write state.json: {}", e))?;
    std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod state.json: {}", e))?;

    // seq_editor.sh — git invokes this once with $1 = path to the rebase-todo
    // file. It reads our state.json and rewrites $1 in our drag-reordered
    // order, dropping `drop` entries.
    let seq_script = r#"#!/usr/bin/env python3
import json, os, sys
state_dir = os.environ["LAUNCHPAD_REBASE_STATE_DIR"]
with open(os.path.join(state_dir, "state.json")) as f:
    state = json.load(f)
todo_path = sys.argv[1]
with open(todo_path, "w") as out:
    for entry in state["todo"]:
        action = entry["action"]
        if action == "drop":
            continue
        out.write(f"{action} {entry['oid']}\n")
"#;
    let seq_path = dir.join("seq_editor.sh");
    std::fs::write(&seq_path, seq_script).map_err(|e| format!("write seq_editor.sh: {}", e))?;
    std::fs::set_permissions(&seq_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod seq_editor.sh: {}", e))?;

    // commit_editor.sh — git invokes this once per reword/squash entry with
    // $1 = path to the COMMIT_EDITMSG file. We track which reword/squash
    // we're on via a counter file and write the matching new_message (or
    // leave the file untouched when new_message is null/missing). `edit`
    // does NOT invoke this — git pauses for shell on `edit` and the user
    // resumes via Continue from our UI.
    let commit_script = r#"#!/usr/bin/env python3
import json, os, sys, fcntl
state_dir = os.environ["LAUNCHPAD_REBASE_STATE_DIR"]
with open(os.path.join(state_dir, "state.json")) as f:
    state = json.load(f)

counter_path = os.path.join(state_dir, "commit_editor.counter")
# Atomic open-or-create + flock-protected increment.
fd = os.open(counter_path, os.O_RDWR | os.O_CREAT, 0o600)
try:
    fcntl.flock(fd, fcntl.LOCK_EX)
    raw = os.read(fd, 64).decode().strip()
    n = int(raw) if raw else 0
    os.lseek(fd, 0, os.SEEK_SET)
    os.ftruncate(fd, 0)
    os.write(fd, str(n + 1).encode())
finally:
    fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)

target = None
seen = 0
for entry in state["todo"]:
    if entry["action"] in ("reword", "squash"):
        if seen == n:
            target = entry
            break
        seen += 1

msg_path = sys.argv[1]
if target and target.get("new_message"):
    with open(msg_path, "w") as f:
        f.write(target["new_message"])
        if not target["new_message"].endswith("\n"):
            f.write("\n")
# else: leave file untouched — preserves existing message (git's default)
"#;
    let commit_path = dir.join("commit_editor.sh");
    std::fs::write(&commit_path, commit_script)
        .map_err(|e| format!("write commit_editor.sh: {}", e))?;
    std::fs::set_permissions(&commit_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod commit_editor.sh: {}", e))?;

    Ok(dir)
}

pub(crate) fn cleanup_rebase_state_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// True when git's rebase state files indicate the rebase has finished
// (state files removed). We poll this after each command result to decide
// whether to clean up the state dir + (optionally) the backup tag.
pub(crate) fn rebase_in_progress(repo_path: &str) -> bool {
    let gd = match git_dir(repo_path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    gd.join("rebase-merge").is_dir() || gd.join("rebase-apply").is_dir()
}

#[tauri::command]
pub(crate) async fn git_rebase_interactive_apply(
    path: String,
    base_oid: String,
    todo: Vec<RebaseTodoEntry>,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    // Accept either a literal OID or a rev expression (HEAD^, abc1234~2, a
    // branch name). Validate via the ref/oid grammar (so shell metachars
    // stay out), then revparse_single below to a concrete OID before
    // shelling out. This lets the frontend pass `<oid>^` for "rebase from
    // here" without needing a separate parent-resolution IPC.
    if !is_valid_git_ref(&base_oid) && !is_valid_git_oid(&base_oid) {
        return Err("Invalid base ref".into());
    }
    for entry in &todo {
        if !VALID_REBASE_ACTIONS.contains(&entry.action.as_str()) {
            return Err(format!("Invalid action: {}", entry.action));
        }
        if !is_valid_git_oid(&entry.oid) {
            return Err(format!("Invalid commit oid: {}", entry.oid));
        }
    }

    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();

    blocking(move || {
        // Resolve the (possibly rev-expression) base to a concrete OID up front
        // — both for cleaner argv to `git rebase -i` and so the frontend gets
        // a real OID back in `stopped_at` if anything goes wrong.
        let resolved_base = {
            let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
            // Compute the OID inside the same scope as `repo` so the borrow
            // graph (obj → repo) stays valid; only the resulting String escapes.
            let oid = repo
                .revparse_single(&base_oid)
                .map_err(|e| format!("Resolve `{}`: {}", base_oid, e))?
                .peel_to_commit()
                .map_err(|e| format!("`{}` does not resolve to a commit: {}", base_oid, e))?
                .id();
            oid.to_string()
        };

        // Hold the rebase_state lock across the in-progress check + tag/dir
        // creation + register, so two near-simultaneous IPC calls can't both
        // pass the in-progress check and clobber each other's state. Also
        // refuses if our own AppState already has a rebase registered (i.e.
        // an earlier attempt is paused mid-conflict).
        let (backup_tag, state_dir) = {
            let mut guard = rebase_state.lock_safe();
            if guard.is_some() || rebase_in_progress(&path) {
                return Err("A rebase is already in progress — abort or continue it before starting a new one".into());
            }

            let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
            let backup_tag = create_rebase_backup_tag(&repo)?;
            drop(repo);

            let state_dir = match create_rebase_state_dir(&todo) {
                Ok(d) => d,
                Err(e) => {
                    // Tag was created but we never spawned — drop it so we
                    // don't leave noise behind.
                    delete_rebase_backup_tag(&path, &backup_tag);
                    return Err(e);
                }
            };
            *guard = Some(RebaseStateInfo {
                state_dir: state_dir.clone(),
                backup_tag: backup_tag.clone(),
            });
            (backup_tag, state_dir)
        };

        let result = spawn_rebase(&path, &resolved_base, &state_dir, &slots, &counter, &label);

        // Inspect git's repo state to decide whether this was a terminal result
        // (rebase finished) or a pause (state files still on disk). Three cases
        // matter: (1) rebase still paused → conflict-or-edit pause; (2) finished
        // with Ok → clean completion; (3) finished with Err → terminal failure.
        let rebase_paused = rebase_in_progress(&path);

        if rebase_paused {
            // Conflict (or `edit`) pause — leave state dir + tag intact for
            // continue/abort to consume. Surface the actual stopped commit so
            // the frontend can highlight it.
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }

        match result {
            Ok(_) => {
                // Clean completion — drop the state dir, KEEP the backup tag
                // (user-visible safety net surfaced in the success toast).
                cleanup_rebase_state_dir(&state_dir);
                *rebase_state.lock_safe() = None;
                Ok(RebaseResult {
                    ok: true,
                    stopped_at: None,
                    conflicted_files: Vec::new(),
                    completed: true,
                    backup_tag,
                })
            }
            Err(err) => {
                // Terminal failure — clean up everything we created.
                cleanup_rebase_state_dir(&state_dir);
                delete_rebase_backup_tag(&path, &backup_tag);
                *rebase_state.lock_safe() = None;
                Err(err)
            }
        }
    })
    .await
}

// During a rebase pause, .git/rebase-merge/stopped-sha contains the OID of
// the commit that caused the stop (conflict or `edit`). Best-effort: a
// missing or unparseable file just yields None, which the frontend tolerates.
pub(crate) fn read_rebase_stopped_sha(repo_path: &str) -> Option<String> {
    let gd = git_dir(repo_path).ok()?;
    let candidates = [
        gd.join("rebase-merge").join("stopped-sha"),
        gd.join("rebase-apply").join("stopped-sha"),
    ];
    for p in &candidates {
        if let Ok(s) = std::fs::read_to_string(p) {
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

pub(crate) fn spawn_rebase(
    path: &str,
    base_oid: &str,
    state_dir: &std::path::Path,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    label: &str,
) -> Result<String, String> {
    // We can't reuse run_git_cancellable directly — it doesn't take env
    // overrides. Instead replicate its slot reservation pattern around a
    // bespoke spawn that sets the editor envs.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = (|| -> Result<String, String> {
        let mut command = std::process::Command::new("git");
        command
            .args(["rebase", "-i", base_oid])
            .current_dir(path)
            .env(
                "GIT_SEQUENCE_EDITOR",
                state_dir.join("seq_editor.sh"),
            )
            .env("GIT_EDITOR", state_dir.join("commit_editor.sh"))
            .env("LAUNCHPAD_REBASE_STATE_DIR", state_dir)
            // Neutralize user wrappers — GIT_*_EDITOR take precedence over
            // EDITOR/VISUAL/core.editor in git's resolution order, but
            // clear EDITOR/VISUAL too just in case a user wrapper script
            // ignores our overrides.
            .env_remove("EDITOR")
            .env_remove("VISUAL")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        apply_git_env(&mut command);

        let child = command.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();

        let kill_now = {
            let mut guard = slots.lock_safe();
            match guard.get_mut(label) {
                Some(s) if s.op_id == op_id => {
                    s.pid = Some(pid);
                    s.cancelled
                }
                _ => false,
            }
        };
        if kill_now {
            kill_pid_hard(pid);
        }

        let output = child.wait_with_output().map_err(|e| e.to_string())?;

        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get_mut(label) {
                if s.op_id == op_id {
                    s.pid = None;
                }
            }
        }

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    })();

    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get(label) {
            if s.op_id == op_id {
                guard.remove(label);
            }
        }
    }

    result
}

// Common cleanup logic for terminal results from continue/skip/abort.
// `delete_tag` controls whether the backup tag is also removed (true on
// abort; false on clean continue/skip — the tag is the user's safety net).
pub(crate) fn finalize_rebase(rebase_state: &Arc<Mutex<Option<RebaseStateInfo>>>, repo_path: &str, delete_tag: bool) {
    let info = rebase_state.lock_safe().take();
    if let Some(info) = info {
        cleanup_rebase_state_dir(&info.state_dir);
        if delete_tag {
            delete_rebase_backup_tag(repo_path, &info.backup_tag);
        }
    }
}

pub(crate) fn run_git_with_rebase_env(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    rebase_state: &Arc<Mutex<Option<RebaseStateInfo>>>,
    label: &str,
) -> Result<String, String> {
    // Rebase --continue/--skip may invoke commit_editor.sh again (for the
    // next reword/squash entry), so we have to keep the env vars set.
    // Replicate run_git_cancellable's slot pattern with the env overrides.
    let info = rebase_state.lock_safe().clone();
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = (|| -> Result<String, String> {
        let mut command = std::process::Command::new("git");
        command
            .args(args)
            .current_dir(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(info) = info.as_ref() {
            command
                .env("GIT_SEQUENCE_EDITOR", info.state_dir.join("seq_editor.sh"))
                .env("GIT_EDITOR", info.state_dir.join("commit_editor.sh"))
                .env("LAUNCHPAD_REBASE_STATE_DIR", &info.state_dir)
                .env_remove("EDITOR")
                .env_remove("VISUAL");
        }
        apply_git_env(&mut command);

        let child = command.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();
        let kill_now = {
            let mut guard = slots.lock_safe();
            match guard.get_mut(label) {
                Some(s) if s.op_id == op_id => {
                    s.pid = Some(pid);
                    s.cancelled
                }
                _ => false,
            }
        };
        if kill_now {
            kill_pid_hard(pid);
        }

        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get_mut(label) {
                if s.op_id == op_id {
                    s.pid = None;
                }
            }
        }

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    })();

    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get(label) {
            if s.op_id == op_id {
                guard.remove(label);
            }
        }
    }

    result
}

#[tauri::command]
pub(crate) async fn git_rebase_continue(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--continue"], &path, &slots, &counter, &rebase_state, &label);
        let still_paused = rebase_in_progress(&path);
        // rebase_state is intentionally read BEFORE finalize_rebase runs:
        // run_git_with_rebase_env doesn't touch it, so backup_tag is still
        // Some(_) here whether the rebase paused again or completed. The
        // finalize_rebase call below is what clears it on terminal results.
        let backup_tag = rebase_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.backup_tag.clone())
            .unwrap_or_default();

        if still_paused {
            // Another conflict — keep state dir + tag.
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }

        // Terminal: rebase finished. Clean up state dir; keep tag on success.
        let ok = result.is_ok();
        finalize_rebase(&rebase_state, &path, /*delete_tag=*/ false);
        if !ok {
            let err = result.err().unwrap_or_else(|| "Rebase continue failed".into());
            return Err(err);
        }
        Ok(RebaseResult {
            ok: true,
            stopped_at: None,
            conflicted_files: Vec::new(),
            completed: true,
            backup_tag,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_rebase_skip(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--skip"], &path, &slots, &counter, &rebase_state, &label);
        let still_paused = rebase_in_progress(&path);
        let backup_tag = rebase_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.backup_tag.clone())
            .unwrap_or_default();

        if still_paused {
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }
        let ok = result.is_ok();
        finalize_rebase(&rebase_state, &path, /*delete_tag=*/ false);
        if !ok {
            return Err(result.err().unwrap_or_else(|| "Rebase skip failed".into()));
        }
        Ok(RebaseResult {
            ok: true,
            stopped_at: None,
            conflicted_files: Vec::new(),
            completed: true,
            backup_tag,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_rebase_abort(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--abort"], &path, &slots, &counter, &rebase_state, &label);
        // Abort always cleans up — drop the tag too. Even on abort failure we
        // still try to drop the state dir (it's our temp resource), but leave
        // the tag in place so the user can recover manually if abort failed.
        if result.is_ok() {
            finalize_rebase(&rebase_state, &path, /*delete_tag=*/ true);
        } else {
            // Best-effort: clean the state dir without removing the tag.
            let info = rebase_state.lock_safe().take();
            if let Some(info) = info {
                cleanup_rebase_state_dir(&info.state_dir);
            }
        }
        result
    })
    .await
}

// ═══════════════════════════════════════════════════════════════════════════
// End interactive rebase block
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize)]
pub(crate) struct ConflictVersions {
    // Each version is None when the corresponding stage is missing (e.g.
    // a file added on both sides has no base; deleted-by-us has no ours).
    // Working tree's `merged` is None when the file isn't on disk.
    pub(crate) base: Option<String>,
    pub(crate) ours: Option<String>,
    pub(crate) theirs: Option<String>,
    pub(crate) merged: Option<String>,
}

// True when `p` is a repo-relative path that stays inside the repo: not
// absolute and with no `..` components. libgit2 always hands us such paths,
// but get_conflict_versions is IPC-reachable with arbitrary args, so we guard
// the working-tree read against `../../etc/passwd`-style escapes before
// joining onto the repo root.
pub(crate) fn is_safe_relative_path(p: &str) -> bool {
    use std::path::Component;
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

// Reads index stages 1 (base), 2 (ours), 3 (theirs) for a conflicted file
// and the working-tree contents (`merged`). Used by the 3-pane merge tab.
// Stages encode their role in libgit2's index: 0 = normal, 1 = base,
// 2 = ours, 3 = theirs. A file may be missing any of 1/2/3 depending on
// which side added/deleted it.
#[tauri::command]
pub(crate) async fn get_conflict_versions(path: String, file_path: String) -> Result<ConflictVersions, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let index = repo.index().map_err(|e| e.to_string())?;

        let path_bytes = file_path.as_bytes();
        let mut base = None;
        let mut ours = None;
        let mut theirs = None;

        for entry in index.iter() {
            if entry.path != path_bytes {
                continue;
            }
            // libgit2 packs stage in bits 12-13 of `flags`: GIT_IDXENTRY_STAGEMASK
            // = 0x3000, shift = 12. Stage 0 = normal, 1 = base, 2 = ours, 3 = theirs.
            let stage = ((entry.flags & 0x3000) >> 12) as i32;
            let blob = match repo.find_blob(entry.id) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Blob content may not be UTF-8 (binary files, mixed encodings);
            // for the merge UI we surface the lossy conversion so the user
            // gets *something* instead of an opaque error. The 3-pane editor
            // is text-only by design.
            let content = String::from_utf8_lossy(blob.content()).into_owned();
            match stage {
                1 => base = Some(content),
                2 => ours = Some(content),
                3 => theirs = Some(content),
                _ => {}
            }
        }

        // Only read the working tree for a contained relative path. A traversal
        // or absolute file_path would have matched no index entry above (libgit2
        // stores normalized repo-relative paths), so base/ours/theirs are
        // already None; we just refuse to leak an out-of-repo file as `merged`.
        let merged = if is_safe_relative_path(&file_path) {
            let abs_path = std::path::Path::new(&path).join(&file_path);
            std::fs::read_to_string(&abs_path).ok()
        } else {
            None
        };

        Ok(ConflictVersions { base, ours, theirs, merged })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_resolve_ours(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let mut checkout = std::process::Command::new("git");
        checkout
            .args(["checkout", "--ours", "--", &file_path])
            .current_dir(&path);
        apply_git_env(&mut checkout);
        let status = checkout.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to checkout --ours".into());
        }
        let mut add = std::process::Command::new("git");
        add.args(["add", "--", &file_path]).current_dir(&path);
        apply_git_env(&mut add);
        let status = add.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to stage resolved file".into());
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_resolve_theirs(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let mut checkout = std::process::Command::new("git");
        checkout
            .args(["checkout", "--theirs", "--", &file_path])
            .current_dir(&path);
        apply_git_env(&mut checkout);
        let status = checkout.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to checkout --theirs".into());
        }
        let mut add = std::process::Command::new("git");
        add.args(["add", "--", &file_path]).current_dir(&path);
        apply_git_env(&mut add);
        let status = add.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to stage resolved file".into());
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_unstage_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        match repo.head() {
            Ok(head) => {
                let head_commit = head.peel_to_commit().map_err(|e| e.to_string())?;
                let obj = head_commit.as_object();
                repo.reset_default(Some(obj), [file_path.as_str()])
                    .map_err(|e| e.to_string())?;
            }
            Err(_) => {
                // Unborn HEAD (no commits yet) — remove from index directly
                let mut index = repo.index().map_err(|e| e.to_string())?;
                index.remove_path(std::path::Path::new(&file_path)).map_err(|e| e.to_string())?;
                index.write().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_unstage_all(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        match repo.head() {
            Ok(head) => {
                let head_commit = head.peel_to_commit().map_err(|e| e.to_string())?;
                let obj = head_commit.as_object();
                repo.reset_default(Some(obj), ["*"])
                    .map_err(|e| e.to_string())?;
            }
            Err(_) => {
                // Unborn HEAD (no commits yet) — clear the entire index
                let mut index = repo.index().map_err(|e| e.to_string())?;
                index.clear().map_err(|e| e.to_string())?;
                index.write().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_discard_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let workdir = repo.workdir().ok_or("Repository has no workdir")?;
        let abs = workdir.join(&file_path);

        // Two cases route to "delete from disk":
        //   1) status_file says wt_new (untracked file at the top level).
        //   2) status_file errors with GIT_ENOTFOUND because the path lives
        //      inside an untracked directory — git reports the directory,
        //      not its contents, so status_file can't see the child. Fall
        //      back to a presence check on disk, gated on the NotFound
        //      error code so we don't misclassify other libgit2 errors.
        let status_result = repo.status_file(std::path::Path::new(&file_path));
        let treat_as_untracked = match &status_result {
            Ok(s) => s.is_wt_new(),
            Err(e) if e.code() == git2::ErrorCode::NotFound => abs.exists(),
            Err(_) => false,
        };

        if treat_as_untracked {
            let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&abs).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(&abs).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }

        // Surface any non-NotFound status_file error rather than silently
        // falling through to checkout_head, which would just fail again.
        status_result.map_err(|e| e.to_string())?;

        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.path(&file_path).force();
        repo.checkout_head(Some(&mut checkout))
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_save(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let sig = repo.signature().map_err(|e| e.to_string())?;
        // stash_save requires &mut Repository
        let mut repo = repo;
        repo.stash_save(&sig, "Launchpad stash", None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_pop(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        repo.stash_pop(0, None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct StashEntry {
    pub(crate) index: usize,
    pub(crate) message: String,
}

#[tauri::command]
pub(crate) async fn git_stash_list(path: String) -> Result<Vec<StashEntry>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        let mut entries = Vec::new();
        repo.stash_foreach(|index, message, _oid| {
            entries.push(StashEntry {
                index,
                message: message.to_string(),
            });
            true
        }).map_err(|e| e.to_string())?;
        Ok(entries)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_apply(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let mut repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        repo.stash_apply(index, None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_drop(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        repo.stash_drop(index)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_delete_branch(path: String, branch_name: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .map_err(|e| e.to_string())?;
        branch.delete().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_remote_branches(path: String) -> Result<Vec<BranchInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let branches = repo
            .branches(Some(BranchType::Remote))
            .map_err(|e| e.to_string())?;

        let result: Vec<BranchInfo> = branches
            .filter_map(|b| {
                let (branch, _) = b.ok()?;
                let full_name = branch.name().ok()??.to_string();
                // Skip HEAD pointer
                if full_name.ends_with("/HEAD") {
                    return None;
                }
                // Strip "origin/" prefix for display
                let name = full_name
                    .strip_prefix("origin/")
                    .unwrap_or(&full_name)
                    .to_string();

                let commit = branch.get().peel_to_commit().ok();
                let last_commit_msg = commit
                    .as_ref()
                    .and_then(|c| c.summary().map(String::from));
                let last_commit_time = commit.as_ref().map(|c| c.time().seconds());

                Some(BranchInfo {
                    name,
                    is_current: false,
                    last_commit_msg,
                    last_commit_time,
                    upstream: None,
                })
            })
            .collect();

        Ok(result)
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_commit_details(path: String, oid: String) -> Result<CommitDetail, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let obj = repo.revparse_single(&oid).map_err(|e| e.to_string())?;
        let commit = obj.peel_to_commit().map_err(|e| e.to_string())?;

        let tree = commit.tree().map_err(|e| e.to_string())?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

        let diff = repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
            .map_err(|e| e.to_string())?;

        // First pass: build file list from deltas
        let mut files: Vec<CommitFileStat> = Vec::new();
        for idx in 0..diff.deltas().len() {
            let delta = diff.get_delta(idx).unwrap();
            let file_path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let status = match delta.status() {
                git2::Delta::Added => "added",
                git2::Delta::Deleted => "deleted",
                git2::Delta::Modified => "modified",
                git2::Delta::Renamed => "renamed",
                git2::Delta::Copied => "copied",
                _ => "modified",
            };
            files.push(CommitFileStat {
                path: file_path,
                additions: 0,
                deletions: 0,
                status: status.to_string(),
            });
        }

        // Second pass: count additions/deletions per file via Patch
        let num_deltas = diff.deltas().len();
        for i in 0..num_deltas {
            if let Ok(patch) = git2::Patch::from_diff(&diff, i) {
                if let Some(patch) = patch {
                    let (_, additions, deletions) = patch.line_stats().unwrap_or((0, 0, 0));
                    files[i].additions = additions;
                    files[i].deletions = deletions;
                }
            }
        }

        let message = commit.summary().unwrap_or("(no message)").to_string();
        let author = commit.author().name().unwrap_or("Unknown").to_string();
        let timestamp = commit.time().seconds();
        let short_oid = commit.id().to_string()[..7].to_string();

        Ok(CommitDetail {
            oid: short_oid,
            message,
            author,
            timestamp,
            files,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_remote_url(path: String) -> Result<String, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let remote = repo.find_remote("origin").map_err(|e| e.to_string())?;
        let url = remote.url().ok_or("No URL for origin")?.to_string();
        Ok(url)
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct CommitFileStat {
    pub(crate) path: String,
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
    pub(crate) status: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct CommitDetail {
    pub(crate) oid: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) files: Vec<CommitFileStat>,
}
