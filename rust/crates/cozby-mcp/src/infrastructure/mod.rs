//! Infrastructure — реализации портов application-слоя.

pub mod config;
pub mod contract_loader;
pub mod fs_adapter;
pub mod http_transport;
pub mod mcp_bridge;

#[cfg(test)]
pub(crate) mod in_memory_fs;

pub use config::{parse_args, Args, ConfigError, BRAIN_URL_ENV, CONTRACTS_ENV};
pub use contract_loader::{builtin_brain_contract, load_contract_file};
pub use fs_adapter::StdFileSystem;
pub use http_transport::ReqwestTransport;
pub use mcp_bridge::build_spec;
