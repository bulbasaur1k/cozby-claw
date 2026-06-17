//! Доменная модель HTTP-контракта: декларативное описание внешнего сервиса и
//! его инструментов, превращаемое в MCP-tools.
//!
//! Это обобщение того, что раньше было захардкожено для cozby-brain: вместо
//! отдельного порта/адаптера под каждый сервис описываешь его контрактом
//! (`base_url` + список `tools`), а cozby-mcp выставляет каждый tool по MCP.
//! cozby-brain теперь — просто встроенный контракт.
//!
//! Слой **чистый**: ноль сети/IO. Здесь живут построение JSON-схемы tool'а,
//! подготовка запроса из аргументов (path/query/body) и извлечение/форматирование
//! ответа. Реальную отправку делает `application::HttpTransport`.

use serde_json::{json, Map, Value as JsonValue};

use crate::domain::DomainError;

/// Куда подставляется параметр в HTTP-запросе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLocation {
    /// В тело JSON (`{ wire_name: value }`).
    Body,
    /// В query-строку (`?wire_name=value`).
    Query,
    /// В шаблон пути (`/x/{name}/y`).
    Path,
}

/// HTTP-метод инструмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

impl HttpMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
        }
    }

    #[must_use]
    pub fn from_str_ci(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "PATCH" => Some(Self::Patch),
            _ => None,
        }
    }

    /// Методы, которые по семантике обычно меняют состояние сервера.
    #[must_use]
    pub const fn is_writing(self) -> bool {
        !matches!(self, Self::Get)
    }
}

/// Один параметр инструмента: как имя из аргументов tool-call'а ложится в запрос.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractParam {
    /// Имя аргумента в JSON-схеме инструмента (то, что присылает модель).
    pub name: String,
    /// Имя на проводе (query-ключ / поле тела). По умолчанию совпадает с `name`.
    pub wire_name: String,
    pub location: ParamLocation,
    /// JSON-тип для схемы: `string|integer|boolean|number|array|object`.
    pub json_type: String,
    pub required: bool,
    pub description: Option<String>,
}

/// Инструмент контракта = один HTTP-эндпоинт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractTool {
    pub name: String,
    pub description: String,
    pub method: HttpMethod,
    /// Путь относительно `base_url`, может содержать `{param}`-плейсхолдеры.
    pub path: String,
    /// Точечный путь до полезной части ответа (`data`, `data.items`); пусто —
    /// вернуть тело целиком.
    pub response_pointer: String,
    pub params: Vec<ContractParam>,
}

/// Описание внешнего сервиса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub name: String,
    pub base_url: String,
    /// Заголовки запроса; значения могут содержать `${env:VAR}` (резолвится в
    /// application-слое, домен про env ничего не знает).
    pub headers: Vec<(String, String)>,
    pub tools: Vec<ContractTool>,
}

/// Подготовленный запрос (без `base_url` и заголовков — их добавляет вызывающий).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequest {
    pub method: HttpMethod,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<JsonValue>,
}

/// Строит JSON-схему входа инструмента из его параметров.
#[must_use]
pub fn input_schema(tool: &ContractTool) -> JsonValue {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for param in &tool.params {
        let mut prop = json!({ "type": param.json_type });
        if let Some(description) = &param.description {
            prop["description"] = JsonValue::String(description.clone());
        }
        properties.insert(param.name.clone(), prop);
        if param.required {
            required.push(JsonValue::String(param.name.clone()));
        }
    }
    json!({
        "type": "object",
        "properties": JsonValue::Object(properties),
        "required": JsonValue::Array(required),
    })
}

/// Любой ли инструмент контракта пишет (для аннотации `readOnlyHint`).
#[must_use]
pub fn tool_writes(tool: &ContractTool) -> bool {
    tool.method.is_writing()
}

/// Превращает аргумент в строку для query/path (строка — как есть, прочее —
/// через JSON без кавычек у скаляров).
fn scalar_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Готовит HTTP-запрос из контрактного инструмента и аргументов tool-call'а.
///
/// # Errors
/// [`DomainError::MissingField`], если обязательный параметр отсутствует.
pub fn prepare(tool: &ContractTool, args: &JsonValue) -> Result<PreparedRequest, DomainError> {
    let mut path = tool.path.clone();
    let mut query = Vec::new();
    let mut body = Map::new();

    for param in &tool.params {
        let value = args.get(&param.name);
        let Some(value) = value else {
            if param.required {
                return Err(DomainError::MissingField(param.name.clone()));
            }
            continue;
        };
        match param.location {
            ParamLocation::Path => {
                let placeholder = format!("{{{}}}", param.name);
                path = path.replace(&placeholder, &scalar_to_string(value));
            }
            ParamLocation::Query => {
                query.push((param.wire_name.clone(), scalar_to_string(value)));
            }
            ParamLocation::Body => {
                body.insert(param.wire_name.clone(), value.clone());
            }
        }
    }

    let body = if body.is_empty() {
        None
    } else {
        Some(JsonValue::Object(body))
    };
    Ok(PreparedRequest {
        method: tool.method,
        path,
        query,
        body,
    })
}

