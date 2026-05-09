//! Canonical JSON snapshot store for compaction events.
//!
//! Each successful compaction writes a [`CompactionSnapshot`] as a single
//! JSON file under `<root>/snapshots/<session_id>/<turn_id>.json`. The
//! snapshot is the authoritative recovery record: a resumed session must be
//! able to rebuild the same compacted prompt view from it without any git or
//! provider access.
//!
//! This module intentionally owns only the JSON backend. Git-backed ghost
//! snapshots (when enabled) live in a future integration layer and reuse the
//! same [`CompactionSnapshot`] record with a populated `snapshot_backend`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{CompactionSnapshot, SessionId, SnapshotPersistFailure, TurnId};

/// Writes and loads canonical JSON compaction snapshots under a shared root.
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    /// Creates a store rooted at the supplied directory. `root` is typically
    /// `<lpa_home>/snapshots` but callers may pass any writable path.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the canonical JSON path for a snapshot.
    pub fn snapshot_path(&self, session_id: SessionId, turn_id: TurnId) -> PathBuf {
        self.root
            .join(session_id.to_string())
            .join(format!("{turn_id}.json"))
    }

    /// Persists the supplied snapshot as pretty-printed JSON.
    ///
    /// Errors are normalized into [`SnapshotPersistFailure::JsonSnapshotWriteFailed`]
    /// so callers can map directly into [`crate::CompactionError::SnapshotPersistFailed`].
    pub fn persist(
        &self,
        snapshot: &CompactionSnapshot,
    ) -> Result<PathBuf, SnapshotPersistFailure> {
        let path = self.snapshot_path(snapshot.session_id, snapshot.turn_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io_failure("create snapshot directory", err))?;
        }
        let encoded = serde_json::to_vec_pretty(snapshot).map_err(|err| {
            SnapshotPersistFailure::JsonSnapshotWriteFailed {
                message: format!("serialize snapshot: {err}"),
            }
        })?;
        atomic_write(&path, &encoded).map_err(|err| io_failure("write snapshot file", err))?;
        Ok(path)
    }

    /// Loads a previously persisted snapshot by session and turn identifier.
    pub fn load(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<CompactionSnapshot, SnapshotPersistFailure> {
        let path = self.snapshot_path(session_id, turn_id);
        let bytes = fs::read(&path).map_err(|err| io_failure("read snapshot file", err))?;
        serde_json::from_slice(&bytes).map_err(|err| {
            SnapshotPersistFailure::JsonSnapshotWriteFailed {
                message: format!("parse snapshot JSON: {err}"),
            }
        })
    }
}

fn io_failure(context: &str, err: io::Error) -> SnapshotPersistFailure {
    SnapshotPersistFailure::JsonSnapshotWriteFailed {
        message: format!("{context}: {err}"),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ItemId, SnapshotBackendKind, SummaryModelSelection};
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn sample_snapshot() -> CompactionSnapshot {
        CompactionSnapshot {
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            replaced_from_item_id: ItemId::new(),
            replaced_to_item_id: ItemId::new(),
            summary_item_id: ItemId::new(),
            model_slug: "summary-model".into(),
            summary_model_selection: SummaryModelSelection::UseTurnModel,
            prompt_segment_order: vec![ItemId::new(), ItemId::new()],
            workspace_root: Some(PathBuf::from("/workspace")),
            repo_root: None,
            snapshot_backend: SnapshotBackendKind::JsonOnly,
        }
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let tmp = tempdir().expect("tempdir");
        let store = SnapshotStore::new(tmp.path());
        let snapshot = sample_snapshot();

        let path = store.persist(&snapshot).expect("persist");
        assert!(path.exists());
        let loaded = store
            .load(snapshot.session_id, snapshot.turn_id)
            .expect("load");
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn persist_is_atomic_via_rename() {
        let tmp = tempdir().expect("tempdir");
        let store = SnapshotStore::new(tmp.path());
        let snapshot = sample_snapshot();
        let path = store.persist(&snapshot).expect("persist");

        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists(), "temp file should be renamed into place");
    }

    #[test]
    fn snapshot_path_includes_session_and_turn() {
        let tmp = tempdir().expect("tempdir");
        let store = SnapshotStore::new(tmp.path());
        let snapshot = sample_snapshot();
        let path = store.snapshot_path(snapshot.session_id, snapshot.turn_id);
        let path_str = path.to_string_lossy().to_string();
        assert!(path_str.contains(&snapshot.session_id.to_string()));
        assert!(path_str.contains(&snapshot.turn_id.to_string()));
        assert!(path_str.ends_with(".json"));
    }

    #[test]
    fn load_missing_returns_structured_failure() {
        let tmp = tempdir().expect("tempdir");
        let store = SnapshotStore::new(tmp.path());
        let err = store
            .load(SessionId::new(), TurnId::new())
            .expect_err("missing snapshot");
        match err {
            SnapshotPersistFailure::JsonSnapshotWriteFailed { message } => {
                assert!(message.contains("read snapshot file"));
            }
            other => panic!("expected JsonSnapshotWriteFailed, got {other:?}"),
        }
    }
}
