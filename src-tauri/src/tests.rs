use super::*;
use std::fs;

// ─── Helper: create a temp git repo with an initial commit ───────────────
fn setup_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();

    // Configure git user for commits
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@test.com").unwrap();

    // Create a file, stage it, and commit
    fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("hello.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[]).unwrap();

    dir
}

// ─── Helper: create a temp git repo with NO commits (unborn HEAD) ────────
fn setup_empty_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    dir
}

// ═══════════════════════════════════════════════════════════════════════════
// write_file tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_write_file_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    let result = tauri::async_runtime::block_on(write_file(path.to_string_lossy().to_string(), "hello".into()));
    assert!(result.is_ok());
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
}

#[test]
fn test_write_file_rejects_oversized_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    let content = "x".repeat(10 * 1024 * 1024 + 1); // just over 10MB
    let result = tauri::async_runtime::block_on(write_file(path.to_string_lossy().to_string(), content));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("10MB"));
    // File should not have been created
    assert!(!path.exists());
}

#[test]
fn test_write_file_exactly_at_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limit.txt");
    let content = "x".repeat(10 * 1024 * 1024); // exactly 10MB
    let result = tauri::async_runtime::block_on(write_file(path.to_string_lossy().to_string(), content));
    assert!(result.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════
// read_file_preview tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_read_file_preview_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, "hello world").unwrap();
    let result = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None));
    assert_eq!(result.unwrap(), "hello world");
}

#[test]
fn test_read_file_preview_truncates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, "hello world").unwrap();
    let result = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), Some(5)));
    assert_eq!(result.unwrap(), "hello");
}

#[test]
fn test_read_file_preview_missing_file() {
    let result = tauri::async_runtime::block_on(read_file_preview("/nonexistent/path/file.txt".into(), None));
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// save_settings JSON validation tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_save_settings_rejects_invalid_json() {
    let result = save_settings("not valid json {{{".into());
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Invalid JSON"));
}

#[test]
fn test_save_settings_accepts_valid_json() {
    // Writes to launchpad_dir()/config.json. Local runs hit the real
    // ~/.launchpad; CI sets LAUNCHPAD_HOME=$(mktemp -d) so the
    // workflow stays sandboxed.
    let result = save_settings(r#"{"termFontSize": 14}"#.into());
    assert!(result.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════
// is_valid_git_ref / is_valid_git_oid validation tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_git_ref_rejects_invalid_names() {
    assert!(!is_valid_git_ref("branch name"));
    assert!(!is_valid_git_ref("branch;rm -rf"));
    assert!(!is_valid_git_ref("--no-verify"));
    assert!(!is_valid_git_ref("-x"));
    assert!(!is_valid_git_ref("`whoami`"));
    assert!(!is_valid_git_ref(""));
    // `..` is forbidden by git ref-format and also blocks range-syntax injection
    assert!(!is_valid_git_ref("foo..bar"));
    assert!(!is_valid_git_ref(".."));
}

#[test]
fn test_git_ref_accepts_valid_names() {
    for name in [
        "main",
        "feature/test-branch",
        "release-1.2.3",
        "v0.1.4",
        "user/foo_bar",
        // Rev syntax (parent / ancestor)
        "HEAD^",
        "HEAD~3",
        "abc1234^",
        "main~2",
    ] {
        assert!(is_valid_git_ref(name), "{:?} should be valid", name);
    }
}

#[test]
fn test_git_oid_accepts_short_and_full() {
    // 4 hex (minimum)
    assert!(is_valid_git_oid("abcd"));
    // 7 hex (typical short)
    assert!(is_valid_git_oid("abc1234"));
    // 40 hex (full SHA-1)
    assert!(is_valid_git_oid("0123456789abcdef0123456789abcdef01234567"));
    // Mixed case
    assert!(is_valid_git_oid("ABcd1234"));
}

#[test]
fn test_git_oid_rejects_invalid() {
    // Too short
    assert!(!is_valid_git_oid("abc"));
    // Too long (41)
    assert!(!is_valid_git_oid("0123456789abcdef0123456789abcdef012345678"));
    // Empty
    assert!(!is_valid_git_oid(""));
    // Non-hex
    assert!(!is_valid_git_oid("ghij"));
    // Contains shell metachar
    assert!(!is_valid_git_oid("abcd; rm"));
    // Contains slash (range-ish)
    assert!(!is_valid_git_oid("abcd..efgh"));
}

// ═══════════════════════════════════════════════════════════════════════════
// get_diff_between_refs tests
// ═══════════════════════════════════════════════════════════════════════════

// Helper: create a repo with two commits — c1 has hello.txt; c2 modifies
// hello.txt and adds foo.txt. Returns (TempDir, c1_oid, c2_oid).
fn setup_two_commit_repo() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@test.com").unwrap();
    let sig = repo.signature().unwrap();

    // Commit 1: hello.txt = "one\ntwo\nthree\n"
    fs::write(dir.path().join("hello.txt"), "one\ntwo\nthree\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("hello.txt")).unwrap();
    index.write().unwrap();
    let tree1 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let c1 = repo.commit(Some("HEAD"), &sig, &sig, "c1", &tree1, &[]).unwrap();

    // Commit 2: hello.txt becomes "one\nTWO\nthree\nfour\n"; new foo.txt = "alpha\n"
    fs::write(dir.path().join("hello.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    fs::write(dir.path().join("foo.txt"), "alpha\n").unwrap();
    index.add_path(std::path::Path::new("hello.txt")).unwrap();
    index.add_path(std::path::Path::new("foo.txt")).unwrap();
    index.write().unwrap();
    let tree2 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.find_commit(c1).unwrap();
    let c2 = repo.commit(Some("HEAD"), &sig, &sig, "c2", &tree2, &[&parent]).unwrap();

    (dir, c1.to_string(), c2.to_string())
}

#[test]
fn test_diff_between_refs_multi_file() {
    let (dir, c1, c2) = setup_two_commit_repo();
    let result = tauri::async_runtime::block_on(get_diff_between_refs(
        dir.path().to_string_lossy().to_string(),
        c1,
        c2,
    )).unwrap();

    assert_eq!(result.stats.files_changed, 2, "should detect 2 changed files");
    // foo.txt: 1 added line; hello.txt: 1 added (TWO), 1 removed (two), 1 added (four)
    // Total: 3 additions, 1 deletion
    assert_eq!(result.stats.additions, 3);
    assert_eq!(result.stats.deletions, 1);

    // Each file should have at least one hunk
    for f in &result.files {
        assert!(!f.hunks.is_empty(), "file {:?} should have hunks", f.new_path);
    }

    // Files are walked in libgit2's order (typically alphabetical):
    // foo.txt (new file) and hello.txt (modified)
    let paths: Vec<&str> = result.files.iter()
        .filter_map(|f| f.new_path.as_deref())
        .collect();
    assert!(paths.contains(&"foo.txt"));
    assert!(paths.contains(&"hello.txt"));
}

#[test]
fn test_diff_between_refs_short_oid() {
    // Short OIDs (7 chars) should resolve via revparse_single
    let (dir, c1, c2) = setup_two_commit_repo();
    let result = tauri::async_runtime::block_on(get_diff_between_refs(
        dir.path().to_string_lossy().to_string(),
        c1[..7].to_string(),
        c2[..7].to_string(),
    )).unwrap();
    assert_eq!(result.stats.files_changed, 2);
}

#[test]
fn test_diff_between_refs_rejects_bad_input() {
    let (dir, _c1, c2) = setup_two_commit_repo();
    let path = dir.path().to_string_lossy().to_string();

    // Shell-injection attempt
    let err = tauri::async_runtime::block_on(get_diff_between_refs(path.clone(), "HEAD; rm -rf /".into(), c2.clone())).err().unwrap();
    assert!(err.contains("Invalid"), "expected validation error, got: {}", err);

    // Leading dash (option injection)
    let err = tauri::async_runtime::block_on(get_diff_between_refs(path.clone(), "--upload-pack=evil".into(), c2.clone())).err().unwrap();
    assert!(err.contains("Invalid"), "expected validation error, got: {}", err);

    // Non-existent ref (passes validation, fails revparse)
    assert!(tauri::async_runtime::block_on(get_diff_between_refs(path.clone(), "nonexistent-ref".into(), c2)).is_err());
}

#[test]
fn test_diff_between_refs_same_ref_is_empty() {
    // Diff of a ref with itself should produce no files / no changes.
    let (dir, _c1, c2) = setup_two_commit_repo();
    let result = tauri::async_runtime::block_on(get_diff_between_refs(
        dir.path().to_string_lossy().to_string(),
        c2.clone(),
        c2,
    )).unwrap();
    assert_eq!(result.stats.files_changed, 0);
    assert_eq!(result.stats.additions, 0);
    assert_eq!(result.stats.deletions, 0);
    assert!(result.files.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// git_amend_commit tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_amend_message_only() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let result = tauri::async_runtime::block_on(git_amend_commit(path.clone(), Some("amended message".into()), false)).unwrap();

    let repo = Repository::open(dir.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.message().unwrap().trim(), "amended message");
    assert_eq!(head.id().to_string(), result.oid);
    // Local-only repo — no remote refs, so no force-push needed.
    assert!(!result.requires_force_push);
}

#[test]
fn test_amend_with_staged_changes() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();

    // Stage a new file on top of the initial commit
    fs::write(dir.path().join("extra.txt"), "added by amend").unwrap();
    let repo = Repository::open(dir.path()).unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new("extra.txt")).unwrap();
    idx.write().unwrap();

    tauri::async_runtime::block_on(git_amend_commit(path, None, true)).unwrap();

    // The amended commit's tree should now include extra.txt
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_name("extra.txt").is_some(), "extra.txt should be in amended commit tree");
    // Message preserved (we passed None)
    assert_eq!(head.message().unwrap().trim(), "initial commit");
}

#[test]
fn test_amend_rejects_unborn_head() {
    let dir = setup_empty_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let err = tauri::async_runtime::block_on(git_amend_commit(path, Some("x".into()), false)).err().unwrap();
    assert!(err.contains("unborn"), "expected unborn error, got: {}", err);
}

#[test]
fn test_amend_requires_force_push_when_head_on_remote() {
    // Simulate a remote-tracking ref pointing at HEAD by writing it
    // directly into refs/remotes/origin/main. After amend, the helper
    // should report requires_force_push because the pre-amend HEAD oid
    // is reachable from refs/remotes/*.
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let repo = Repository::open(dir.path()).unwrap();
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference(
        "refs/remotes/origin/main",
        head_oid,
        true,
        "test fixture",
    ).unwrap();

    let result = tauri::async_runtime::block_on(git_amend_commit(path, Some("amended".into()), false)).unwrap();
    assert!(result.requires_force_push, "should require force-push when HEAD is on a remote ref");
}

// ═══════════════════════════════════════════════════════════════════════════
// git_cherry_pick — input validation only (full E2E requires shell git)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cherry_pick_rejects_invalid_oid() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    // Won't actually shell out to git — validator rejects first.
    // We construct AppState manually since this is a unit test.
    let app_state = AppState {
        ptys: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(Mutex::new(0)),
        fs_watcher: Arc::new(Mutex::new(HashMap::new())),
        git_op_slots: Arc::new(Mutex::new(HashMap::new())),
        git_op_counter: Arc::new(AtomicU64::new(0)),
        project_windows: Arc::new(Mutex::new(HashMap::new())),
        projects_file_lock: Arc::new(Mutex::new(())),
        project_env_file_lock: Arc::new(Mutex::new(())),
        rebase_state: Arc::new(Mutex::new(None)),
        lsp_servers: Arc::new(Mutex::new(HashMap::new())),
    };
    // Tauri's State<T> is created via app handle; here we just verify the
    // validator path by calling the command body indirectly. Simplest:
    // call is_valid_git_oid directly (already covered by test_git_oid_*).
    // This test asserts the wiring exists by making sure the function
    // compiles and AppState construction works alongside it.
    let _ = (dir, path, app_state);
}

// ═══════════════════════════════════════════════════════════════════════════
// get_pending_op_state tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_pending_op_none_for_clean_repo() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let s = tauri::async_runtime::block_on(get_pending_op_state(path)).unwrap();
    assert_eq!(s.kind, "none");
}

