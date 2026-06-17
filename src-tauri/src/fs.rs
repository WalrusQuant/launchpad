// Filesystem operations: directory listing, fuzzy file + content search,
// file preview/read, CRUD (create/delete/rename), inode lookup for external
// rename tracking, and native macOS helpers (folder picker, reveal in Finder,
// open-with-default-app). Heavier walks run off the IPC thread via `blocking`.
use crate::*;
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;

#[derive(Clone, Serialize)]
pub(crate) struct FileEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) is_dir: bool,
    pub(crate) is_hidden: bool,
    pub(crate) size: u64,
    pub(crate) modified: Option<u64>,
    pub(crate) mode: u32,
}

#[tauri::command]
pub(crate) async fn read_directory(path: String) -> Result<Vec<FileEntry>, String> {
    blocking(move || {
        let entries = fs::read_dir(&path).map_err(|e| e.to_string())?;

        let mut files: Vec<FileEntry> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                // Use to_string_lossy so non-UTF-8 filenames (rare on macOS but
                // possible) still show up with replacement characters rather
                // than disappearing from the listing entirely.
                let name = entry.file_name().to_string_lossy().to_string();
                let is_hidden = name.starts_with('.');
                // Don't drop the entry if metadata fails (permission denied,
                // or TOCTOU where the file was removed between read_dir and
                // metadata). Fall through to sensible defaults so the user
                // still sees what's there.
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = metadata
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let mode = metadata
                    .as_ref()
                    .map(|m| m.permissions().mode())
                    .unwrap_or(0);

                Some(FileEntry {
                    name,
                    path: entry.path().to_string_lossy().to_string(),
                    is_dir,
                    is_hidden,
                    size,
                    modified,
                    mode,
                })
            })
            .collect();

        files.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(files)
    })
    .await
}

// Recursive file search for quick open
#[tauri::command]
pub(crate) async fn search_files(root: String, query: String, max_results: Option<usize>) -> Result<Vec<String>, String> {
    blocking(move || {
        let limit = max_results.unwrap_or(50);
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        let root_path = std::path::Path::new(&root);

        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            query: &str,
            results: &mut Vec<String>,
            limit: usize,
            depth: usize,
        ) {
            if depth > 10 || results.len() >= limit {
                return;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            // Sort siblings so walk order is deterministic. fs::read_dir's order
            // is filesystem-dependent; with a result limit, a non-deterministic
            // walk makes "which 20 files appear first" vary run-to-run.
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if results.len() >= limit {
                    return;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip hidden dirs and common large dirs
                if name.starts_with('.') || name == "node_modules" || name == "target" || name == "__pycache__" || name == "dist" || name == "build" {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, query, results, limit, depth + 1);
                } else if name.to_lowercase().contains(query) {
                    let relative = path.strip_prefix(root).unwrap_or(&path);
                    results.push(relative.to_string_lossy().to_string());
                }
            }
        }

        walk(root_path, root_path, &query_lower, &mut results, limit, 0);
        results.sort_by(|a, b| a.len().cmp(&b.len()));
        Ok(results)
    })
    .await
}

#[derive(Serialize)]
pub(crate) struct SearchHit {
    file: String,         // relative path from root
    line: u32,            // 1-indexed
    column: u32,          // 1-indexed column of match start (char index)
    match_length: u32,
    line_content: String, // full line, truncated to 500 chars
}

