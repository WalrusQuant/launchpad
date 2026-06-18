use crate::*;
use git2::StatusOptions;
use serde::Serialize;

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
    // The project path's location relative to the git workdir, as a
    // "/"-terminated prefix ("" when the project IS the workdir root). git2's
    // status/index paths are workdir-relative, but the frontend anchors DOM
    // paths on the *project* path; when a project is opened as a subdirectory
    // of a larger repo (monorepo, worktree, opening src/ of a repo) the two
    // diverge by exactly this prefix. The frontend prepends it to convert a
    // project-relative path to the workdir-relative key git2 uses.
    pub(crate) subdir: String,
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
                subdir: String::new(),
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
        subdir: repo_subdir_prefix(&repo, path),
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
