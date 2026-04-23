//! Точка склейки слоёв. Принимает уже разобранный `Args`, собирает
//! `McpServerSpec` и возвращает его вместе с tokio-рантаймом.
//!
//! Запускать сервер (`server.run().await`) — задача `main.rs`, чтобы
//! bootstrap оставался полностью тестируемым без захвата stdin/stdout.

use std::sync::Arc;

use runtime::{McpServer, McpServerSpec};

use crate::application::ports::FileSystem;
use crate::infrastructure::{build_spec, Args};

/// Готовый к запуску MCP-сервер поверх переданного адаптера `FileSystem`.
#[must_use]
pub fn wire_server(args: Args, fs: Arc<dyn FileSystem>) -> McpServer {
    McpServer::new(assemble_spec(args, fs))
}

/// Отдельно от `wire_server`, чтобы тесты проверяли сборку спецификации без
/// инстанцирования рантайм-сервера (который забирает stdin/stdout).
#[must_use]
pub fn assemble_spec(args: Args, fs: Arc<dyn FileSystem>) -> McpServerSpec {
    build_spec(args.root, fs)
}

#[cfg(test)]
mod tests {
    use super::assemble_spec;
    use crate::infrastructure::{in_memory_fs::InMemoryFs, Args};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn assembles_spec_with_declared_tools() {
        let root = PathBuf::from("/r");
        let fs: Arc<dyn crate::application::ports::FileSystem> = Arc::new(InMemoryFs::new(&root));
        let args = Args { root: root.clone() };
        let spec = assemble_spec(args, fs);
        let names: Vec<_> = spec.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["read_file", "list_dir", "glob", "grep"]);
    }
}