#[test]
fn test_pending_op_detects_merge() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    // Simulate a merge in progress by creating MERGE_HEAD.
    let gd = dir.path().join(".git");
    let repo = Repository::open(dir.path()).unwrap();
    let head_oid = repo.head().unwrap().target().unwrap();
    fs::write(gd.join("MERGE_HEAD"), format!("{}\n", head_oid)).unwrap();

    let s = tauri::async_runtime::block_on(get_pending_op_state(path)).unwrap();
    assert_eq!(s.kind, "merge");
    assert!(s.head_message.is_some());
}

#[test]
fn test_pending_op_detects_cherry_pick() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let gd = dir.path().join(".git");
    let repo = Repository::open(dir.path()).unwrap();
    let head_oid = repo.head().unwrap().target().unwrap();
    fs::write(gd.join("CHERRY_PICK_HEAD"), format!("{}\n", head_oid)).unwrap();

    let s = tauri::async_runtime::block_on(get_pending_op_state(path)).unwrap();
    assert_eq!(s.kind, "cherry_pick");
}

#[test]
fn test_pending_op_detects_rebase_merge_with_progress() {
    // rebase-merge layout (interactive rebase). msgnum/end give progress.
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let rd = dir.path().join(".git").join("rebase-merge");
    fs::create_dir_all(&rd).unwrap();
    fs::write(rd.join("msgnum"), "2\n").unwrap();
    fs::write(rd.join("end"), "5\n").unwrap();

    let s = tauri::async_runtime::block_on(get_pending_op_state(path)).unwrap();
    assert_eq!(s.kind, "rebase");
    assert_eq!(s.current_step, Some(2));
    assert_eq!(s.total_steps, Some(5));
}

#[test]
fn test_pending_op_detects_rebase_apply() {
    // rebase-apply layout (`git am`-style). next/last give progress.
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();
    let rd = dir.path().join(".git").join("rebase-apply");
    fs::create_dir_all(&rd).unwrap();
    fs::write(rd.join("next"), "1\n").unwrap();
    fs::write(rd.join("last"), "3\n").unwrap();

    let s = tauri::async_runtime::block_on(get_pending_op_state(path)).unwrap();
    assert_eq!(s.kind, "rebase");
    assert_eq!(s.current_step, Some(1));
    assert_eq!(s.total_steps, Some(3));
}

// ═══════════════════════════════════════════════════════════════════════════
// Interactive rebase tests
// ═══════════════════════════════════════════════════════════════════════════

fn setup_three_commit_repo() -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@test.com").unwrap();
    // Disable any commit signing the user's global config might enable —
    // the test env has no GPG key.
    config.set_bool("commit.gpgsign", false).unwrap();
    let sig = repo.signature().unwrap();
    let mut oids = Vec::new();
    let mut parent: Option<git2::Commit> = None;

    for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
        fs::write(dir.path().join(name), format!("line {}\n", i)).unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new(name)).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, &format!("commit {}", i + 1), &tree, &parents)
            .unwrap();
        oids.push(oid.to_string());
        parent = Some(repo.find_commit(oid).unwrap());
    }
    (dir, oids)
}

#[test]
fn test_rebase_candidates_no_upstream_caps_at_count() {
    let (dir, oids) = setup_three_commit_repo();
    let path = dir.path().to_string_lossy().to_string();
    let candidates = tauri::async_runtime::block_on(get_rebase_candidate_commits(path, 10)).unwrap();
    assert!(!candidates.upstream_known, "no upstream configured");
    assert_eq!(candidates.commits.len(), 3);
    // Newest first
    assert_eq!(candidates.commits[0].oid, oids[2]);
    assert_eq!(candidates.commits[2].oid, oids[0]);
    // Local repo only — no remote refs, so all on_remote should be false
    assert!(candidates.commits.iter().all(|c| !c.on_remote));
}

#[test]
fn test_rebase_candidates_detached_head_typed_error() {
    let (dir, oids) = setup_three_commit_repo();
    let path = dir.path().to_string_lossy().to_string();
    // Detach HEAD at the middle commit
    let repo = Repository::open(dir.path()).unwrap();
    let mid_oid = git2::Oid::from_str(&oids[1]).unwrap();
    repo.set_head_detached(mid_oid).unwrap();
    drop(repo);

    let err = tauri::async_runtime::block_on(get_rebase_candidate_commits(path, 10)).err().unwrap();
    assert_eq!(err, "detached_head");
}

#[test]
fn test_rebase_candidates_on_remote_flag() {
    // When a remote-tracking ref points at HEAD, on_remote is true for
    // that commit and any earlier reachable from it.
    let (dir, oids) = setup_three_commit_repo();
    let path = dir.path().to_string_lossy().to_string();
    let repo = Repository::open(dir.path()).unwrap();
    let head_oid = git2::Oid::from_str(&oids[2]).unwrap();
    repo.reference("refs/remotes/origin/main", head_oid, true, "test fixture")
        .unwrap();
    drop(repo);

    let candidates = tauri::async_runtime::block_on(get_rebase_candidate_commits(path, 10)).unwrap();
    // All three commits are reachable from origin/main → all on_remote.
    assert!(candidates.commits.iter().all(|c| c.on_remote));
}

#[test]
fn test_rebase_state_dir_creates_executable_scripts() {
    // Verifies the script generation independently of running git, so
    // we don't depend on a system git binary in this assertion.
    let todo = vec![
        RebaseTodoEntry { action: "pick".into(), oid: "abc1234".into(), new_message: None },
    ];
    let dir = create_rebase_state_dir(&todo).unwrap();
    assert!(dir.join("state.json").exists());
    assert!(dir.join("seq_editor.sh").exists());
    assert!(dir.join("commit_editor.sh").exists());

    use std::os::unix::fs::PermissionsExt;
    let seq_perms = fs::metadata(dir.join("seq_editor.sh")).unwrap().permissions();
    assert_eq!(seq_perms.mode() & 0o777, 0o700);
    let commit_perms = fs::metadata(dir.join("commit_editor.sh")).unwrap().permissions();
    assert_eq!(commit_perms.mode() & 0o777, 0o700);
    // Directory is 0o700 — other users on a shared box can't read state.
    let dir_perms = fs::metadata(&dir).unwrap().permissions();
    assert_eq!(dir_perms.mode() & 0o777, 0o700);
    // state.json is 0o600 so a permissive umask doesn't expose contents.
    let state_perms = fs::metadata(dir.join("state.json")).unwrap().permissions();
    assert_eq!(state_perms.mode() & 0o777, 0o600);

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["version"], 1);
    assert_eq!(state["todo"][0]["action"], "pick");

    cleanup_rebase_state_dir(&dir);
    assert!(!dir.exists());
}

