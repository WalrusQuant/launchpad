// Shared event-name constants. Centralized so a typo at the emit OR listen
// side becomes a JS reference error instead of a silently-dropped event.
//
// Tauri events: emitted from Rust (src-tauri/src/lib.rs) and listened to via
// `@tauri-apps/api/event`'s listen(). The string values MUST match the
// emit() calls in lib.rs — Rust has its own copies as `pub const` near the
// emit sites and tests would catch a drift.
export const PTY_OUTPUT = "pty-output";
export const PTY_EXIT = "pty-exit";
export const FS_CHANGED = "fs-changed";

// DOM CustomEvents dispatched on `window` for cross-module signaling
// inside the frontend. Not crossing the IPC boundary, but using constants
// here too keeps the policy uniform.
export const PATH_RENAMED = "launchpad:path-renamed";
export const PANEL_TRANSITION_DONE = "panel-transition-done";
// HEAD moved (commit / amend) without the working file changing on disk — open
// editors must recompute their change gutter (disk-vs-HEAD) since fs-changed
// won't fire for a commit.
export const HEAD_MOVED = "launchpad:head-moved";