/// Достаёт из ответа часть по точечному пути (`data.items`); пустой путь —
/// тело целиком. Отсутствующий сегмент → `Null`.
#[must_use]
pub fn extract_response(body: &JsonValue, pointer: &str) -> JsonValue {
    if pointer.is_empty() {
        return body.clone();
    }
    let mut current = body;
    for segment in pointer.split('.') {
        match current.get(segment) {
            Some(next) => current = next,
            None => return JsonValue::Null,
        }
    }
    current.clone()
}

/// Форматирует извлечённое значение для ответа инструмента: строки — как есть,
/// остальное — pretty-JSON.
#[must_use]
pub fn format_response(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => "(empty response)".to_string(),
        other => serde_json::to_string_pretty(other)
            .unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_response, format_response, input_schema, prepare, Contract, ContractParam,
        ContractTool, HttpMethod, ParamLocation, PreparedRequest,
    };
    use crate::domain::DomainError;
    use serde_json::json;

    fn param(name: &str, wire: &str, loc: ParamLocation, required: bool) -> ContractParam {
        ContractParam {
            name: name.to_string(),
            wire_name: wire.to_string(),
            location: loc,
            json_type: "string".to_string(),
            required,
            description: None,
        }
    }

    fn save_note_tool() -> ContractTool {
        ContractTool {
            name: "save_note".to_string(),
            description: "save".to_string(),
            method: HttpMethod::Post,
            path: "/api/notes".to_string(),
            response_pointer: "data".to_string(),
            params: vec![
                param("title", "title", ParamLocation::Body, true),
                param("content", "content", ParamLocation::Body, false),
            ],
        }
    }

    #[test]
    fn schema_lists_properties_and_required() {
        let schema = input_schema(&save_note_tool());
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["title"].is_object());
        assert_eq!(schema["required"][0], "title");
        // optional param not in required
        assert_eq!(schema["required"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn prepare_builds_body_from_present_params_only() {
        let prepared = prepare(&save_note_tool(), &json!({"title": "T"})).unwrap();
        assert_eq!(prepared.method, HttpMethod::Post);
        assert_eq!(prepared.path, "/api/notes");
        assert_eq!(prepared.body, Some(json!({"title": "T"})));
        assert!(prepared.query.is_empty());
    }

    #[test]
    fn prepare_errors_on_missing_required() {
        let err = prepare(&save_note_tool(), &json!({})).unwrap_err();
        assert!(matches!(err, DomainError::MissingField(field) if field == "title"));
    }

    #[test]
    fn prepare_maps_query_and_path_params() {
        let tool = ContractTool {
            name: "get_item".to_string(),
            description: "g".to_string(),
            method: HttpMethod::Get,
            path: "/api/items/{id}".to_string(),
            response_pointer: String::new(),
            params: vec![
                param("id", "id", ParamLocation::Path, true),
                param("query", "q", ParamLocation::Query, false),
            ],
        };
        let prepared = prepare(&tool, &json!({"id": "42", "query": "foo"})).unwrap();
        assert_eq!(prepared.path, "/api/items/42");
        assert_eq!(prepared.query, vec![("q".to_string(), "foo".to_string())]);
        assert_eq!(prepared.body, None);
        let _ = PreparedRequest {
            method: HttpMethod::Get,
            path: String::new(),
            query: vec![],
            body: None,
        };
    }

    #[test]
    fn extract_and_format_response() {
        let body = json!({ "status": "ok", "data": { "id": "n1", "title": "Hi" } });
        let extracted = extract_response(&body, "data");
        assert_eq!(extracted["id"], "n1");
        // whole body when pointer empty
        assert_eq!(extract_response(&body, ""), body);
        // missing → Null → "(empty response)"
        assert_eq!(format_response(&extract_response(&body, "nope")), "(empty response)");
        // string answer passes through
        assert_eq!(format_response(&json!("plain")), "plain");
    }

    #[test]
    fn contract_value_constructs() {
        let contract = Contract {
            name: "svc".to_string(),
            base_url: "http://x".to_string(),
            headers: vec![("A".to_string(), "1".to_string())],
            tools: vec![save_note_tool()],
        };
        assert_eq!(contract.tools.len(), 1);
    }
}
