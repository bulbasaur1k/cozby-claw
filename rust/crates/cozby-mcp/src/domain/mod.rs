//! Domain layer — чистые функции и правила. Ноль зависимостей от tokio,
//! файловой системы или сетевых крейтов.

pub mod contract;
pub mod errors;
pub mod limits;
pub mod path_guard;
pub mod tools;

pub use contract::{
    Contract, ContractParam, ContractTool, HttpMethod, ParamLocation, PreparedRequest,
};
pub use errors::DomainError;
pub use limits::{format_read_body, ReadLimit, MAX_GREP_MATCHES, MAX_READ_BYTES};
pub use path_guard::ensure_under_root;
pub use tools::{tool_descriptors, ToolDescriptor, ToolKind};