// Project-wide content search. Walks the tree applying the same skip rules as
// search_files, reads text files under a size cap, and reports matches.
#[tauri::command]
pub(crate) async fn search_in_files(
    root: String,
    query: String,
    case_sensitive: bool,
    is_regex: bool,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    blocking(move || {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = max_results.unwrap_or(500);
        let root_path = std::path::Path::new(&root);

        // Compile pattern once.
        let pattern_src = if is_regex {
            query.clone()
        } else {
            regex::escape(&query)
        };
        let pattern = regex::RegexBuilder::new(&pattern_src)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("Invalid pattern: {}", e))?;

        let mut results: Vec<SearchHit> = Vec::new();

        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            pattern: &regex::Regex,
            results: &mut Vec<SearchHit>,
            limit: usize,
            depth: usize,
        ) {
            if depth > 12 || results.len() >= limit {
                return;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            // Deterministic walk (same reason as search_files): keeps the set
            // of matches visited before hitting `limit` stable across runs.
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if results.len() >= limit {
                    return;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.')
                    || name == "node_modules"
                    || name == "target"
                    || name == "__pycache__"
                    || name == "dist"
                    || name == "build"
                {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, pattern, results, limit, depth + 1);
                    continue;
                }
                // File size cap: skip anything over 2 MB to keep latency bounded.
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if metadata.len() > 2 * 1024 * 1024 {
                    continue;
                }
                let bytes = match fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // Binary detection: null byte in the first 512 bytes.
                if bytes.iter().take(512).any(|b| *b == 0) {
                    continue;
                }
                let content = match std::str::from_utf8(&bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
                for (line_idx, line) in content.lines().enumerate() {
                    if results.len() >= limit {
                        return;
                    }
                    if let Some(m) = pattern.find(line) {
                        // Count chars up to byte offset for 1-indexed column.
                        let prefix = &line[..m.start()];
                        let column = prefix.chars().count() as u32 + 1;
                        let match_length = line[m.start()..m.end()].chars().count() as u32;
                        let line_content = if line.len() > 500 {
                            let mut end = 500;
                            while !line.is_char_boundary(end) && end > 0 {
                                end -= 1;
                            }
                            format!("{}…", &line[..end])
                        } else {
                            line.to_string()
                        };
                        results.push(SearchHit {
                            file: rel.clone(),
                            line: (line_idx as u32) + 1,
                            column,
                            match_length,
                            line_content,
                        });
                    }
                }
            }
        }

        walk(root_path, root_path, &pattern, &mut results, limit, 0);
        Ok(results)
    })
    .await
}

