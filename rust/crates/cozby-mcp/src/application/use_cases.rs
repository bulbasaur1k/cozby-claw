//! Use-case'ы. Принимают зависимости как `&dyn FileSystem`, чтобы тесты
//! подставляли in-memory реализацию, а prod — `StdFileSystem`.

use std::path::Path;

use serde_json::Value as JsonValue;

use crate::application::ports::FileSystem;
use crate::domain::{
    ensure_under_root, format_read_body, DomainError, ReadLimit, MAX_GREP_MATCHES,
};

/// Извлекает обязательное строковое поле из JSON-аргументов tool-call'а.
fn required_string<'a>(args: &'a JsonValue, key: &'static str) -> Result<&'a str, DomainError> {
    args.get(key)
        .and_then(JsonValue::as_str)
        .ok_or(DomainError::MissingArgument(key))
}

/// Опциональная строка; отсутствие возвращает `default`.
fn optional_string<'a>(args: &'a JsonValue, key: &str, default: &'a str) -> &'a str {
    args.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or(default)
}

pub fn read_file(
    fs: &dyn FileSystem,
    root: &Path,
    args: &JsonValue,
) -> Result<String, DomainError> {
    let relative = required_string(args, "path")?;
    let canonical = fs.canonicalize(root, relative)?;
    ensure_under_root(&canonical, root)?;
    let outcome = fs.read_text(&canonical, ReadLimit::default().max_bytes)?;
    Ok(format_read_body(
        &outcome.text,
        outcome.full_len,
        ReadLimit::default(),
    ))
}

pub fn list_dir(
    fs: &dyn FileSystem,
    root: &Path,
    args: &JsonValue,
) -> Result<String, DomainError> {
    let relative = optional_string(args, "path", ".");
    let canonical = fs.canonicalize(root, relative)?;
    ensure_under_root(&canonical, root)?;
    let mut entries = fs.list_dir(&canonical)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    if entries.is_empty() {
        return Ok("(empty directory)".to_string());
    }

    let mut out = String::new();
    for entry in entries {
        out.push_str(entry.kind.as_str());
        out.push('\t');
        out.push_str(&entry.name);
        out.push('\n');
    }
    Ok(out)
}

pub fn glob(
    fs: &dyn FileSystem,
    root: &Path,
    args: &JsonValue,
) -> Result<String, DomainError> {
    let pattern = required_string(args, "pattern")?;
    let matches = fs.glob(root, pattern)?;

    let mut out = String::new();
    let mut count = 0_usize;
    for canonical in matches {
        if ensure_under_root(&canonical, root).is_err() {
            continue;
        }
        let rel = canonical.strip_prefix(root).unwrap_or(&canonical);
        out.push_str(&rel.display().to_string());
        out.push('\n');
        count += 1;
        if count >= MAX_GREP_MATCHES {
            out.push_str("\n[truncated]\n");
            break;
        }
    }
    if count == 0 {
        Ok("(no matches)".to_string())
    } else {
        Ok(out)
    }
}

pub fn grep(
    fs: &dyn FileSystem,
    root: &Path,
    args: &JsonValue,
) -> Result<String, DomainError> {
    let pattern = required_string(args, "pattern")?;
    let regex = regex::Regex::new(pattern)
        .map_err(|error| DomainError::InvalidPattern(error.to_string()))?;

    let candidates = if let Some(glob_pattern) = args.get("glob").and_then(JsonValue::as_str) {
        fs.glob(root, glob_pattern)?
            .into_iter()
            .filter(|candidate| ensure_under_root(candidate, root).is_ok())
            .collect::<Vec<_>>()
    } else {
        fs.walk_files(root)?
    };

    let mut out = String::new();
    let mut total = 0_usize;
    'files: for path in candidates {
        // walkdir / glob не гарантирует, что вернулись только файлы, повторно
        // читаем; на битых UTF-8 просто пропускаем.
        let Ok(outcome) = fs.read_text(&path, u64::MAX) else {
            continue;
        };
        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (index, line) in outcome.text.lines().enumerate() {
            if regex.is_match(line) {
                out.push_str(&format!("{display}:{}:{line}\n", index + 1));
                total += 1;
                if total >= MAX_GREP_MATCHES {
                    out.push_str("\n[truncated]\n");
                    break 'files;
                }
            }
        }
    }

    if total == 0 {
        Ok("(no matches)".to_string())
    } else {
        Ok(out)
    }
}

