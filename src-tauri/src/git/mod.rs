// Git: status, branches, commits, diffs, staging, commit/amend, stash, discard,
// conflict resolution, the cancellable network-op machinery (push/pull/fetch/
// merge), cherry-pick, and interactive rebase. Local operations use the git2
// (libgit2) crate; network operations shell out to system `git` so the user's
// SSH keys and credential helpers apply. Heavier calls run off the IPC thread
// via `blocking`.
// Re-exported at the crate root (via `pub(crate) use git::*`) so the test
// module — which does `use super::*` and constructs `Repository` directly —
// keeps the same crate-root name.
pub(crate) use git2::Repository;

mod common;
mod status;
mod branches;
mod commits;
mod diff;
mod staging;
mod stash;
mod network;
mod conflicts;
mod cherrypick;
mod pending;
mod rebase;
mod blame;

pub(crate) use common::*;
pub(crate) use status::*;
pub(crate) use branches::*;
pub(crate) use commits::*;
pub(crate) use diff::*;
pub(crate) use staging::*;
pub(crate) use stash::*;
pub(crate) use network::*;
pub(crate) use conflicts::*;
pub(crate) use cherrypick::*;
pub(crate) use pending::*;
pub(crate) use rebase::*;
pub(crate) use blame::*;