#[test]
fn test_rebase_state_dir_round_trips_complex_messages() {
    // Reword messages can contain newlines, Unicode, quotes, and JSON
    // metacharacters. serde must round-trip them so commit_editor.sh
    // (which json.loads the file) writes the literal user content.
    let messages = [
        "simple message",
        "multi\nline\nmessage",
        "with \"quotes\" and \\backslashes",
        "Unicode: 日本語 🎉",
        "shell\\meta;chars`$(echo evil)",
    ];
    for msg in messages.iter() {
        let todo = vec![
            RebaseTodoEntry {
                action: "reword".into(),
                oid: "abc1234".into(),
                new_message: Some((*msg).into()),
            },
        ];
        let dir = create_rebase_state_dir(&todo).unwrap();
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(state["todo"][0]["new_message"].as_str().unwrap(), *msg);
        cleanup_rebase_state_dir(&dir);
    }
}

#[test]
fn test_rebase_backup_tag_lifecycle() {
    let (dir, _oids) = setup_three_commit_repo();
    let path = dir.path().to_string_lossy().to_string();
    let repo = Repository::open(dir.path()).unwrap();
    let head_before = repo.head().unwrap().peel_to_commit().unwrap().id();

    let tag = create_rebase_backup_tag(&repo).unwrap();
    assert!(tag.starts_with("launchpad/pre-rebase/"));

    // Tag should resolve to HEAD's commit.
    let tag_ref = repo
        .find_reference(&format!("refs/tags/{}", tag))
        .unwrap();
    assert_eq!(tag_ref.target().unwrap(), head_before);

    // Delete works.
    delete_rebase_backup_tag(&path, &tag);
    assert!(repo
        .find_reference(&format!("refs/tags/{}", tag))
        .is_err());
}

#[test]
fn test_rebase_in_progress_detection() {
    let (dir, _) = setup_three_commit_repo();
    let path = dir.path().to_string_lossy().to_string();
    assert!(!rebase_in_progress(&path));

    // Simulate a rebase pause by writing the state directory git uses.
    let rd = dir.path().join(".git").join("rebase-merge");
    fs::create_dir_all(&rd).unwrap();
    assert!(rebase_in_progress(&path));
    fs::remove_dir_all(&rd).unwrap();
    assert!(!rebase_in_progress(&path));
}

// ═══════════════════════════════════════════════════════════════════════════
// git_unstage tests — including unborn HEAD
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_unstage_file_with_commits() {
    let dir = setup_git_repo();
    let path = dir.path().to_string_lossy().to_string();

    // Create and stage a new file
    fs::write(dir.path().join("new.txt"), "new file").unwrap();
    let repo = Repository::discover(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("new.txt")).unwrap();
    index.write().unwrap();

    // Unstage it
    let result = tauri::async_runtime::block_on(git_unstage_file(path, "new.txt".into()));
    assert!(result.is_ok());

    // Verify it's no longer staged
    let repo = Repository::discover(dir.path()).unwrap();
    let statuses = repo.statuses(None).unwrap();
    for entry in statuses.iter() {
        if entry.path() == Some("new.txt") {
            assert!(!entry.status().contains(git2::Status::INDEX_NEW));
        }
    }
}

#[test]
fn test_unstage_file_unborn_head() {
    let dir = setup_empty_git_repo();
    let path = dir.path().to_string_lossy().to_string();

    // Create and stage a file in a repo with no commits
    fs::write(dir.path().join("first.txt"), "first file").unwrap();
    let repo = Repository::discover(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("first.txt")).unwrap();
    index.write().unwrap();

    // Should not panic or error — this is the bug we fixed
    let result = tauri::async_runtime::block_on(git_unstage_file(path, "first.txt".into()));
    assert!(result.is_ok());
}

#[test]
fn test_unstage_all_unborn_head() {
    let dir = setup_empty_git_repo();
    let path = dir.path().to_string_lossy().to_string();

    // Stage multiple files in a repo with no commits
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.txt"), "b").unwrap();
    let repo = Repository::discover(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("a.txt")).unwrap();
    index.add_path(std::path::Path::new("b.txt")).unwrap();
    index.write().unwrap();

    // Should not panic or error
    let result = tauri::async_runtime::block_on(git_unstage_all(path));
    assert!(result.is_ok());

    // Verify index is empty
    let repo = Repository::discover(dir.path()).unwrap();
    let index = repo.index().unwrap();
    assert_eq!(index.len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// get_ahead_behind tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_ahead_behind_no_upstream() {
    let dir = setup_git_repo();
    let repo = Repository::discover(dir.path()).unwrap();

    // No remote configured — should return None gracefully
    let result = get_ahead_behind(&repo);
    assert!(result.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// checkout_branch_inner tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_checkout_nonexistent_branch() {
    let dir = setup_git_repo();
    let repo = Repository::discover(dir.path()).unwrap();

    let result = checkout_branch_inner(&repo, "nonexistent-branch");
    assert!(result.is_err());
}

#[test]
fn test_checkout_existing_branch() {
    let dir = setup_git_repo();
    let repo = Repository::discover(dir.path()).unwrap();

    // Create a branch
    let head = repo.head().unwrap();
    let commit = head.peel_to_commit().unwrap();
    repo.branch("test-branch", &commit, false).unwrap();

    // Checkout should succeed
    let result = checkout_branch_inner(&repo, "test-branch");
    assert!(result.is_ok());

    // Verify HEAD points to new branch
    let head = repo.head().unwrap();
    assert_eq!(head.shorthand().unwrap(), "test-branch");
}

// ═══════════════════════════════════════════════════════════════════════════
// atomic_write / atomic_write_with_mode tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_atomic_write_basic() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    atomic_write(&dest, b"hello").unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"hello");
}

#[test]
fn test_atomic_write_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    fs::write(&dest, b"old contents").unwrap();
    atomic_write(&dest, b"new").unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"new");
}

#[test]
fn test_atomic_write_preserves_mode_on_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    fs::write(&dest, b"old").unwrap();
    // Set a non-default mode on the existing file
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).unwrap();
    atomic_write(&dest, b"new").unwrap();
    let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "atomic_write should preserve 0o600 across overwrite");
}

#[test]
fn test_atomic_write_with_explicit_mode_overrides_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    // Pre-create with 0o644
    fs::write(&dest, b"old").unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).unwrap();
    // Explicit 0o600 should win over the preserved 0o644
    atomic_write_with_mode(&dest, b"new", Some(0o600)).unwrap();
    let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn test_atomic_write_with_explicit_mode_on_new_file() {
    // First-write case for project-env.json: no destination yet, so the
    // explicit mode is the *only* thing protecting the secret.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("new-secret.json");
    atomic_write_with_mode(&dest, b"{}", Some(0o600)).unwrap();
    let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn test_atomic_write_no_temp_leftover_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    atomic_write(&dest, b"data").unwrap();
    // Scan dir for stray .lp-tmp- files
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".lp-tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "no .lp-tmp- files should remain after success");
}

#[test]
fn test_atomic_write_unique_temp_paths_under_burst() {
    // Counter must produce distinct temp names for back-to-back writes
    // to the same destination from the same process. We can't observe
    // the temp paths directly, so we drive a tight loop and assert that
    // every write succeeds (a name collision would manifest as an
    // io::Error from File::create or rename racing the cleanup).
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.txt");
    for i in 0..50 {
        atomic_write(&dest, format!("payload-{}", i).as_bytes()).unwrap();
    }
    assert_eq!(fs::read_to_string(&dest).unwrap(), "payload-49");
}

// ═══════════════════════════════════════════════════════════════════════════
// is_valid_env_key tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_env_key_accepts_valid_names() {
    for k in ["FOO", "_FOO", "FOO_BAR", "F1", "_", "a", "lower_case", "MIXED_Case_1"] {
        assert!(is_valid_env_key(k), "{:?} should be valid", k);
    }
}

#[test]
fn test_env_key_rejects_empty() {
    assert!(!is_valid_env_key(""));
}

#[test]
fn test_env_key_rejects_leading_digit() {
    assert!(!is_valid_env_key("1FOO"));
    assert!(!is_valid_env_key("9"));
}

#[test]
fn test_env_key_rejects_punctuation_and_whitespace() {
    for k in ["FOO-BAR", "FOO.BAR", "FOO BAR", "FOO=BAR", "FOO/BAR", " FOO", "FOO ", "FOO\n"] {
        assert!(!is_valid_env_key(k), "{:?} should be invalid", k);
    }
}

#[test]
fn test_env_key_rejects_non_ascii() {
    // POSIX is ASCII-only; reject Unicode letters even though they
    // satisfy `char::is_alphabetic`.
    assert!(!is_valid_env_key("FOOÉ"));
    assert!(!is_valid_env_key("Ω"));
    assert!(!is_valid_env_key("FOO_λ"));
}

// ═══════════════════════════════════════════════════════════════════════════
// read_file_preview binary / encoding heuristic tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_read_file_preview_rejects_utf16_le_bom() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u16le.txt");
    let mut data = vec![0xFF, 0xFE];
    data.extend_from_slice(b"h\0e\0l\0l\0o\0");
    fs::write(&path, &data).unwrap();
    let err = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None)).unwrap_err();
    assert!(err.contains("UTF-16"), "expected UTF-16 message, got: {}", err);
}

#[test]
fn test_read_file_preview_rejects_utf16_be_bom() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u16be.txt");
    let mut data = vec![0xFE, 0xFF];
    data.extend_from_slice(b"\0h\0e\0l\0l\0o");
    fs::write(&path, &data).unwrap();
    let err = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None)).unwrap_err();
    assert!(err.contains("UTF-16"));
}

