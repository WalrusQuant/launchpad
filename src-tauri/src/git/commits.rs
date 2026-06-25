use crate::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct CommitInfo {
    pub(crate) oid: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) parent_count: usize,
}

// One-shot summary for the project picker card: does the folder still exist on
// disk, is it a git repo, what branch is checked out, and what was the last
// commit. Folded into a single command so the picker makes ONE round-trip per
// project instead of three (exists + status + log).
#[derive(Clone, Serialize)]
pub(crate) struct ProjectCardInfo {
    pub(crate) exists: bool,
    pub(crate) is_repo: bool,
    pub(crate) branch: Option<String>,
    pub(crate) last_commit: Option<CommitInfo>,
}

#[tauri::command]
pub(crate) async fn get_project_card(path: String) -> Result<ProjectCardInfo, String> {
    blocking(move || {
        if !std::path::Path::new(&path).exists() {
            return Ok(ProjectCardInfo {
                exists: false,
                is_repo: false,
                branch: None,
                last_commit: None,
            });
        }

        let repo = match Repository::discover(&path) {
            Ok(r) => r,
            Err(_) => {
                return Ok(ProjectCardInfo {
                    exists: true,
                    is_repo: false,
                    branch: None,
                    last_commit: None,
                });
            }
        };

        let branch = repo.head().ok().and_then(|h| h.shorthand().map(String::from));

        let last_commit = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .map(|commit| CommitInfo {
                oid: commit.id().to_string()[..7].to_string(),
                message: commit.summary().unwrap_or("(no message)").to_string(),
                author: commit.author().name().unwrap_or("Unknown").to_string(),
                timestamp: commit.time().seconds(),
                parent_count: commit.parent_count(),
            });

        Ok(ProjectCardInfo {
            exists: true,
            is_repo: true,
            branch,
            last_commit,
        })
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
