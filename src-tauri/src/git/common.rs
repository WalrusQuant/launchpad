use crate::*;

// Compute the project's location relative to the repo workdir as a
// "/"-terminated prefix, or "" when the project is the workdir root (or the
// relationship can't be resolved). Both paths are canonicalized so a symlinked
// project alias doesn't defeat the strip_prefix.
pub(crate) fn repo_subdir_prefix(repo: &Repository, path: &str) -> String {
    let Some(workdir) = repo.workdir() else {
        return String::new();
    };
    let (Ok(wd), Ok(proj)) = (
        std::fs::canonicalize(workdir),
        std::fs::canonicalize(path),
    ) else {
        return String::new();
    };
    match proj.strip_prefix(&wd) {
        Ok(rel) if !rel.as_os_str().is_empty() => {
            format!("{}/", rel.to_string_lossy().replace('\\', "/").trim_end_matches('/'))
        }
        _ => String::new(),
    }
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

// Path to `.git` for `path`. Repository::path() includes the trailing slash.
pub(crate) fn git_dir(path: &str) -> Result<std::path::PathBuf, String> {
    let repo = Repository::discover(path).map_err(|e| e.to_string())?;
    Ok(repo.path().to_path_buf())
}
