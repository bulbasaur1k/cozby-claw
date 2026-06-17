//! `cozby-mcp` — standalone pure-Rust MCP stdio server.
//!
//! Организован по гексагональной (clean) архитектуре:
//!
//! * [`domain`] — чистые правила: валидация путей, лимиты, описания
//!   инструментов. Никаких `tokio` / `reqwest` / `std::fs` здесь.
//! * [`application`] — порты (trait [`FileSystem`](crate::application::FileSystem))
//!   и use-case'ы, работающие только через порты.
//! * [`infrastructure`] — адаптеры: `StdFileSystem`, парсер argv,
//!   мост к `runtime::McpServer`.
//! * [`bootstrap`] — склейка для `main.rs` и интеграционных тестов.
//!
//! Направление зависимостей: `infrastructure → application → domain`.

pub mod application;
pub mod bootstrap;
pub mod domain;
pub mod infrastructure;

pub use bootstrap::{assemble_spec, wire_server};
pub use infrastructure::{
    builtin_brain_contract, load_contract_file, parse_args, Args, ConfigError, ReqwestTransport,
    StdFileSystem,
};
