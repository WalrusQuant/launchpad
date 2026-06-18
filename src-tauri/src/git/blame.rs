use crate::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct BlameHunk {
    pub(crate) start_line: usize,
    pub(crate) line_count: usize,
    pub(crate) oid: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) summary: String,
}

// Per-line authorship for a file, as contiguous hunks (a run of lines sharing
// the same commit). Blames the committed file (HEAD); local unsaved edits won't
// be attributed and may shift line alignment — blame is an opt-in, best-effort
// overlay. A zero/uncommitted OID (e.g. a not-yet-committed line) surfaces with
// the all-zero short id so the frontend can render it as "uncommitted".
#[tauri::command]
pub(crate) async fn git_blame_file(path: String, file_path: String) -> Result<Vec<BlameHunk>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let blame = repo
            .blame_file(std::path::Path::new(&file_path), None)
            .map_err(|e| e.to_string())?;

        let mut out = Vec::with_capacity(blame.len());
        for hunk in blame.iter() {
            let oid = hunk.final_commit_id();
            let oid_str = oid.to_string();
            let short = if oid_str.len() >= 7 { oid_str[..7].to_string() } else { oid_str };
            let sig = hunk.final_signature();
            let author = sig.name().unwrap_or("Unknown").to_string();
            let timestamp = sig.when().seconds();
            let summary = repo
                .find_commit(oid)
                .ok()
                .and_then(|c| c.summary().map(String::from))
                .unwrap_or_default();
            out.push(BlameHunk {
                start_line: hunk.final_start_line(),
                line_count: hunk.lines_in_hunk(),
                oid: short,
                author,
                timestamp,
                summary,
            });
        }
        Ok(out)
    })
    .await
}
