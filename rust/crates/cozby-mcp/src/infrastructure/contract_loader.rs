//! Загрузка контрактов: встроенный cozby-brain и пользовательские TOML-файлы.
//!
//! Формат TOML-контракта:
//! ```toml
//! name = "myservice"
//! base_url = "https://api.example.com"
//!
//! [headers]
//! Authorization = "Bearer ${env:MY_TOKEN}"   # ${env:VAR} резолвится при вызове
//!
//! [[tools]]
//! name = "get_item"
//! description = "Get an item by id"
//! method = "GET"                 # GET|POST|PUT|DELETE|PATCH
//! path = "/items/{id}"           # {param} подставляется из path-параметров
//! response = "data"              # точечный путь до полезной части (по умолч. всё тело)
//!
//!   [[tools.params]]
//!   name = "id"
//!   location = "path"            # path|query|body
//!   type = "string"              # json-тип для схемы (по умолч. string)
//!   required = true              # по умолч. false
//!   wire_name = "id"             # имя на проводе (по умолч. = name)
//!   description = "Item id"
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::domain::{Contract, ContractParam, ContractTool, DomainError, HttpMethod, ParamLocation};

#[derive(Debug, Deserialize)]
struct RawContract {
    name: String,
    base_url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    tools: Vec<RawTool>,
}

#[derive(Debug, Deserialize)]
struct RawTool {
    name: String,
    #[serde(default)]
    description: String,
    method: String,
    path: String,
    #[serde(default)]
    response: String,
    #[serde(default)]
    params: Vec<RawParam>,
}

#[derive(Debug, Deserialize)]
struct RawParam {
    name: String,
    #[serde(default)]
    wire_name: Option<String>,
    location: String,
    #[serde(rename = "type", default = "default_param_type")]
    json_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    description: Option<String>,
}

fn default_param_type() -> String {
    "string".to_string()
}

fn parse_location(value: &str, tool: &str, param: &str) -> Result<ParamLocation, DomainError> {
    match value.to_ascii_lowercase().as_str() {
        "path" => Ok(ParamLocation::Path),
        "query" => Ok(ParamLocation::Query),
        "body" => Ok(ParamLocation::Body),
        other => Err(DomainError::Contract(format!(
            "tool `{tool}` param `{param}`: unknown location `{other}` (use path|query|body)"
        ))),
    }
}

fn raw_into_contract(raw: RawContract) -> Result<Contract, DomainError> {
    let mut tools = Vec::with_capacity(raw.tools.len());
    for tool in raw.tools {
        let method = HttpMethod::from_str_ci(&tool.method).ok_or_else(|| {
            DomainError::Contract(format!(
                "tool `{}`: unknown HTTP method `{}`",
                tool.name, tool.method
            ))
        })?;
        let mut params = Vec::with_capacity(tool.params.len());
        for param in tool.params {
            let location = parse_location(&param.location, &tool.name, &param.name)?;
            let wire_name = param.wire_name.unwrap_or_else(|| param.name.clone());
            params.push(ContractParam {
                name: param.name,
                wire_name,
                location,
                json_type: param.json_type,
                required: param.required,
                description: param.description,
            });
        }
        tools.push(ContractTool {
            name: tool.name,
            description: tool.description,
            method,
            path: tool.path,
            response_pointer: tool.response,
            params,
        });
    }
    Ok(Contract {
        name: raw.name,
        base_url: raw.base_url,
        headers: raw.headers.into_iter().collect(),
        tools,
    })
}

/// Разбирает контракт из TOML-текста.
///
/// # Errors
/// [`DomainError::Contract`] при ошибке синтаксиса TOML или неверных полях.
pub fn parse_contract_toml(source: &str) -> Result<Contract, DomainError> {
    let raw: RawContract = toml::from_str(source)
        .map_err(|error| DomainError::Contract(format!("invalid contract TOML: {error}")))?;
    raw_into_contract(raw)
}

/// Загружает контракт из файла.
///
/// # Errors
/// [`DomainError::Contract`] при ошибке чтения файла или разбора.
pub fn load_contract_file(path: &Path) -> Result<Contract, DomainError> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        DomainError::Contract(format!("cannot read contract {}: {error}", path.display()))
    })?;
    parse_contract_toml(&source)
}