pub fn dispatch(
    fs: &dyn FileSystem,
    root: &Path,
    name: &str,
    args: &JsonValue,
) -> Result<String, DomainError> {
    use crate::domain::ToolKind;
    let kind = ToolKind::from_name(name)
        .ok_or_else(|| DomainError::MissingArgument("tool name"))?;
    match kind {
        ToolKind::ReadFile => read_file(fs, root, args),
        ToolKind::ListDir => list_dir(fs, root, args),
        ToolKind::Glob => glob(fs, root, args),
        ToolKind::Grep => grep(fs, root, args),
    }
}

// --------------------------------------------------------------------------
// Контракты (HTTP-сервисы как MCP-инструменты). Чистая подготовка запроса и
// разбор ответа живут в `domain::contract`; здесь — маршрутизация по имени,
// резолв заголовков (env) и вызов транспорта.
// --------------------------------------------------------------------------

use crate::application::ports::HttpTransport;
use crate::domain::{contract, Contract};

/// Подставляет `${env:VAR}` в значениях заголовков. Чтение env — здесь
/// (application), а не в чистом домене. Неизвестная переменная → пустая строка.
fn resolve_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let env_ref = regex::Regex::new(r"\$\{env:([A-Za-z_][A-Za-z0-9_]*)\}")
        .expect("env-ref regex is valid");
    headers
        .iter()
        .map(|(name, value)| {
            let resolved = env_ref
                .replace_all(value, |caps: &regex::Captures| {
                    std::env::var(&caps[1]).unwrap_or_default()
                })
                .into_owned();
            (name.clone(), resolved)
        })
        .collect()
}

/// Исполняет инструмент контракта: ищет его по имени среди контрактов, готовит
/// запрос (домен), шлёт через транспорт, извлекает и форматирует ответ.
pub fn dispatch_contract(
    transport: &dyn HttpTransport,
    contracts: &[Contract],
    name: &str,
    args: &JsonValue,
) -> Result<String, DomainError> {
    for service in contracts {
        let Some(tool) = service.tools.iter().find(|tool| tool.name == name) else {
            continue;
        };
        let prepared = contract::prepare(tool, args)?;
        let url = format!(
            "{}{}",
            service.base_url.trim_end_matches('/'),
            prepared.path
        );
        let headers = resolve_headers(&service.headers);
        let body = transport.send(
            prepared.method,
            &url,
            &prepared.query,
            &headers,
            prepared.body.as_ref(),
        )?;
        let extracted = contract::extract_response(&body, &tool.response_pointer);
        return Ok(contract::format_response(&extracted));
    }
    Err(DomainError::MissingArgument("tool name"))
}