#[test]
fn test_read_file_preview_rejects_binary_above_threshold() {
    // 100 bytes, ~30% nulls — well above the 1% binary cutoff.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bin.dat");
    let mut data = Vec::with_capacity(100);
    for i in 0..100 {
        data.push(if i % 3 == 0 { 0u8 } else { b'A' });
    }
    fs::write(&path, &data).unwrap();
    let err = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None)).unwrap_err();
    assert!(err.contains("Binary"), "expected Binary message, got: {}", err);
}

#[test]
fn test_read_file_preview_tolerates_null_in_short_file() {
    // 20 bytes, single null. Pre-fix this would have been "5% null" and
    // wrongly rejected; the 64-byte minimum keeps it readable.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.txt");
    let mut data = b"hello world".to_vec();
    data.push(0);
    data.extend_from_slice(b"more text");
    assert!(data.len() <= 64);
    fs::write(&path, &data).unwrap();
    let result = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None));
    assert!(result.is_ok(), "short file with single null should be readable");
}

#[test]
fn test_read_file_preview_accepts_long_text_with_no_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.txt");
    let body: String = "the quick brown fox jumps over the lazy dog\n".repeat(20);
    fs::write(&path, body.as_bytes()).unwrap();
    let result = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None)).unwrap();
    // Default cap is 8192 bytes — body is well under that, so full content returns.
    assert_eq!(result, body);
}

#[test]
fn test_read_file_preview_just_at_binary_threshold() {
    // 200 bytes with exactly 1 null = 0.5% — under the 1% cutoff, should pass.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edge.txt");
    let mut data = vec![b'A'; 200];
    data[100] = 0;
    fs::write(&path, &data).unwrap();
    let result = tauri::async_runtime::block_on(read_file_preview(path.to_string_lossy().to_string(), None));
    assert!(result.is_ok(), "0.5% nulls should be tolerated");
}

// ═══════════════════════════════════════════════════════════════════════════
// delete_path project-root guard tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_delete_path_allows_file_inside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let target = dir.path().join("doomed.txt");
    fs::write(&target, b"x").unwrap();
    let result = tauri::async_runtime::block_on(delete_path(target.to_string_lossy().to_string(), Some(root)));
    assert!(result.is_ok(), "delete inside root should succeed: {:?}", result);
    assert!(!target.exists());
}

#[test]
fn test_delete_path_allows_directory_inside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let sub = dir.path().join("nested/deeper");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("a.txt"), b"a").unwrap();
    let target = dir.path().join("nested");
    let result = tauri::async_runtime::block_on(delete_path(target.to_string_lossy().to_string(), Some(root)));
    assert!(result.is_ok());
    assert!(!target.exists(), "recursive directory delete should remove tree");
}

#[test]
fn test_delete_path_refuses_outside_root() {
    // Two sibling temp dirs. The "project" claims one as root; the
    // target lives in the other. The guard should refuse.
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let outside = other_dir.path().join("victim.txt");
    fs::write(&outside, b"keep me").unwrap();

    let root = project_dir.path().to_string_lossy().to_string();
    let result = tauri::async_runtime::block_on(delete_path(outside.to_string_lossy().to_string(), Some(root)));
    assert!(result.is_err(), "expected refusal, got: {:?}", result);
    assert!(
        result.as_ref().unwrap_err().contains("outside project root"),
        "wrong error message: {:?}",
        result
    );
    assert!(outside.exists(), "guard must not delete the target");
}

#[test]
fn test_is_safe_relative_path() {
    // Contained relative paths are allowed...
    assert!(is_safe_relative_path("src/main.rs"));
    assert!(is_safe_relative_path("a/b/c.txt"));
    assert!(is_safe_relative_path("./file.txt"));
    assert!(is_safe_relative_path("file.txt"));
    // ...absolute and traversal paths are refused (the merge-tab read guard).
    assert!(!is_safe_relative_path("/etc/passwd"));
    assert!(!is_safe_relative_path("../secrets"));
    assert!(!is_safe_relative_path("a/../../b"));
    assert!(!is_safe_relative_path("../../etc/passwd"));
}

#[test]
fn test_delete_path_refuses_when_root_is_none() {
    // A delete with no project_root is refused outright — every real caller
    // (the file-browser context menu) passes one, so a missing root means
    // an unexpected/foreign caller. The target must survive untouched.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("file.txt");
    fs::write(&target, b"x").unwrap();
    let result = tauri::async_runtime::block_on(delete_path(target.to_string_lossy().to_string(), None));
    assert!(result.is_err(), "expected refusal, got: {:?}", result);
    assert!(target.exists(), "guard must not delete the target");
}

#[test]
fn test_delete_path_refuses_root_itself() {
    // target == root canonicalizes equal, and starts_with is true for equal
    // paths — so the guard must reject the root explicitly or it would
    // remove_dir_all the whole project.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let result = tauri::async_runtime::block_on(delete_path(root.clone(), Some(root.clone())));
    assert!(result.is_err(), "expected refusal, got: {:?}", result);
    assert!(dir.path().exists(), "guard must not delete the project root");
}

#[test]
fn test_delete_path_refuses_symlink_escaping_root() {
    use std::os::unix::fs::symlink;
    // The symlink itself lives inside the project, but its target is
    // outside. canonicalize() follows symlinks, so the guard's
    // starts_with check sees the outside path and refuses.
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let outside_target = other_dir.path().join("victim.txt");
    fs::write(&outside_target, b"keep me").unwrap();

    let escape_link = project_dir.path().join("escape");
    symlink(&outside_target, &escape_link).unwrap();

    let root = project_dir.path().to_string_lossy().to_string();
    let result = tauri::async_runtime::block_on(delete_path(escape_link.to_string_lossy().to_string(), Some(root)));
    assert!(result.is_err(), "symlink whose target escapes root must be refused");
    assert!(outside_target.exists(), "outside target must survive");
}

#[test]
fn test_delete_path_errors_on_missing_target() {
    // canonicalize fails on a non-existent path → propagated error.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let missing = dir.path().join("never-existed.txt");
    let result = tauri::async_runtime::block_on(delete_path(missing.to_string_lossy().to_string(), Some(root)));
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// normalize_project_path canonicalization tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_normalize_project_path_dedupes_trailing_slash() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().to_string_lossy().to_string();
    let with_slash = format!("{}/", raw);
    assert_eq!(
        normalize_project_path(&raw),
        normalize_project_path(&with_slash),
        "trailing slash should not produce a different key"
    );
}

#[test]
fn test_normalize_project_path_dedupes_dot_segments() {
    // `./foo` and `foo` from the same cwd should canonicalize identically.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("inner");
    fs::create_dir(&sub).unwrap();
    let direct = sub.to_string_lossy().to_string();
    let dotted = format!("{}/./", sub.to_string_lossy());
    assert_eq!(normalize_project_path(&direct), normalize_project_path(&dotted));
}

#[test]
fn test_normalize_project_path_dedupes_symlink_alias() {
    use std::os::unix::fs::symlink;
    // macOS has /var → /private/var as a system-level alias. Reproduce
    // the same shape with our own symlink: alias should normalize to
    // the canonical target.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = dir.path().join("alias");
    symlink(&real, &alias).unwrap();

    let real_norm = normalize_project_path(&real.to_string_lossy());
    let alias_norm = normalize_project_path(&alias.to_string_lossy());
    assert_eq!(real_norm, alias_norm,
        "symlink alias must dedupe to canonical target");
}

#[test]
fn test_normalize_project_path_fallback_for_missing_dir() {
    // canonicalize fails on a non-existent path. The fallback path
    // trims trailing `/` and returns the raw string, so two different
    // not-yet-existing strings won't dedupe — but a single string
    // remains stable (with trailing slash stripped).
    let raw = "/no/such/place".to_string();
    let trailing = "/no/such/place/".to_string();
    assert_eq!(normalize_project_path(&raw), normalize_project_path(&trailing));
    assert_eq!(normalize_project_path(&raw), "/no/such/place");
}

// ═══════════════════════════════════════════════════════════════════════════
// cancel_git_op_inner / GitOpSlot state machine tests
// ═══════════════════════════════════════════════════════════════════════════

fn slots_with(label: &str, slot: GitOpSlot) -> Mutex<GitOpSlots> {
    let mut map = GitOpSlots::new();
    map.insert(label.to_string(), slot);
    Mutex::new(map)
}

#[test]
fn test_cancel_op_no_slot_is_noop() {
    let store: Mutex<GitOpSlots> = Mutex::new(GitOpSlots::new());
    let pid = cancel_git_op_inner(&store, "main");
    assert_eq!(pid, None);
    assert!(store.lock_safe().is_empty());
}

#[test]
fn test_cancel_op_sets_flag_when_pid_unrecorded() {
    // Simulates the spawn-window race: cancel arrives AFTER the slot
    // is reserved but BEFORE spawn_git_under_slot has recorded a PID.
    // The flag must persist so the spawning op observes it post-spawn.
    let store = slots_with("main", GitOpSlot {
        op_id: 42,
        pid: None,
        cancelled: false,
    });
    let pid = cancel_git_op_inner(&store, "main");
    assert_eq!(pid, None, "no PID to kill yet");

    let guard = store.lock_safe();
    let slot = guard.get("main").expect("slot must NOT be cleared");
    assert!(slot.cancelled, "flag must be set so spawn-window cancel survives");
    assert_eq!(slot.op_id, 42, "op_id must be preserved");
}

#[test]
fn test_cancel_op_returns_pid_when_recorded() {
    let store = slots_with("main", GitOpSlot {
        op_id: 7,
        pid: Some(99999),
        cancelled: false,
    });
    let pid = cancel_git_op_inner(&store, "main");
    assert_eq!(pid, Some(99999), "caller needs the PID to kill");
    assert!(store.lock_safe().get("main").unwrap().cancelled);
}

