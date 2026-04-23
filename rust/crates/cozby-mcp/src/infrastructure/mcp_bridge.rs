//! Мост между application-слоем (use-cases, порт `FileSystem`) и
//! транспортной машиной MCP в крейте `runtime` (`McpServer`).
//!
//! Сюда вынесена вся специфика `runtime::McpTool` / `runtime::McpServerSpec`:
//! application и domain про неё ничего не знают.

use std::path::PathBuf;
use std::sync::Arc;

use runtime::{McpServerSpec, McpTool, ToolCallHandler};

use crate::application::ports::FileSystem;
use crate::application::use_cases;
use crate::domain::tools::{tool_descriptors, ToolDescriptor};

/// Имя и версия, которые сервер сообщает клиенту по `initialize`.
pub const SERVER_NAME: &str = "cozby-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn descriptor_to_mcp_tool(descriptor: &ToolDescriptor) -> McpTool {
    McpTool {
        name: descriptor.kind.as_str().to_string(),
        description: Some(descriptor.description.to_string()),
        input_schema: Some(descriptor.input_schema.clone()),
        annotations: Some(descriptor.annotations.clone()),
        meta: None,
    }
}

fn build_handler(root: PathBuf, fs: Arc<dyn FileSystem>) -> ToolCallHandler {
    Box::new(move |name, args| {
        use_cases::dispatch(fs.as_ref(), &root, name, args).map_err(|error| error.to_string())
    })
}

/// Собирает `McpServerSpec`, готовый к передаче в `runtime::McpServer::new`.
#[must_use]
pub fn build_spec(root: PathBuf, fs: Arc<dyn FileSystem>) -> McpServerSpec {
    let tools = tool_descriptors()
        .iter()
        .map(descriptor_to_mcp_tool)
        .collect();

    McpServerSpec {
        server_name: SERVER_NAME.to_string(),
        server_version: SERVER_VERSION.to_string(),
        tools,
        tool_handler: build_handler(root, fs),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_spec, SERVER_NAME, SERVER_VERSION};
    use crate::infrastructure::in_memory_fs::InMemoryFs;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn spec_exposes_configured_server_identity() {
        let root = PathBuf::from("/r");
        let fs: Arc<dyn crate::application::ports::FileSystem> = Arc::new(InMemoryFs::new(&root));
        let spec = build_spec(root.clone(), fs);
        assert_eq!(spec.server_name, SERVER_NAME);
        assert_eq!(spec.server_version, SERVER_VERSION);
        assert_eq!(spec.tools.len(), 4);
    }

    #[test]
    fn handler_dispatches_to_use_cases() {
        let root = PathBuf::from("/r");
        let mut fs_impl = InMemoryFs::new(&root);
        fs_impl.insert_file("a.txt", b"hi");
        let fs: Arc<dyn crate::application::ports::FileSystem> = Arc::new(fs_impl);
        let spec = build_spec(root, fs);

        let out = (spec.tool_handler)("read_file", &serde_json::json!({"path": "a.txt"})).unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn handler_surfaces_domain_errors_as_strings() {
        let root = PathBuf::from("/r");
        let fs: Arc<dyn crate::application::ports::FileSystem> = Arc::new(InMemoryFs::new(&root));
        let spec = build_spec(root, fs);

        let err =
            (spec.tool_handler)("read_file", &serde_json::json!({})).expect_err("must error");
        assert!(err.contains("missing required argument"));
    }
}