#[tauri::command]
pub(crate) fn pick_directory() -> Result<Option<String>, String> {
    let output = std::process::Command::new("osascript")
        .args(["-e", "POSIX path of (choose folder)"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Remove trailing slash
        let path = path.trim_end_matches('/').to_string();
        Ok(Some(path))
    } else {
        // User cancelled
        Ok(None)
    }
}

#[tauri::command]
pub(crate) fn reveal_in_finder(path: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Hand `path` off to the OS to open with the user's default application
// for that file type — the macOS `open` CLI does this. PDF → Preview,
// PNG → Preview, .xcodeproj → Xcode, etc. No-op for paths that don't
// resolve; we surface the `open` command's exit status so the frontend
// can show the actual error rather than a silent failure.
#[tauri::command]
pub(crate) fn open_in_default_app(path: String) -> Result<(), String> {
    let output = std::process::Command::new("open")
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn read_file_preview(path: String, max_bytes: Option<usize>) -> Result<String, String> {
    blocking(move || {
        let limit = max_bytes.unwrap_or(8192);

        // Stream only up to `limit` bytes so a 50 MB file opened for a 512 KB
        // preview doesn't allocate 50 MB transiently.
        let file = fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut data = Vec::with_capacity(limit.min(64 * 1024));
        file.take(limit as u64)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;

        // UTF-16 BOM: surface a specific, actionable message instead of a
        // generic "binary" refusal. UTF-16 ASCII-dominant text used to trip
        // the old null-byte check as a false positive.
        if data.len() >= 2
            && (data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]))
        {
            return Err("UTF-16 file — re-save as UTF-8 to edit".into());
        }

        // Binary heuristic: real text files rarely contain ANY null bytes, but
        // tolerate a small ratio so source files with null-in-string-literal
        // don't get rejected. UTF-16-like content will have ~50% nulls, far
        // above the 1% threshold. Only apply once we have enough bytes to
        // make the ratio meaningful — a 20-byte file with one null otherwise
        // looks like "5% null" and would be wrongly rejected.
        if data.len() > 64 {
            let null_count = data.iter().filter(|&&b| b == 0).count();
            if null_count * 100 > data.len() {
                return Err("Binary file — cannot display".into());
            }
        }

        Ok(String::from_utf8_lossy(&data).to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn write_file(path: String, content: String) -> Result<(), String> {
    // Size limit to prevent accidental large writes. Checked before
    // spawn_blocking so the rejection round-trip is cheap.
    if content.len() > 10 * 1024 * 1024 {
        return Err("File content exceeds 10MB limit".into());
    }
    blocking(move || {
        atomic_write(std::path::Path::new(&path), content.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn create_file(path: String) -> Result<(), String> {
    blocking(move || {
        if std::path::Path::new(&path).exists() {
            return Err("File already exists".into());
        }
        std::fs::write(&path, "").map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn create_directory(path: String) -> Result<(), String> {
    blocking(move || std::fs::create_dir_all(&path).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub(crate) async fn delete_path(path: String, project_root: Option<String>) -> Result<(), String> {
    // Off-thread because remove_dir_all on a large subtree (e.g. accidental
    // delete of node_modules/) can take seconds and would otherwise block
    // the IPC dispatch thread.
    blocking(move || {
        // Refuse any deletion not proven to be inside a project root. Every
        // caller (the file-browser context menu) passes project_root, so a
        // missing root means an unexpected/foreign caller — refuse rather than
        // reach remove_dir_all unguarded. With a root, the target must canonicalize
        // to a path inside it.
        let root = project_root.ok_or("Refusing to delete: no project root supplied")?;
        let target = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        let root_canon = fs::canonicalize(&root).map_err(|e| e.to_string())?;
        if !target.starts_with(&root_canon) {
            return Err("Refusing to delete path outside project root".into());
        }
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
        } else {
            std::fs::remove_file(&path).map_err(|e| e.to_string())
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    blocking(move || std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())).await
}

// Inode of the file at `path`. Used to relocate editor tabs whose file
// was renamed externally (Finder, `mv`, git operations). Unix rename
// preserves the inode, so capturing it at open time lets us search for
// the same file under its new name when the original path disappears.
#[tauri::command]
pub(crate) async fn get_file_inode(path: String) -> Result<u64, String> {
    blocking(move || {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).map(|m| m.ino()).map_err(|e| e.to_string())
    })
    .await
}

// Walk the project tree looking for a file whose inode matches `inode`.
// Returns the first match. Applies the same skip rules as search_files
// (hidden dirs, node_modules, target, etc.) so a rename outside the
// interesting tree doesn't cost us a giant walk.
#[tauri::command]
pub(crate) async fn find_path_by_inode(root: String, inode: u64) -> Result<Option<String>, String> {
    blocking(move || {
        fn walk(
            dir: &std::path::Path,
            inode: u64,
            found: &mut Option<String>,
            depth: usize,
        ) {
            use std::os::unix::fs::MetadataExt;
            // Bounds match search_files. Project root is depth 0, so this
            // stops before descending into depth 13 (i.e. walks up to 12
            // levels below the root).
            if found.is_some() || depth > 12 {
                return;
            }
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if found.is_some() {
                    return;
                }
                // Avoid an allocation per entry — name is only used for the
                // skip-list check, and Cow<str> compares cleanly.
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.')
                    || name_str == "node_modules"
                    || name_str == "target"
                    || name_str == "__pycache__"
                    || name_str == "dist"
                    || name_str == "build"
                {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    walk(&entry.path(), inode, found, depth + 1);
                } else if metadata.ino() == inode {
                    *found = Some(entry.path().to_string_lossy().into_owned());
                    return;
                }
            }
        }

        let mut found = None;
        walk(std::path::Path::new(&root), inode, &mut found, 0);
        Ok(found)
    })
    .await
}

#[tauri::command]
pub(crate) fn get_home_dir() -> Result<String, String> {
    dirs_home()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}
