//! Точка склейки слоёв. Принимает уже разобранный `Args`, собирает
//! `McpServerSpec` и возвращает его вместе с tokio-рантаймом.
//!
//! Запускать сервер (`server.run().await`) — задача `main.rs`, чтобы
//! bootstrap оставался полностью тестируемым без захвата stdin/stdout.

use std::sync::Arc;

use runtime::{McpServer, McpServerSpec};

use crate::application::ports::{FileSystem, HttpTransport};
use crate::domain::Contract;
use crate::infrastructure::{build_spec, Args};

/// Готовый к запуску MCP-сервер поверх переданных адаптеров и контрактов.
/// Пустой `contracts` → только файловые инструменты (как раньше).
#[must_use]
pub fn wire_server(
    args: Args,
    fs: Arc<dyn FileSystem>,
    transport: Arc<dyn HttpTransport>,
    contracts: Vec<Contract>,
) -> McpServer {
    McpServer::new(assemble_spec(args, fs, transport, contracts))
}

/// Отдельно от `wire_server`, чтобы тесты проверяли сборку спецификации без
/// инстанцирования рантайм-сервера (который забирает stdin/stdout).
#[must_use]
pub fn assemble_spec(
    args: Args,
    fs: Arc<dyn FileSystem>,
    transport: Arc<dyn HttpTransport>,
    contracts: Vec<Contract>,
) -> McpServerSpec {
    build_spec(args.root, fs, transport, contracts)
}

#[cfg(test)]
mod tests {
    use super::assemble_spec;
    use crate::application::ports::HttpTransport;
    use crate::domain::{DomainError, HttpMethod};
    use crate::infrastructure::{in_memory_fs::InMemoryFs, Args};
    use serde_json::Value as JsonValue;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct NoTransport;
    impl HttpTransport for NoTransport {
        fn send(
            &self,
            _method: HttpMethod,
            _url: &str,
            _query: &[(String, String)],
            _headers: &[(String, String)],
            _body: Option<&JsonValue>,
        ) -> Result<JsonValue, DomainError> {
            Ok(JsonValue::Null)
        }
    }

    #[test]
    fn assembles_spec_with_declared_tools() {
        let root = PathBuf::from("/r");
        let fs: Arc<dyn crate::application::ports::FileSystem> = Arc::new(InMemoryFs::new(&root));
        let args = Args {
            root: root.clone(),
            brain_url: None,
            contracts: Vec::new(),
        };
        let transport: Arc<dyn HttpTransport> = Arc::new(NoTransport);
        let spec = assemble_spec(args, fs, transport, Vec::new());
        let names: Vec<_> = spec.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["read_file", "list_dir", "glob", "grep"]);
    }
}