/// Встроенный контракт cozby-brain — то, что раньше было захардкожено как
/// `HttpBrainClient`. Включается, когда задан `--brain-url`.
#[must_use]
pub fn builtin_brain_contract(base_url: impl Into<String>) -> Contract {
    let opt = |name: &str, json_type: &str, loc: ParamLocation, wire: &str, desc: &str| {
        ContractParam {
            name: name.to_string(),
            wire_name: wire.to_string(),
            location: loc,
            json_type: json_type.to_string(),
            required: false,
            description: Some(desc.to_string()),
        }
    };
    let req = |name: &str, json_type: &str, loc: ParamLocation, wire: &str, desc: &str| {
        ContractParam {
            required: true,
            ..opt(name, json_type, loc, wire, desc)
        }
    };

    Contract {
        name: "cozby-brain".to_string(),
        base_url: base_url.into(),
        headers: Vec::new(),
        tools: vec![
            ContractTool {
                name: "save_note".to_string(),
                description: "Save a note to cozby-brain (personal knowledge base). Use when \
                              asked to remember or jot something down."
                    .to_string(),
                method: HttpMethod::Post,
                path: "/api/notes".to_string(),
                response_pointer: "data".to_string(),
                params: vec![
                    req("title", "string", ParamLocation::Body, "title", "Short note title"),
                    opt("content", "string", ParamLocation::Body, "content", "Note body (markdown)"),
                    opt("tags", "array", ParamLocation::Body, "tags", "Tags"),
                ],
            },
            ContractTool {
                name: "search_notes".to_string(),
                description: "Search previously saved notes in cozby-brain by keyword."
                    .to_string(),
                method: HttpMethod::Get,
                path: "/api/notes/search".to_string(),
                response_pointer: "data".to_string(),
                params: vec![req(
                    "query", "string", ParamLocation::Query, "q", "Search query",
                )],
            },
            ContractTool {
                name: "save_doc".to_string(),
                description: "Save or update a documentation page in cozby-brain. `operation` \
                              is create|append|replace|section."
                    .to_string(),
                method: HttpMethod::Post,
                path: "/api/doc/pages".to_string(),
                response_pointer: "data".to_string(),
                params: vec![
                    req("project", "string", ParamLocation::Body, "project", "Project slug/id/title"),
                    req("page", "string", ParamLocation::Body, "page", "Page title"),
                    req("content", "string", ParamLocation::Body, "content", "Markdown content"),
                    opt("tags", "array", ParamLocation::Body, "tags", "Tags"),
                    opt("operation", "string", ParamLocation::Body, "operation", "create|append|replace|section"),
                    opt("section_title", "string", ParamLocation::Body, "section_title", "Target section"),
                ],
            },
            ContractTool {
                name: "recall".to_string(),
                description: "Ask cozby-brain a question over everything saved (RAG); returns \
                              an answer with [N] citations."
                    .to_string(),
                method: HttpMethod::Get,
                path: "/api/ask".to_string(),
                // /api/ask кладёт answer/sources на верхний уровень — берём всё тело.
                response_pointer: String::new(),
                params: vec![req(
                    "question", "string", ParamLocation::Query, "q", "Natural-language question",
                )],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{builtin_brain_contract, parse_contract_toml};
    use crate::domain::{HttpMethod, ParamLocation};

    #[test]
    fn brain_contract_exposes_four_tools() {
        let contract = builtin_brain_contract("http://localhost:8081");
        let names: Vec<_> = contract.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["save_note", "search_notes", "save_doc", "recall"]);
        let recall = contract.tools.iter().find(|t| t.name == "recall").unwrap();
        assert_eq!(recall.method, HttpMethod::Get);
        assert!(recall.response_pointer.is_empty());
    }

    #[test]
    fn parses_a_user_contract() {
        let source = r#"
            name = "myservice"
            base_url = "https://api.example.com"

            [headers]
            Authorization = "Bearer ${env:MY_TOKEN}"

            [[tools]]
            name = "get_item"
            description = "Get an item"
            method = "get"
            path = "/items/{id}"
            response = "data"

              [[tools.params]]
              name = "id"
              location = "path"
              required = true

              [[tools.params]]
              name = "q"
              location = "query"
        "#;
        let contract = parse_contract_toml(source).unwrap();
        assert_eq!(contract.name, "myservice");
        assert_eq!(contract.headers.len(), 1);
        let tool = &contract.tools[0];
        assert_eq!(tool.method, HttpMethod::Get);
        assert_eq!(tool.params[0].location, ParamLocation::Path);
        assert!(tool.params[0].required);
        // default type is string, default required false
        assert_eq!(tool.params[1].json_type, "string");
        assert!(!tool.params[1].required);
    }

    #[test]
    fn rejects_unknown_method_and_location() {
        let bad_method = r#"
            name = "s"
            base_url = "http://x"
            [[tools]]
            name = "t"
            method = "FETCH"
            path = "/"
        "#;
        assert!(parse_contract_toml(bad_method).is_err());

        let bad_loc = r#"
            name = "s"
            base_url = "http://x"
            [[tools]]
            name = "t"
            method = "GET"
            path = "/"
            [[tools.params]]
            name = "p"
            location = "header"
        "#;
        assert!(parse_contract_toml(bad_loc).is_err());
    }
}
