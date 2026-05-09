//! In-process integration with launchpad-agent.
//!
//! The agent runtime (`lpa_server::ServerRuntime`) lives inside this binary.
//! Per-Tauri-window connections are registered against the shared runtime;
//! each window's outbound JSON envelopes are forwarded as Tauri events on
//! `agent:event:<window_label>`.

mod commands;
mod config;
mod host;
mod native_tools;

pub use commands::*;
pub use config::*;
pub use host::AgentState;