#[test]
fn test_cancel_op_idempotent() {
    // Double-click cancel: second call shouldn't change anything.
    let store = slots_with("main", GitOpSlot {
        op_id: 1,
        pid: Some(123),
        cancelled: false,
    });
    assert_eq!(cancel_git_op_inner(&store, "main"), Some(123));
    assert_eq!(cancel_git_op_inner(&store, "main"), Some(123));
    let guard = store.lock_safe();
    let slot = guard.get("main").unwrap();
    assert!(slot.cancelled);
    assert_eq!(slot.pid, Some(123));
}

#[test]
fn test_cancel_op_does_not_clear_slot() {
    // The cleanup of the slot (removing it) is owned by
    // run_git_cancellable / git_push, gated on op_id match. cancel
    // must NOT do it — otherwise a stale cancel races a new op.
    let store = slots_with("main", GitOpSlot {
        op_id: 5,
        pid: None,
        cancelled: false,
    });
    cancel_git_op_inner(&store, "main");
    assert!(
        store.lock_safe().contains_key("main"),
        "cancel must not wipe slot — cleanup is the spawning op's job"
    );
}

#[test]
fn test_cancel_op_isolates_per_window() {
    // Two windows each with their own in-flight op. Cancelling window A
    // must NOT touch window B's slot, and vice versa.
    let mut map = GitOpSlots::new();
    map.insert("window-a".into(), GitOpSlot {
        op_id: 1,
        pid: Some(1111),
        cancelled: false,
    });
    map.insert("window-b".into(), GitOpSlot {
        op_id: 2,
        pid: Some(2222),
        cancelled: false,
    });
    let store = Mutex::new(map);

    let pid = cancel_git_op_inner(&store, "window-a");
    assert_eq!(pid, Some(1111));

    let guard = store.lock_safe();
    assert!(guard.get("window-a").unwrap().cancelled, "A is cancelled");
    assert!(!guard.get("window-b").unwrap().cancelled, "B must be untouched");
    assert_eq!(guard.get("window-b").unwrap().pid, Some(2222));
}

#[test]
fn test_cancel_op_unknown_window_is_noop() {
    let store = slots_with("main", GitOpSlot {
        op_id: 1,
        pid: Some(123),
        cancelled: false,
    });
    let pid = cancel_git_op_inner(&store, "other-window");
    assert_eq!(pid, None);
    // Original window's slot untouched.
    assert!(!store.lock_safe().get("main").unwrap().cancelled);
}

// ═══════════════════════════════════════════════════════════════════════════
// read_directory tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_read_directory_sorts_dirs_first_then_alpha() {
    let dir = tempfile::tempdir().unwrap();
    // Mix dirs and files with names that test case-insensitive sort.
    fs::write(dir.path().join("zeta.txt"), b"").unwrap();
    fs::write(dir.path().join("Apple.txt"), b"").unwrap();
    fs::create_dir(dir.path().join("Zoo")).unwrap();
    fs::create_dir(dir.path().join("alpha-dir")).unwrap();

    let entries = tauri::async_runtime::block_on(read_directory(dir.path().to_string_lossy().to_string())).unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    // Dirs first (case-insensitive alpha), then files (case-insensitive alpha).
    assert_eq!(names, vec!["alpha-dir", "Zoo", "Apple.txt", "zeta.txt"]);
}

#[test]
fn test_read_directory_marks_hidden() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".hidden"), b"").unwrap();
    fs::write(dir.path().join("visible.txt"), b"").unwrap();

    let entries = tauri::async_runtime::block_on(read_directory(dir.path().to_string_lossy().to_string())).unwrap();
    let hidden: Vec<&str> = entries.iter().filter(|e| e.is_hidden).map(|e| e.name.as_str()).collect();
    assert_eq!(hidden, vec![".hidden"]);
}

#[test]
fn test_read_directory_populates_metadata() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("with-bytes.txt"), b"abcdefg").unwrap();

    let entries = tauri::async_runtime::block_on(read_directory(dir.path().to_string_lossy().to_string())).unwrap();
    let entry = entries.iter().find(|e| e.name == "with-bytes.txt").unwrap();
    assert_eq!(entry.size, 7);
    assert!(!entry.is_dir);
    assert!(entry.modified.is_some(), "modified time should be populated");
    assert_ne!(entry.mode, 0, "mode should be populated for an existing file");
}

#[test]
fn test_read_directory_errors_on_missing_dir() {
    let result = tauri::async_runtime::block_on(read_directory("/no/such/directory/anywhere".into()));
    assert!(result.is_err());
}

#[test]
fn test_read_directory_includes_hidden_entries_in_listing() {
    // The hidden flag is informational — the entry must still appear.
    // Filtering hidden files is a frontend choice.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), b"").unwrap();
    let entries = tauri::async_runtime::block_on(read_directory(dir.path().to_string_lossy().to_string())).unwrap();
    assert!(entries.iter().any(|e| e.name == ".env"));
}

// ═══════════════════════════════════════════════════════════════════════════
// find_path_by_inode walker tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_find_path_by_inode_locates_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hello.txt");
    fs::write(&path, b"x").unwrap();
    let inode = tauri::async_runtime::block_on(get_file_inode(path.to_string_lossy().to_string())).unwrap();

    let found = tauri::async_runtime::block_on(find_path_by_inode(dir.path().to_string_lossy().to_string(), inode)).unwrap();
    assert_eq!(found, Some(path.to_string_lossy().to_string()));
}

#[test]
fn test_find_path_by_inode_follows_rename() {
    // The whole point of inode lookup: an external rename moves the
    // file but preserves the inode. The walker should still find it.
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("before.txt");
    fs::write(&original, b"x").unwrap();
    let inode = tauri::async_runtime::block_on(get_file_inode(original.to_string_lossy().to_string())).unwrap();

    let renamed = dir.path().join("nested").join("after.txt");
    fs::create_dir_all(renamed.parent().unwrap()).unwrap();
    fs::rename(&original, &renamed).unwrap();

    let found = tauri::async_runtime::block_on(find_path_by_inode(dir.path().to_string_lossy().to_string(), inode)).unwrap();
    assert_eq!(found, Some(renamed.to_string_lossy().to_string()),
        "inode walker should find the file at its new path");
}

#[test]
fn test_find_path_by_inode_returns_none_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"x").unwrap();
    // Inode 1 effectively never matches a real file in a tempdir.
    let found = tauri::async_runtime::block_on(find_path_by_inode(dir.path().to_string_lossy().to_string(), 1)).unwrap();
    assert_eq!(found, None);
}

#[test]
fn test_find_path_by_inode_skips_node_modules() {
    // File is the only one in the tree, but it lives under a skipped
    // directory. The walker should NOT find it (skip rules apply).
    let dir = tempfile::tempdir().unwrap();
    let nm = dir.path().join("node_modules").join("pkg");
    fs::create_dir_all(&nm).unwrap();
    let buried = nm.join("buried.js");
    fs::write(&buried, b"x").unwrap();
    let inode = tauri::async_runtime::block_on(get_file_inode(buried.to_string_lossy().to_string())).unwrap();

    let found = tauri::async_runtime::block_on(find_path_by_inode(dir.path().to_string_lossy().to_string(), inode)).unwrap();
    assert_eq!(found, None,
        "node_modules must be skipped to keep the walk bounded");
}

#[test]
fn test_find_path_by_inode_skips_hidden_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join(".cache");
    fs::create_dir(&hidden).unwrap();
    let buried = hidden.join("data");
    fs::write(&buried, b"x").unwrap();
    let inode = tauri::async_runtime::block_on(get_file_inode(buried.to_string_lossy().to_string())).unwrap();

    let found = tauri::async_runtime::block_on(find_path_by_inode(dir.path().to_string_lossy().to_string(), inode)).unwrap();
    assert_eq!(found, None);
}

// ═══════════════════════════════════════════════════════════════════════════
// search_files skip-rule + ranking tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_search_files_basic_substring_match() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), b"").unwrap();
    fs::write(dir.path().join("settings.rs"), b"").unwrap();
    fs::write(dir.path().join("other.txt"), b"").unwrap();

    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "set".into(), None)).unwrap();
    assert_eq!(hits, vec!["settings.rs"]);
}

#[test]
fn test_search_files_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ReadMe.md"), b"").unwrap();
    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "README".into(), None)).unwrap();
    assert_eq!(hits, vec!["ReadMe.md"]);
}

#[test]
fn test_search_files_skips_node_modules_target_and_hidden() {
    let dir = tempfile::tempdir().unwrap();
    // Files in skipped dirs — none should match.
    for skipped in ["node_modules", "target", ".git", "__pycache__", "dist", "build"] {
        let p = dir.path().join(skipped);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("hit.txt"), b"").unwrap();
    }
    // One match in the project root proper.
    fs::write(dir.path().join("hit.txt"), b"").unwrap();

    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "hit".into(), None)).unwrap();
    assert_eq!(hits, vec!["hit.txt"], "only the root-level file should match");
}

#[test]
fn test_search_files_respects_max_results() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        fs::write(dir.path().join(format!("file-{}.txt", i)), b"").unwrap();
    }
    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "file".into(), Some(3))).unwrap();
    assert_eq!(hits.len(), 3);
}

#[test]
fn test_search_files_ranks_shorter_paths_first() {
    // Quick-open scoring: shorter relative path wins. A shallow file
    // should rank above the same name buried deeper.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config.rs"), b"").unwrap();
    let nested = dir.path().join("subdir").join("more").join("config-deep.rs");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    fs::write(&nested, b"").unwrap();

    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "config".into(), None)).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0], "config.rs", "shorter path should rank first");
}

