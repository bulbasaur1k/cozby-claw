//! Мост между application-слоем (use-cases, порты `FileSystem`/`HttpTransport`)
//! и транспортной машиной MCP в крейте `runtime` (`McpServer`).
//!
//! Сюда вынесена вся специфика `runtime::McpTool` / `runtime::McpServerSpec`:
//! application и domain про неё ничего не знают.

use std::path::PathBuf;
use std::sync::Arc;

use runtime::{McpServerSpec, McpTool, ToolCallHandler};
use serde_json::json;

use crate::application::ports::{FileSystem, HttpTransport};
use crate::application::use_cases;
use crate::domain::contract::{input_schema, tool_writes};
use crate::domain::tools::{tool_descriptors, ToolDescriptor};
use crate::domain::{Contract, ToolKind};

/// Имя и версия, которые сервер сообщает клиенту по `initialize`.
pub const SERVER_NAME: &str = "cozby-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn descriptor_to_mcp_tool(descriptor: &ToolDescriptor) -> McpTool {
    McpTool {
        name: descriptor.name.to_string(),
        description: Some(descriptor.description.to_string()),
        input_schema: Some(descriptor.input_schema.clone()),
        annotations: Some(descriptor.annotations.clone()),
        meta: None,
    }
}

/// MCP-инструменты из всех контрактов. Аннотации: всё ходит в сеть
/// (`openWorldHint: true`); не-GET считаются пишущими (`readOnlyHint: false`).
fn contract_tools(contracts: &[Contract]) -> Vec<McpTool> {
    contracts
        .iter()
        .flat_map(|service| {
            service.tools.iter().map(|tool| {
                let read_only = !tool_writes(tool);
                McpTool {
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    input_schema: Some(input_schema(tool)),
                    annotations: Some(json!({
                        "readOnlyHint": read_only,
                        "destructiveHint": false,
                        "openWorldHint": true,
                    })),
                    meta: None,
                }
            })
        })
        .collect()
}

/// Маршрутизатор tool-call'ов. Файловые имена идут в `dispatch`, остальные —
/// в `dispatch_contract` поверх загруженных контрактов. Незнакомое имя → ошибка.
fn build_handler(
    root: PathBuf,
    fs: Arc<dyn FileSystem>,
    transport: Arc<dyn HttpTransport>,
    contracts: Arc<Vec<Contract>>,
) -> ToolCallHandler {
    Box::new(move |name, args| {
        if ToolKind::from_name(name).is_some() {
            use_cases::dispatch(fs.as_ref(), &root, name, args).map_err(|error| error.to_string())
        } else {
            use_cases::dispatch_contract(transport.as_ref(), &contracts, name, args)
                .map_err(|error| error.to_string())
        }
    })
}

/// Собирает `McpServerSpec`. К файловым инструментам добавляются инструменты
/// всех контрактов (cozby-brain и пользовательские TOML). Пустой список
/// контрактов → сервер выставляет только файловые инструменты, как раньше.
#[must_use]
pub fn build_spec(
    root: PathBuf,
    fs: Arc<dyn FileSystem>,
    transport: Arc<dyn HttpTransport>,
    contracts: Vec<Contract>,
) -> McpServerSpec {
    let mut tools: Vec<McpTool> = tool_descriptors().iter().map(descriptor_to_mcp_tool).collect();
    tools.extend(contract_tools(&contracts));

    McpServerSpec {
        server_name: SERVER_NAME.to_string(),
        server_version: SERVER_VERSION.to_string(),
        tools,
        tool_handler: build_handler(root, fs, transport, Arc::new(contracts)),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_spec, SERVER_NAME, SERVER_VERSION};
    use crate::application::ports::{FileSystem, HttpTransport};
    use crate::domain::{DomainError, HttpMethod};
    use crate::infrastructure::contract_loader::builtin_brain_contract;
    use crate::infrastructure::in_memory_fs::InMemoryFs;
    use serde_json::{json, Value as JsonValue};
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn fs_arc(root: &Path) -> Arc<dyn FileSystem> {
        Arc::new(InMemoryFs::new(root))
    }

    /// Транспорт-стаб: возвращает фиксированное тело, сеть не трогает.
    struct StubTransport;
    impl HttpTransport for StubTransport {
        fn send(
            &self,
            _method: HttpMethod,
            _url: &str,
            _query: &[(String, String)],
            _headers: &[(String, String)],
            _body: Option<&JsonValue>,
        ) -> Result<JsonValue, DomainError> {
            Ok(json!({ "data": { "id": "n1", "title": "Hi" } }))
        }
    }

    fn transport_arc() -> Arc<dyn HttpTransport> {
        Arc::new(StubTransport)
    }

    #[test]
    fn spec_exposes_only_fs_tools_without_contracts() {
        let root = PathBuf::from("/r");
        let spec = build_spec(root.clone(), fs_arc(&root), transport_arc(), Vec::new());
        assert_eq!(spec.server_name, SERVER_NAME);
        assert_eq!(spec.server_version, SERVER_VERSION);
        assert_eq!(spec.tools.len(), 4);
    }

    #[test]
    fn contract_tools_are_added() {
        let root = PathBuf::from("/r");
        let contracts = vec![builtin_brain_contract("http://localhost:8081")];
        let spec = build_spec(root.clone(), fs_arc(&root), transport_arc(), contracts);
        assert_eq!(spec.tools.len(), 8);
        let names: Vec<_> = spec.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"save_note"));
        assert!(names.contains(&"recall"));
        // save_note (POST) is writing; search_notes (GET) is read-only.
        let save = spec.tools.iter().find(|t| t.name == "save_note").unwrap();
        assert_eq!(save.annotations.as_ref().unwrap()["readOnlyHint"], false);
        let search = spec.tools.iter().find(|t| t.name == "search_notes").unwrap();
        assert_eq!(search.annotations.as_ref().unwrap()["readOnlyHint"], true);
    }

    #[test]
    fn handler_dispatches_fs_tool() {
        let root = PathBuf::from("/r");
        let mut fs_impl = InMemoryFs::new(&root);
        fs_impl.insert_file("a.txt", b"hi");
        let fs: Arc<dyn FileSystem> = Arc::new(fs_impl);
        let spec = build_spec(root, fs, transport_arc(), Vec::new());

        let out = (spec.tool_handler)("read_file", &json!({"path": "a.txt"})).unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn handler_routes_contract_tool() {
        let root = PathBuf::from("/r");
        let contracts = vec![builtin_brain_contract("http://localhost:8081")];
        let spec = build_spec(root.clone(), fs_arc(&root), transport_arc(), contracts);

        let out = (spec.tool_handler)("save_note", &json!({"title": "x"})).unwrap();
        // response_pointer "data" extracted, pretty-printed.
        assert!(out.contains("\"id\": \"n1\""), "got: {out}");
    }

    #[test]
    fn handler_rejects_unknown_tool() {
        let root = PathBuf::from("/r");
        let spec = build_spec(root.clone(), fs_arc(&root), transport_arc(), Vec::new());
        let err = (spec.tool_handler)("save_note", &json!({"title": "x"}))
            .expect_err("no contracts");
        assert!(err.contains("missing required argument"));
    }

    #[test]
    fn handler_surfaces_domain_errors_as_strings() {
        let root = PathBuf::from("/r");
        let spec = build_spec(root.clone(), fs_arc(&root), transport_arc(), Vec::new());

        let err = (spec.tool_handler)("read_file", &json!({})).expect_err("must error");
        assert!(err.contains("missing required argument"));
    }
}
