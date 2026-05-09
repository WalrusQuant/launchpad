//! Per-server supervisor that wraps an `McpClient` with lifecycle management.

pub mod catalog;
pub mod handle;
pub mod supervisor;

pub use catalog::ServerCatalog;
pub use handle::ServerHandle;
pub use supervisor::ServerSupervisor;

#[cfg(test)]
mod tests;