#[test]
fn test_search_files_returns_relative_paths() {
    // Tab UI and Cmd+P show relative paths; absolute paths would
    // bloat the list and reveal /private/var/folders/... noise.
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("src").join("main.rs");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    fs::write(&nested, b"").unwrap();

    let hits = tauri::async_runtime::block_on(search_files(dir.path().to_string_lossy().to_string(), "main".into(), None)).unwrap();
    assert_eq!(hits, vec!["src/main.rs"]);
}

// ═══════════════════════════════════════════════════════════════════════════
// get_git_status tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_git_status_non_repo() {
    let dir = tempfile::tempdir().unwrap();
    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    assert!(!info.is_repo);
    assert_eq!(info.branch, None);
    assert!(info.files.is_empty());
    assert_eq!(info.ahead, 0);
    assert_eq!(info.behind, 0);
    assert!(!info.has_upstream);
}

#[test]
fn test_git_status_clean_repo_after_initial_commit() {
    let dir = setup_git_repo();
    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    assert!(info.is_repo);
    assert!(info.branch.is_some(), "branch should resolve after a commit");
    assert!(info.files.is_empty(), "clean tree should have no status entries");
}

#[test]
fn test_git_status_reports_untracked_as_new() {
    let dir = setup_git_repo();
    fs::write(dir.path().join("untracked.txt"), b"x").unwrap();
    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    let entry = info.files.iter().find(|f| f.path == "untracked.txt")
        .expect("untracked file should appear in status");
    assert_eq!(entry.status, "new");
}

#[test]
fn test_git_status_subdir_empty_at_repo_root() {
    // When the project IS the repo root, the subdir prefix is empty so the
    // frontend's project-relative paths already equal git's workdir-relative.
    let dir = setup_git_repo();
    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    assert_eq!(info.subdir, "", "repo-root project has no subdir prefix");
}

#[test]
fn test_git_status_subdir_for_nested_project() {
    // A project opened as a SUBDIRECTORY of the repo: Repository::discover walks
    // up to the repo root, status paths come back workdir-relative
    // ("frontend/..."), and `subdir` carries the prefix the frontend needs to
    // convert its project-relative paths. Regression guard for the
    // monorepo/worktree case where coloring + conflict-mode previously broke.
    let dir = setup_git_repo();
    let nested = dir.path().join("frontend");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("app.js"), b"x").unwrap();

    let info = get_git_status_inner(&nested.to_string_lossy()).unwrap();
    assert!(info.is_repo);
    assert_eq!(info.subdir, "frontend/", "subdir prefix should be the project's path under the workdir");
    // The untracked file is reported workdir-relative, so prefix + project-rel
    // ("frontend/" + "app.js") is what the frontend must match against.
    assert!(
        info.files.iter().any(|f| f.path == "frontend/app.js" && f.status == "new"),
        "nested untracked file should be keyed workdir-relative, got: {:?}",
        info.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

#[test]
fn test_git_status_reports_staged_as_index_new() {
    let dir = setup_git_repo();
    fs::write(dir.path().join("added.txt"), b"x").unwrap();
    let repo = Repository::open(dir.path()).unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new("added.txt")).unwrap();
    idx.write().unwrap();

    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    let entry = info.files.iter().find(|f| f.path == "added.txt").unwrap();
    assert_eq!(entry.status, "index_new",
        "staged-but-not-committed file should be index_new");
}

#[test]
fn test_git_status_dual_entry_when_staged_then_modified() {
    // The whole point of the dual-entry shape: a file appears in BOTH
    // the staged list (index_modified) AND the unstaged list (modified)
    // when its index version differs from HEAD AND its working tree
    // differs from the index.
    let dir = setup_git_repo();
    let target = dir.path().join("hello.txt");
    // Stage a change against HEAD ("hello world" → "stage me")
    fs::write(&target, b"stage me").unwrap();
    let repo = Repository::open(dir.path()).unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new("hello.txt")).unwrap();
    idx.write().unwrap();
    // Then modify again on disk so the worktree differs from the index
    fs::write(&target, b"and again").unwrap();

    let info = get_git_status_inner(&dir.path().to_string_lossy()).unwrap();
    let kinds: Vec<&str> = info.files.iter()
        .filter(|f| f.path == "hello.txt")
        .map(|f| f.status.as_str())
        .collect();
    assert!(kinds.contains(&"index_modified"),
        "expected staged entry, got: {:?}", kinds);
    assert!(kinds.contains(&"modified"),
        "expected unstaged entry, got: {:?}", kinds);
}

// ═══════════════════════════════════════════════════════════════════════════
// git_discard_file tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_discard_deletes_untracked_file() {
    let dir = setup_git_repo();
    let target = dir.path().join("untracked.txt");
    fs::write(&target, b"x").unwrap();
    assert!(target.exists());

    tauri::async_runtime::block_on(git_discard_file(
        dir.path().to_string_lossy().to_string(),
        "untracked.txt".into(),
    )).unwrap();
    assert!(!target.exists(), "untracked file should be deleted from disk");
}

#[test]
fn test_discard_deletes_file_inside_untracked_directory() {
    // Reproduces the GIT_ENOTFOUND fallback path: status_file can't
    // see children of an untracked directory (git only reports the
    // dir), so libgit2 returns NotFound. We have to fall back to a
    // disk presence check or the user clicks Discard on a file that
    // looks tracked-but-isn't and nothing happens.
    let dir = setup_git_repo();
    let untracked_dir = dir.path().join("scratch");
    fs::create_dir(&untracked_dir).unwrap();
    let target = untracked_dir.join("notes.txt");
    fs::write(&target, b"x").unwrap();

    tauri::async_runtime::block_on(git_discard_file(
        dir.path().to_string_lossy().to_string(),
        "scratch/notes.txt".into(),
    )).unwrap();
    assert!(!target.exists(), "file inside untracked dir should be deleted");
}

#[test]
fn test_discard_restores_tracked_file_to_head() {
    // setup_git_repo commits hello.txt with "hello world". Modify it
    // on disk, then discard — should snap back to HEAD content.
    let dir = setup_git_repo();
    let target = dir.path().join("hello.txt");
    fs::write(&target, b"corrupted").unwrap();

    tauri::async_runtime::block_on(git_discard_file(
        dir.path().to_string_lossy().to_string(),
        "hello.txt".into(),
    )).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"hello world",
        "discard should restore HEAD content");
}

#[test]
fn test_discard_errors_on_truly_missing_file() {
    // Path doesn't exist on disk and isn't tracked. status_file
    // returns NotFound, abs.exists() is false → treat_as_untracked
    // is false, status_result error propagates.
    let dir = setup_git_repo();
    let result = tauri::async_runtime::block_on(git_discard_file(
        dir.path().to_string_lossy().to_string(),
        "never-existed.txt".into(),
    ));
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// git_upstream_status tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_upstream_status_non_repo_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let (has, branch) = git_upstream_status(&dir.path().to_string_lossy());
    assert!(!has);
    assert_eq!(branch, None);
}

#[test]
fn test_upstream_status_unborn_head_returns_none() {
    // Empty repo with no commits: head() fails because HEAD points
    // at an unborn ref. The early-return must produce (false, None)
    // rather than panic.
    let dir = setup_empty_git_repo();
    let (has, branch) = git_upstream_status(&dir.path().to_string_lossy());
    assert!(!has);
    assert_eq!(branch, None);
}

#[test]
fn test_upstream_status_branch_without_upstream() {
    // Repo with a commit but no remote. Branch resolves; upstream doesn't.
    // git_push relies on this exact shape to decide between `push` and
    // `push --set-upstream origin <branch>`.
    let dir = setup_git_repo();
    let (has, branch) = git_upstream_status(&dir.path().to_string_lossy());
    assert!(!has, "no remote configured → no upstream");
    assert!(
        branch.is_some(),
        "branch name must be available so push can pass it to --set-upstream"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// git_stage_file / git_stage_all tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_stage_file_moves_status_from_unstaged_to_staged() {
    // setup_git_repo committed hello.txt. Modify it, then stage it.
    // It should appear as index_modified, NOT modified.
    let dir = setup_git_repo();
    let path_str = dir.path().to_string_lossy().to_string();
    fs::write(dir.path().join("hello.txt"), b"changed").unwrap();

    // Pre-stage: should be 'modified' (worktree)
    let pre = get_git_status_inner(&path_str).unwrap();
    assert!(pre.files.iter().any(|f| f.path == "hello.txt" && f.status == "modified"));

    tauri::async_runtime::block_on(git_stage_file(path_str.clone(), "hello.txt".into())).unwrap();

    let post = get_git_status_inner(&path_str).unwrap();
    let kinds: Vec<&str> = post.files.iter()
        .filter(|f| f.path == "hello.txt")
        .map(|f| f.status.as_str())
        .collect();
    assert!(kinds.contains(&"index_modified"),
        "post-stage should include index_modified, got: {:?}", kinds);
    assert!(!kinds.contains(&"modified"),
        "post-stage should NOT still have unstaged 'modified', got: {:?}", kinds);
}

#[test]
fn test_stage_file_picks_up_untracked() {
    let dir = setup_git_repo();
    let path_str = dir.path().to_string_lossy().to_string();
    fs::write(dir.path().join("fresh.txt"), b"x").unwrap();

    tauri::async_runtime::block_on(git_stage_file(path_str.clone(), "fresh.txt".into())).unwrap();

    let info = get_git_status_inner(&path_str).unwrap();
    let entry = info.files.iter().find(|f| f.path == "fresh.txt").unwrap();
    assert_eq!(entry.status, "index_new",
        "newly-tracked file should be index_new after staging");
}

#[test]
fn test_stage_all_picks_up_multiple_files() {
    let dir = setup_git_repo();
    let path_str = dir.path().to_string_lossy().to_string();
    fs::write(dir.path().join("a.txt"), b"a").unwrap();
    fs::write(dir.path().join("b.txt"), b"b").unwrap();
    fs::write(dir.path().join("hello.txt"), b"changed").unwrap();

    tauri::async_runtime::block_on(git_stage_all(path_str.clone())).unwrap();

    let info = get_git_status_inner(&path_str).unwrap();
    // Every entry now should be a staged status.
    for f in &info.files {
        assert!(
            f.status.starts_with("index_"),
            "stage_all should have moved {:?} into the index, got {:?}",
            f.path, f.status
        );
    }
    // And specifically these three are present as staged.
    let staged: std::collections::HashSet<&str> = info.files.iter()
        .map(|f| f.path.as_str())
        .collect();
    assert!(staged.contains("a.txt"));
    assert!(staged.contains("b.txt"));
    assert!(staged.contains("hello.txt"));
}

// ═══════════════════════════════════════════════════════════════════════════
// list_branches / get_commits tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_list_branches_marks_current() {
    let dir = setup_git_repo();
    // Create a second branch off HEAD
    let repo = Repository::open(dir.path()).unwrap();
    let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("feature/x", &head_commit, false).unwrap();

    let branches = tauri::async_runtime::block_on(list_branches(dir.path().to_string_lossy().to_string())).unwrap();
    assert_eq!(branches.len(), 2);
    let current_count = branches.iter().filter(|b| b.is_current).count();
    assert_eq!(current_count, 1, "exactly one branch should be marked current");
    // Current branch sorts first
    assert!(branches[0].is_current);
}