// --------------------------------------------------------------------------
// Тесты используют in-memory FS из `infrastructure::in_memory_fs`, чтобы
// проверить бизнес-правила без выхода на настоящую файловую систему.
// --------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::{dispatch, grep, list_dir, read_file};
    use crate::domain::DomainError;
    use crate::infrastructure::in_memory_fs::InMemoryFs;
    use serde_json::json;
    use std::path::PathBuf;

    fn fs_with_files(files: &[(&str, &str)]) -> (InMemoryFs, PathBuf) {
        let root = PathBuf::from("/root");
        let mut fs = InMemoryFs::new(&root);
        for (path, content) in files {
            fs.insert_file(path, content.as_bytes());
        }
        (fs, root)
    }

    #[test]
    fn read_file_returns_contents_inside_root() {
        let (fs, root) = fs_with_files(&[("a.txt", "hello")]);
        let out = read_file(&fs, &root, &json!({"path": "a.txt"})).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn read_file_rejects_escape() {
        let (fs, root) = fs_with_files(&[]);
        let err = read_file(&fs, &root, &json!({"path": "../etc/passwd"})).unwrap_err();
        assert!(
            matches!(err, DomainError::PathEscape { .. })
                || matches!(err, DomainError::Filesystem(_)),
            "expected escape or filesystem error, got {err:?}"
        );
    }

    #[test]
    fn read_file_flags_missing_path_argument() {
        let (fs, root) = fs_with_files(&[]);
        let err = read_file(&fs, &root, &json!({})).unwrap_err();
        assert_eq!(err, DomainError::MissingArgument("path"));
    }

    #[test]
    fn list_dir_defaults_to_root() {
        let (mut fs, root) = fs_with_files(&[("one.txt", ""), ("two.txt", "")]);
        fs.insert_dir("sub");
        let out = list_dir(&fs, &root, &json!({})).unwrap();
        assert!(out.contains("file\tone.txt"));
        assert!(out.contains("file\ttwo.txt"));
        assert!(out.contains("dir\tsub"));
    }

    #[test]
    fn list_dir_reports_empty() {
        let (fs, root) = fs_with_files(&[]);
        let out = list_dir(&fs, &root, &json!({})).unwrap();
        assert_eq!(out, "(empty directory)");
    }

    #[test]
    fn grep_returns_matching_lines_with_1_based_numbers() {
        let (fs, root) = fs_with_files(&[("src.rs", "fn main() {}\nfn helper() {}\n")]);
        let out = grep(&fs, &root, &json!({"pattern": "^fn "})).unwrap();
        assert!(out.contains("src.rs:1:fn main()"));
        assert!(out.contains("src.rs:2:fn helper()"));
    }

    #[test]
    fn grep_reports_no_matches_message() {
        let (fs, root) = fs_with_files(&[("src.rs", "let x = 1;\n")]);
        let out = grep(&fs, &root, &json!({"pattern": "^UNMATCHED$"})).unwrap();
        assert_eq!(out, "(no matches)");
    }

    #[test]
    fn grep_flags_invalid_regex() {
        let (fs, root) = fs_with_files(&[]);
        let err = grep(&fs, &root, &json!({"pattern": "["})).unwrap_err();
        assert!(matches!(err, DomainError::InvalidPattern(_)));
    }

    #[test]
    fn dispatch_routes_known_tool_names() {
        let (fs, root) = fs_with_files(&[("a.txt", "hi")]);
        let out = dispatch(&fs, &root, "read_file", &json!({"path": "a.txt"})).unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn dispatch_rejects_unknown_tool() {
        let (fs, root) = fs_with_files(&[]);
        let err = dispatch(&fs, &root, "delete_universe", &json!({})).unwrap_err();
        assert!(matches!(err, DomainError::MissingArgument(_)));
    }

    // ----------------------------------------------------------------------
    // Контракты — мок транспорта фиксирует исходящий запрос и отдаёт заранее
    // заданное тело, чтобы проверить маршрутизацию/подготовку/разбор без сети.
    // ----------------------------------------------------------------------

    use super::dispatch_contract;
    use crate::application::ports::HttpTransport;
    use crate::domain::{
        Contract, ContractParam, ContractTool, HttpMethod, ParamLocation,
    };
    use serde_json::Value as JsonValue;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockTransport {
        last_url: Mutex<Option<String>>,
        last_body: Mutex<Option<JsonValue>>,
        last_headers: Mutex<Option<Vec<(String, String)>>>,
        reply: JsonValue,
    }

    impl HttpTransport for MockTransport {
        fn send(
            &self,
            _method: HttpMethod,
            url: &str,
            query: &[(String, String)],
            headers: &[(String, String)],
            body: Option<&JsonValue>,
        ) -> Result<JsonValue, DomainError> {
            let full = if query.is_empty() {
                url.to_string()
            } else {
                let qs = query
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&");
                format!("{url}?{qs}")
            };
            *self.last_url.lock().unwrap() = Some(full);
            *self.last_body.lock().unwrap() = body.cloned();
            *self.last_headers.lock().unwrap() = Some(headers.to_vec());
            Ok(self.reply.clone())
        }
    }

    fn note_contract() -> Contract {
        Contract {
            name: "svc".to_string(),
            base_url: "http://localhost:8081/".to_string(),
            headers: vec![("Authorization".to_string(), "Bearer ${env:CONSULT_TEST_TOKEN}".to_string())],
            tools: vec![
                ContractTool {
                    name: "save_note".to_string(),
                    description: "save".to_string(),
                    method: HttpMethod::Post,
                    path: "/api/notes".to_string(),
                    response_pointer: "data".to_string(),
                    params: vec![ContractParam {
                        name: "title".to_string(),
                        wire_name: "title".to_string(),
                        location: ParamLocation::Body,
                        json_type: "string".to_string(),
                        required: true,
                        description: None,
                    }],
                },
                ContractTool {
                    name: "search".to_string(),
                    description: "search".to_string(),
                    method: HttpMethod::Get,
                    path: "/api/notes/search".to_string(),
                    response_pointer: "data".to_string(),
                    params: vec![ContractParam {
                        name: "query".to_string(),
                        wire_name: "q".to_string(),
                        location: ParamLocation::Query,
                        json_type: "string".to_string(),
                        required: true,
                        description: None,
                    }],
                },
            ],
        }
    }

    #[test]
    fn dispatch_contract_posts_body_and_extracts_data() {
        let transport = MockTransport {
            reply: json!({ "status": "ok", "data": { "id": "n1", "title": "Hi" } }),
            ..Default::default()
        };
        let contracts = vec![note_contract()];
        let out = dispatch_contract(&transport, &contracts, "save_note", &json!({"title": "T"}))
            .unwrap();
        // base_url trailing slash trimmed; body carries the note title.
        assert_eq!(
            transport.last_url.lock().unwrap().as_deref(),
            Some("http://localhost:8081/api/notes")
        );
        assert_eq!(transport.last_body.lock().unwrap().as_ref().unwrap()["title"], "T");
        // response_pointer "data" extracted and pretty-printed.
        assert!(out.contains("\"id\": \"n1\""), "got: {out}");
    }

    #[test]
    fn dispatch_contract_maps_query_param_wire_name() {
        let transport = MockTransport {
            reply: json!({ "data": [] }),
            ..Default::default()
        };
        let contracts = vec![note_contract()];
        let _ = dispatch_contract(&transport, &contracts, "search", &json!({"query": "foo"}))
            .unwrap();
        assert_eq!(
            transport.last_url.lock().unwrap().as_deref(),
            Some("http://localhost:8081/api/notes/search?q=foo")
        );
    }

    #[test]
    fn dispatch_contract_resolves_env_headers() {
        std::env::set_var("CONSULT_TEST_TOKEN", "secret-xyz");
        let transport = MockTransport {
            reply: json!({ "data": {} }),
            ..Default::default()
        };
        let contracts = vec![note_contract()];
        let _ = dispatch_contract(&transport, &contracts, "save_note", &json!({"title": "T"}))
            .unwrap();
        let headers = transport.last_headers.lock().unwrap().clone().unwrap();
        assert!(headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer secret-xyz"));
        std::env::remove_var("CONSULT_TEST_TOKEN");
    }

    #[test]
    fn dispatch_contract_rejects_unknown_and_missing_field() {
        let transport = MockTransport::default();
        let contracts = vec![note_contract()];
        let unknown = dispatch_contract(&transport, &contracts, "nope", &json!({})).unwrap_err();
        assert!(matches!(unknown, DomainError::MissingArgument(_)));
        let missing =
            dispatch_contract(&transport, &contracts, "save_note", &json!({})).unwrap_err();
        assert!(matches!(missing, DomainError::MissingField(f) if f == "title"));
    }
}
