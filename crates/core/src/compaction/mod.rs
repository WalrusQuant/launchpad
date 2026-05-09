//! Context compaction pipeline.
//!
//! This module owns the selection, prompt construction, and (in later phases)
//! execution of LLM-based context compaction. See `docs/spec-context-management.md`
//! for the full contract. Phase 1 introduces:
//!
//! - [`EligibilitySelector`] — picks which prefix of a message list is eligible
//!   for summarization while preserving the last `K` complete turns and tool
//!   call/result pair invariants.
//! - [`COMPACTION_SYSTEM_PROMPT`] and [`build_compaction_user_prompt`] — the
//!   summarization prompt contract.
//! - [`serialize_transcript`] — compact transcript rendering for the summarizer.
//!
//! Later phases (Phase 2+) add the concrete `LlmContextCompactor`, snapshot
//! persistence, prompt-view rebuild, and query-loop integration.

mod llm_compactor;
mod prompt;
mod prompt_view;
mod runner;
mod selector;
mod snapshots;

pub use llm_compactor::{DEFAULT_SUMMARY_MAX_OUTPUT_TOKENS, LlmContextCompactor};
pub use prompt::{
    COMPACTION_SYSTEM_PROMPT, CompactionPromptInput, build_compaction_user_prompt,
    serialize_transcript,
};
pub use prompt_view::{
    ActiveCompaction, COMPACTED_SUMMARY_FOOTER, COMPACTED_SUMMARY_HEADER, rebuild_prompt_view,
    summary_message,
};
pub use runner::{CompactionOutcome, run_llm_compaction, warn_compaction_failed};
pub use selector::{EligibilitySelector, EligibleRange};
pub use snapshots::SnapshotStore;