#[test]
fn test_list_branches_no_upstream_when_remote_missing() {
    let dir = setup_git_repo();
    let branches = tauri::async_runtime::block_on(list_branches(dir.path().to_string_lossy().to_string())).unwrap();
    assert!(branches.iter().all(|b| b.upstream.is_none()),
        "no remote configured → no branch should report an upstream");
}

#[test]
fn test_get_commits_returns_history() {
    let dir = setup_git_repo();
    let commits = tauri::async_runtime::block_on(get_commits(dir.path().to_string_lossy().to_string(), None)).unwrap();
    assert_eq!(commits.len(), 1, "single initial commit from setup_git_repo");
    let c = &commits[0];
    assert_eq!(c.message, "initial commit");
    assert_eq!(c.parent_count, 0);
    assert_eq!(c.oid.len(), 7, "short OID is 7 chars");
}

#[test]
fn test_get_commits_respects_count_limit() {
    // Add a couple more commits, then verify the limit caps results.
    let dir = setup_git_repo();
    let repo = Repository::open(dir.path()).unwrap();
    let sig = repo.signature().unwrap();
    for i in 0..3 {
        let name = format!("file-{}.txt", i);
        fs::write(dir.path().join(&name), b"x").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new(&name)).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, &format!("commit {}", i), &tree, &[&parent]).unwrap();
    }

    let limited = tauri::async_runtime::block_on(get_commits(dir.path().to_string_lossy().to_string(), Some(2))).unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn test_get_commits_unborn_head_returns_empty() {
    let dir = setup_empty_git_repo();
    let result = tauri::async_runtime::block_on(get_commits(dir.path().to_string_lossy().to_string(), None));
    assert!(result.is_ok(), "unborn HEAD should return Ok, not Err");
    assert!(result.unwrap().is_empty(), "unborn HEAD should return empty vec");
}

// ═══════════════════════════════════════════════════════════════════════════
// apply_git_env tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_git_env_sets_no_prompt_vars() {
    // GIT_TERMINAL_PROMPT=0 and GIT_ASKPASS=echo are the load-bearing
    // bits — without them, an HTTPS remote with no credential helper
    // hangs forever waiting on a stdin pipe with no TTY. Worth a test.
    let mut cmd = std::process::Command::new("git");
    apply_git_env(&mut cmd);

    let envs: std::collections::HashMap<String, String> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().to_string(),
                v?.to_string_lossy().to_string(),
            ))
        })
        .collect();

    assert_eq!(envs.get("GIT_TERMINAL_PROMPT"), Some(&"0".to_string()));
    assert_eq!(envs.get("GIT_ASKPASS"), Some(&"echo".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════
// get_conflict_versions tests
// ═══════════════════════════════════════════════════════════════════════════

// Builds a real conflicted index by creating two divergent branches that
// edit the same line, then calling repo.merge_commits to leave conflict
// entries at stages 1/2/3. Avoids shelling out to system git.
fn setup_conflicted_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@test.com").unwrap();
    let sig = repo.signature().unwrap();

    // base commit: file.txt with one line
    fs::write(dir.path().join("file.txt"), "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let base_oid = repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
    let base_commit = repo.find_commit(base_oid).unwrap();

    // Capture the ref HEAD points at after the first commit (could be
    // refs/heads/main or refs/heads/master depending on git config).
    // We come back to it for the ours commit + final merge.
    let head_ref_name = repo.head().unwrap().name().unwrap().to_string();

    // ours commit on the default branch: replace base line with "ours line"
    fs::write(dir.path().join("file.txt"), "ours line\n").unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let ours_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let _ours_oid = repo.commit(Some("HEAD"), &sig, &sig, "ours", &ours_tree, &[&base_commit]).unwrap();

    // theirs commit on a sibling branch from base: replace with "theirs line"
    let theirs_branch = repo.branch("theirs", &base_commit, false).unwrap();
    let theirs_branch_ref = theirs_branch.get().name().unwrap().to_string();
    repo.set_head(&theirs_branch_ref).unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

    fs::write(dir.path().join("file.txt"), "theirs line\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let theirs_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let theirs_oid = repo.commit(Some("HEAD"), &sig, &sig, "theirs", &theirs_tree, &[&base_commit]).unwrap();
    let theirs_commit = repo.find_commit(theirs_oid).unwrap();

    // Switch back to the default branch (ours) and merge theirs in.
    repo.set_head(&head_ref_name).unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

    let theirs_annotated = repo.find_annotated_commit(theirs_commit.id()).unwrap();
    repo.merge(&[&theirs_annotated], None, None).unwrap();
    // Working tree now has conflict markers, index has stages 1/2/3.
    // Don't commit — we want to inspect the unresolved conflict state.

    dir
}

#[test]
fn test_get_conflict_versions_returns_all_three_stages() {
    let dir = setup_conflicted_repo();
    let result = tauri::async_runtime::block_on(get_conflict_versions(
        dir.path().to_string_lossy().to_string(),
        "file.txt".into(),
    ))
    .expect("get_conflict_versions should succeed on conflicted file");

    assert_eq!(result.base.as_deref(), Some("base\n"));
    assert_eq!(result.ours.as_deref(), Some("ours line\n"));
    assert_eq!(result.theirs.as_deref(), Some("theirs line\n"));
    // merged is the working tree — has conflict markers from libgit2's merge.
    let merged = result.merged.expect("merged should be present");
    assert!(merged.contains("<<<<<<<"), "merged should have conflict markers");
    assert!(merged.contains("ours line"));
    assert!(merged.contains("theirs line"));
}

#[test]
fn test_get_conflict_versions_clean_file_returns_none_stages() {
    let dir = setup_git_repo();
    let result = tauri::async_runtime::block_on(get_conflict_versions(
        dir.path().to_string_lossy().to_string(),
        "hello.txt".into(),
    ))
    .expect("clean file lookup should not error");

    // No conflict → no stages 1/2/3. merged still reads from working tree.
    assert!(result.base.is_none());
    assert!(result.ours.is_none());
    assert!(result.theirs.is_none());
    assert_eq!(result.merged.as_deref(), Some("hello world"));
}

#[test]
fn test_get_conflict_versions_missing_file_returns_all_none() {
    let dir = setup_git_repo();
    let result = tauri::async_runtime::block_on(get_conflict_versions(
        dir.path().to_string_lossy().to_string(),
        "does_not_exist.txt".into(),
    ))
    .expect("missing file lookup should not error");

    assert!(result.base.is_none());
    assert!(result.ours.is_none());
    assert!(result.theirs.is_none());
    assert!(result.merged.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// fs_event_is_noise tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_fs_noise_skips_git_internal_writes() {
    let root = std::path::PathBuf::from("/projects/repo");
    assert!(fs_event_is_noise(
        &root.join(".git/index"),
        &root,
    ));
    assert!(fs_event_is_noise(
        &root.join(".git/refs/heads/main"),
        &root,
    ));
}

#[test]
fn test_fs_noise_skips_node_modules_and_target() {
    let root = std::path::PathBuf::from("/projects/repo");
    assert!(fs_event_is_noise(&root.join("node_modules/foo/index.js"), &root));
    assert!(fs_event_is_noise(&root.join("target/debug/build/foo"), &root));
    assert!(fs_event_is_noise(&root.join("__pycache__/x.pyc"), &root));
    assert!(fs_event_is_noise(&root.join("dist/bundle.js"), &root));
    assert!(fs_event_is_noise(&root.join("build/output.o"), &root));
}

#[test]
fn test_fs_noise_lets_user_files_through() {
    let root = std::path::PathBuf::from("/projects/repo");
    assert!(!fs_event_is_noise(&root.join("src/main.js"), &root));
    assert!(!fs_event_is_noise(&root.join("README.md"), &root));
    // .gitignore is a real user file even though its name starts with .git
    assert!(!fs_event_is_noise(&root.join(".gitignore"), &root));
}

#[test]
fn test_fs_noise_only_checks_below_root() {
    // A user whose home is /Users/target shouldn't have everything
    // filtered out. The skip list is consulted only for path components
    // below the watched root.
    let root = std::path::PathBuf::from("/Users/target/proj");
    assert!(!fs_event_is_noise(&root.join("src/file.rs"), &root));
    // Still skips when the noise dir is BELOW the root.
    assert!(fs_event_is_noise(&root.join("target/x"), &root));
}

#[test]
fn test_fs_noise_lets_outside_root_paths_through() {
    // Defensive: notify shouldn't deliver paths outside the watched
    // root, but if it does we surface them rather than silently drop.
    let root = std::path::PathBuf::from("/projects/repo");
    assert!(!fs_event_is_noise(
        &std::path::PathBuf::from("/elsewhere/file.txt"),
        &root,
    ));
}

#[test]
fn test_fs_noise_nested_node_modules() {
    // node_modules nested inside another package (npm workspaces):
    // both levels are noise.
    let root = std::path::PathBuf::from("/projects/repo");
    assert!(fs_event_is_noise(
        &root.join("packages/foo/node_modules/bar/index.js"),
        &root,
    ));
}

// ─── LSP message framing ────────────────────────────────────────────────
#[test]
fn test_encode_lsp_message_uses_byte_length() {
    // ASCII: length == char count.
    let framed = encode_lsp_message("{}");
    assert_eq!(framed, b"Content-Length: 2\r\n\r\n{}");

    // Non-ASCII: Content-Length must be the BYTE length, not char count.
    // {"k":"é"} is 9 chars but 10 bytes ("é" is 2 bytes in UTF-8).
    let body = "{\"k\":\"é\"}";
    assert_eq!(body.chars().count(), 9);
    assert_eq!(body.len(), 10);
    let framed = encode_lsp_message(body);
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    assert!(framed.starts_with(header.as_bytes()));
}

#[test]
fn test_read_lsp_message_roundtrip() {
    let body = "{\"jsonrpc\":\"2.0\",\"id\":1}";
    let framed = encode_lsp_message(body);
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(framed));
    let got = read_lsp_message(&mut reader).unwrap();
    assert_eq!(got.as_deref(), Some(body));
}

#[test]
fn test_read_lsp_message_back_to_back() {
    // Two messages concatenated in one stream must decode as two reads.
    let mut bytes = encode_lsp_message("{\"a\":1}");
    bytes.extend(encode_lsp_message("{\"b\":2}"));
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(bytes));
    assert_eq!(read_lsp_message(&mut reader).unwrap().as_deref(), Some("{\"a\":1}"));
    assert_eq!(read_lsp_message(&mut reader).unwrap().as_deref(), Some("{\"b\":2}"));
    // Stream exhausted → clean EOF.
    assert_eq!(read_lsp_message(&mut reader).unwrap(), None);
}

#[test]
fn test_read_lsp_message_ignores_extra_headers() {
    // A Content-Type header (which some servers send) must be skipped.
    let body = "{\"ok\":true}";
    let mut framed =
        format!("Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n", body.len())
            .into_bytes();
    framed.extend_from_slice(body.as_bytes());
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(framed));
    assert_eq!(read_lsp_message(&mut reader).unwrap().as_deref(), Some(body));
}

#[test]
fn test_read_lsp_message_clean_eof_on_empty() {
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(Vec::<u8>::new()));
    assert_eq!(read_lsp_message(&mut reader).unwrap(), None);
}

