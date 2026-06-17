//! Реализация порта [`HttpTransport`](crate::application::HttpTransport) на
//! `reqwest::blocking`. Единственный сетевой код крейта.
//!
//! Запрос выполняется на отдельном OS-потоке через [`std::thread::scope`]:
//! `reqwest::blocking` создаёт собственный tokio-runtime и паникует, если его
//! дропнуть внутри async-контекста, а наш handler вызывается из `block_on`.
//! Свежий поток tokio-контекста не имеет — паники нет.

use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::application::ports::HttpTransport;
use crate::domain::{DomainError, HttpMethod};

/// Блокирующий HTTP-транспорт для контрактов.
pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    /// # Errors
    /// Если `reqwest` не смог собрать blocking-клиент (нет TLS-бэкенда и т.п.).
    pub fn new() -> Result<Self, DomainError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| DomainError::Http(format!("cannot build HTTP client: {error}")))?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestTransport {
    fn send(
        &self,
        method: HttpMethod,
        url: &str,
        query: &[(String, String)],
        headers: &[(String, String)],
        body: Option<&JsonValue>,
    ) -> Result<JsonValue, DomainError> {
        let mut request = match method {
            HttpMethod::Get => self.client.get(url),
            HttpMethod::Post => self.client.post(url),
            HttpMethod::Put => self.client.put(url),
            HttpMethod::Delete => self.client.delete(url),
            HttpMethod::Patch => self.client.patch(url),
        };
        if !query.is_empty() {
            request = request.query(query);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let outcome = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let response = request.send()?;
                    let status = response.status();
                    let text = response.text()?;
                    Ok::<(reqwest::StatusCode, String), reqwest::Error>((status, text))
                })
                .join()
        });

        let (status, text) = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                return Err(DomainError::Http(format!("request to {url} failed: {error}")));
            }
            Err(_) => return Err(DomainError::Http("request thread panicked".to_string())),
        };
        let parsed: JsonValue = serde_json::from_str(&text).unwrap_or(JsonValue::Null);

        if status.is_success() {
            return Ok(parsed);
        }
        // По соглашению cozby-brain-подобных API: `{ "error": "…" }`.
        let message = parsed
            .get("error")
            .and_then(JsonValue::as_str)
            .map_or_else(|| format!("HTTP {status}"), ToString::to_string);
        Err(DomainError::Http(message))
    }
}
