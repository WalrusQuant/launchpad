use crate::*;
use git2::BranchType;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct BranchInfo {
    pub(crate) name: String,
    pub(crate) is_current: bool,
    pub(crate) last_commit_msg: Option<String>,
    pub(crate) last_commit_time: Option<i64>,
    pub(crate) upstream: Option<String>,
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