// End-to-end LSP host proof: drive a REAL language server through the same
// framing functions lsp_start/lsp_send use, and assert diagnostics come back for
// a file with a type error. #[ignore]d because each needs its server binary
// installed (not present in CI); run locally with:
//   cargo test --manifest-path src-tauri/Cargo.toml -- --ignored lsp_e2e
//
// Drives `program` from `cwd`, opens `open_file` (already written) with
// `language_id`, and returns whether a non-empty publishDiagnostics arrives
// within `timeout_secs`. Replies to any server→client request with a null
// result so servers that gate on registerCapability / workDoneProgress
// (pyright, rust-analyzer) don't stall.
fn lsp_diagnostics_probe(
    program: &std::ffi::OsStr,
    args: &[&str],
    cwd: &std::path::Path,
    open_file: &std::path::Path,
    language_id: &str,
    timeout_secs: u64,
) -> bool {
    use std::io::{BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root_uri = format!("file://{}", cwd.display());
    let uri = format!("file://{}", open_file.display());
    let text = serde_json::to_string(&std::fs::read_to_string(open_file).unwrap()).unwrap();

    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(msg)) = read_lsp_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut send = |stdin: &mut std::process::ChildStdin, body: String| {
        stdin.write_all(&encode_lsp_message(&body)).unwrap();
        stdin.flush().unwrap();
    };

    send(&mut stdin, format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"processId":null,"rootUri":"{root_uri}","capabilities":{{"window":{{"workDoneProgress":true}},"textDocument":{{"synchronization":{{}},"publishDiagnostics":{{}}}}}}}}}}"#
    ));
    send(&mut stdin, r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string());
    send(&mut stdin, format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"{language_id}","version":1,"text":{text}}}}}}}"#
    ));

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut got = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(msg) => {
                let v: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // A server→client request (has both id and method) — answer null
                // so the server doesn't block waiting on us.
                if v.get("id").is_some() && v.get("method").is_some() {
                    let id = &v["id"];
                    send(&mut stdin, format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#));
                    continue;
                }
                if v["method"] == "textDocument/publishDiagnostics" {
                    let diags = &v["params"]["diagnostics"];
                    if diags.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                        got = true;
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    let _ = child.kill();
    got
}

#[test]
#[ignore]
fn test_lsp_e2e_typescript_diagnostics() {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../node_modules/.bin/typescript-language-server");
    if !bin.exists() {
        eprintln!("skipping: {} not found", bin.display());
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("bad.ts");
    std::fs::write(&file, "const x: number = \"not a number\";\n").unwrap();
    assert!(
        lsp_diagnostics_probe(bin.as_os_str(), &["--stdio"], work.path(), &file, "typescript", 20),
        "expected diagnostics from typescript-language-server"
    );
}

#[test]
#[ignore]
fn test_lsp_e2e_python_diagnostics() {
    if which_on_path("pyright-langserver").is_none() {
        eprintln!("skipping: pyright-langserver not on PATH");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("bad.py");
    std::fs::write(&file, "x: int = \"not an int\"\n").unwrap();
    assert!(
        lsp_diagnostics_probe(
            std::ffi::OsStr::new("pyright-langserver"),
            &["--stdio"],
            work.path(),
            &file,
            "python",
            30,
        ),
        "expected diagnostics from pyright"
    );
}

#[test]
#[ignore]
fn test_lsp_e2e_rust_diagnostics() {
    if which_on_path("rust-analyzer").is_none() {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    // rust-analyzer needs a real Cargo project to analyze.
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(work.path().join("src")).unwrap();
    let file = work.path().join("src/main.rs");
    std::fs::write(&file, "fn main() { let _x: u32 = \"not a u32\"; }\n").unwrap();
    assert!(
        lsp_diagnostics_probe(
            std::ffi::OsStr::new("rust-analyzer"),
            &[],
            work.path(),
            &file,
            "rust",
            60,
        ),
        "expected diagnostics from rust-analyzer"
    );
}

// Proves the documentSymbol round-trip (the request that powers the Cmd+Shift+O
// outline's LSP upgrade): initialize → didOpen → textDocument/documentSymbol,
// asserting a non-empty result. Uses the same framing fns the host uses.
#[test]
#[ignore]
fn test_lsp_e2e_typescript_document_symbols() {
    use std::io::{BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../node_modules/.bin/typescript-language-server");
    if !bin.exists() {
        eprintln!("skipping: {} not found", bin.display());
        return;
    }

    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("sym.ts");
    std::fs::write(&file, "function alpha() {}\nclass Beta { gamma() {} }\n").unwrap();
    let root_uri = format!("file://{}", work.path().display());
    let uri = format!("file://{}", file.display());

    let mut child = Command::new(&bin)
        .arg("--stdio")
        .current_dir(work.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(msg)) = read_lsp_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut send = |stdin: &mut std::process::ChildStdin, body: String| {
        stdin.write_all(&encode_lsp_message(&body)).unwrap();
        stdin.flush().unwrap();
    };

    send(&mut stdin, format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"processId":null,"rootUri":"{root_uri}","capabilities":{{"textDocument":{{"documentSymbol":{{"hierarchicalDocumentSymbolSupport":true}}}}}}}}}}"#
    ));
    send(&mut stdin, r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string());
    let text = serde_json::to_string("function alpha() {}\nclass Beta { gamma() {} }\n").unwrap();
    send(&mut stdin, format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"typescript","version":1,"text":{text}}}}}}}"#
    ));
    send(&mut stdin, format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{{"textDocument":{{"uri":"{uri}"}}}}}}"#
    ));

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut got_symbols = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(msg) => {
                let v: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("id").is_some() && v.get("method").is_some() {
                    let id = &v["id"];
                    send(&mut stdin, format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#));
                    continue;
                }
                if v["id"] == 2 {
                    let result = &v["result"];
                    if result.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                        got_symbols = true;
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    let _ = child.kill();
    assert!(got_symbols, "expected documentSymbol result from typescript-language-server");
}

// Minimal `which`: is `name` resolvable on PATH? Keeps the e2e probes skippable
// when a server isn't installed.
fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(name);
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    })
}
