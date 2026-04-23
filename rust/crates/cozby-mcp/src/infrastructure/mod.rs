//! Infrastructure — реализации портов application-слоя.

pub mod config;
pub mod fs_adapter;
pub mod mcp_bridge;

#[cfg(test)]
pub(crate) mod in_memory_fs;

pub use config::{parse_args, Args, ConfigError};
pub use fs_adapter::StdFileSystem;
pub use mcp_bridge::build_spec;
