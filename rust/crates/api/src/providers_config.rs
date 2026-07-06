//! Конфиг провайдеров: `~/.claw/providers.toml` (вне git, права 600).
//!
//! Хранит до трёх «слотов»:
//! * `primary` — основная модель, которой думает агент;
//! * `auxiliary` — вспомогательная (более сильная) модель для `consult`-инструмента;
//! * `embedder` — зарезервировано под будущий RAG (сейчас парсится, но не используется).
//!
//! Протокол каждого слота задаётся ключом `type` (`anthropic` | `openai`). Ключи
//! лежат прямо в файле для локального удобства, поэтому файл создаётся с правами
//! `0600` и не должен попадать в репозиторий.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::providers::openai_compat::{AuthSource, OpenAiCompatClient, OpenAiCompatConfig};

/// Протокол провайдера в слоте — какой «диалект» API использовать.
///
/// В TOML задаётся ключом `type` (как `protocol` в qwen-code); для обратной
/// совместимости принимается и старый ключ `kind`. Значение `openai` покрывает
/// любой OpenAI-совместимый endpoint (`OpenRouter`, qwen/`DashScope`, `DeepSeek`,
/// локальный `llama.cpp`/`Ollama`, …) — «кастомный» провайдер задаётся как
/// `type = "openai"` + произвольный `base_url`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSlotKind {
    /// Нативный Anthropic API (`/v1/messages`).
    Anthropic,
    /// Любой OpenAI-совместимый endpoint (`/v1/chat/completions`), напр. `OpenRouter`.
    #[default]
    Openai,
}

impl ProviderSlotKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
        }
    }
}

fn default_max_tokens() -> u32 {
    8192
}

/// Тип аутентификации для OpenAI-совместимых провайдеров
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    /// Bearer токен (Authorization: Bearer <token>)
    #[default]
    Bearer,
    /// API Key (X-API-Key: <key>)
    ApiKey,
    /// Кастомные заголовки
    Custom,
    /// custom-auth (X-Auth-Token: <jwt>) — для vendor custom-auth
    #[serde(rename = "customauth")]
    Command,
}

fn expand_tilde(path: &str) -> String {
    if path.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}

/// Раскрывает переменные окружения вида `${env:VAR}` в строке.
/// Если переменная не задана и это AUTH_TOKEN, пытается получить через `custom-auth token`.
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    while let Some(start) = result.find("${env:") {
        if let Some(end) = result[start..].find('}').map(|i| start + i) {
            let var_name = &result[start + 6..end];
            let var_value = get_env_or_custom-auth(var_name);
            result.replace_range(start..=end, &var_value);
        } else {
            break;
        }
    }
    result
}

/// Получает переменную окружения или выполняет `custom-auth token` для AUTH_TOKEN.
/// Если есть Ory token, обменивает его на JWT через redacted API.
fn get_env_or_custom-auth(var_name: &str) -> String {
    // Сначала пробуем прочитать из env
    if let Ok(value) = std::env::var(var_name) {
        if !value.is_empty() {
            eprintln!("[DEBUG] Using {} from env (len={})", var_name, value.len());
            return value;
        }
    }
    
    // Если это AUTH_TOKEN или ORY_TOKEN — пробуем получить из кэша
    if var_name == "AUTH_TOKEN" || var_name == "ORY_TOKEN" {
        // Пробуем получить JWT из кэша reference (для AUTH_TOKEN)
        if var_name == "AUTH_TOKEN" {
            if let Some(jwt) = get_reference_jwt() {
                eprintln!("[DEBUG] Using JWT from reference cache (len={})", jwt.len());
                std::env::set_var("AUTH_TOKEN", &jwt);
                return jwt;
            }
            eprintln!("[DEBUG] No reference JWT cache, trying to exchange Ory token...");
        }

        // Пробуем получить Ory токен из ~/.auth/access_token.json
        if let Some(ory_token) = get_ory_token() {
            eprintln!("[DEBUG] Using Ory token from cache (len={})", ory_token.len());
            
            // Для AUTH_TOKEN — обмениваем на JWT
            if var_name == "AUTH_TOKEN" {
                if let Some(jwt) = exchange_for_jwt(&ory_token) {
                    eprintln!("[DEBUG] Exchanged Ory token for JWT (len={})", jwt.len());
                    std::env::set_var("AUTH_TOKEN", &jwt);
                    return jwt;
                }
                eprintln!("[DEBUG] Failed to exchange token, using Ory token");
            }
            
            std::env::set_var(var_name, &ory_token);
            return ory_token;
        }

        // Если нет кэша — получаем через auth-cli
        if let Ok(output) = std::process::Command::new("auth-cli")
            .args(["auth", "token"])
            .output()
        {
            if output.status.success() {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !token.is_empty() && !token.contains("unable to load") {
                    eprintln!("[DEBUG] Got token from auth-cli (len={})", token.len());
                    
                    // Для AUTH_TOKEN — обмениваем на JWT
                    if var_name == "AUTH_TOKEN" {
                        if let Some(jwt) = exchange_for_jwt(&token) {
                            eprintln!("[DEBUG] Exchanged Ory token for JWT (len={})", jwt.len());
                            std::env::set_var("AUTH_TOKEN", &jwt);
                            return jwt;
                        }
                        eprintln!("[DEBUG] Failed to exchange token, using Ory token");
                    }
                    
                    std::env::set_var(var_name, &token);
                    return token;
                }
            }
        }
        eprintln!("[DEBUG] Failed to get {}", var_name);
    }

    String::new()
}

/// Пытается получить Ory access token из кэша (~/.auth/access_token.json)
fn get_ory_token() -> Option<String> {
    use std::path::PathBuf;
    
    // Пробуем ~/.auth/access_token.json (основное место хранения auth-cli)
    let home = std::env::var("HOME").ok()?;
    let token_path = PathBuf::from(home).join(".auth/access_token.json");
    
    eprintln!("[DEBUG] Looking for Ory token at {:?}", token_path);
    let content = std::fs::read_to_string(&token_path).ok()?;
    eprintln!("[DEBUG] Read Ory token file (len={})", content.len());
    
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("tokens")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("token"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Пытается получить JWT токен из кэша reference (~/.reference/custom-auth_creds.json)
fn get_reference_jwt() -> Option<String> {
    use std::path::PathBuf;
    
    let home = std::env::var("HOME").ok()?;
    let creds_path = PathBuf::from(home).join(".reference/custom-auth_creds.json");
    
    eprintln!("[DEBUG] Looking for reference creds at {:?}", creds_path);
    let content = std::fs::read_to_string(&creds_path).ok()?;
    eprintln!("[DEBUG] Read reference creds (len={})", content.len());
    
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    
    // Проверяем, не истёк ли токен
    if let Some(expires_at) = json.get("expiresAt").and_then(|v| v.as_u64()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        if now >= expires_at {
            eprintln!("[WARN] reference JWT token expired (now={}, expires={})", now, expires_at);
            return None;
        }
    }
    
    let jwt = json.get("jwt").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref t) = jwt {
        eprintln!("[DEBUG] Found JWT in reference cache (len={})", t.len());
    } else {
        eprintln!("[DEBUG] No JWT field in reference creds");
    }
    jwt
}

/// Обменивает Ory access token на JWT через redacted API
fn exchange_for_jwt(ory_token: &str) -> Option<String> {
    use std::time::Duration;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;

    let response = client
        .post("https://internal-host.example/api/v2/token")
        .header("Authorization", format!("Bearer {}", ory_token))
        .send()
        .ok()?;

    if !response.status().is_success() {
        eprintln!("[WARN] Failed to exchange token: {}", response.status());
        return None;
    }

    let json: serde_json::Value = response.json().ok()?;
    // Возвращаем JWT из поля "jwt" (а не "token")
    let jwt = json.get("jwt").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref t) = jwt {
        eprintln!("[DEBUG] Exchanged token, got JWT (len={})", t.len());
    }
    jwt
}

/// Один настроенный провайдер: модель + endpoint + ключ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSlot {
    /// Протокол провайдера. Канонический TOML-ключ — `type`; алиас `kind`
    /// принимается для файлов, созданных старыми версиями.
    #[serde(default, rename = "type", alias = "kind")]
    pub kind: ProviderSlotKind,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Тип аутентификации (bearer | apikey | custom)
    #[serde(default, rename = "auth_type", alias = "authType")]
    pub auth_type: AuthType,
    /// Кастомные заголовки аутентификации (используется при auth_type = custom)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_headers: BTreeMap<String, String>,
    /// Используется GUI для запоминания режима прав; CLI берёт права из флагов/конфига.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

impl ProviderSlot {
    /// Строит AuthSource из конфигурации слота
    fn build_auth_source(&self) -> Result<AuthSource, String> {
        // Раскрываем переменные окружения в api_key
        let api_key = expand_env_vars(&self.api_key);
        eprintln!("[DEBUG] build_auth_source: api_key len={}, starts with: {}", api_key.len(), &api_key[..api_key.len().min(30)]);

        match &self.auth_type {
            AuthType::Bearer => {
                // Определяем тип токена по префиксу
                if api_key.starts_with("ory_at_") {
                    eprintln!("[DEBUG] Using ApiKey auth (ory token)");
                    Ok(AuthSource::ApiKey(api_key))
                } else {
                    eprintln!("[DEBUG] Using Bearer auth (JWT)");
                    Ok(AuthSource::Bearer(api_key))
                }
            }
            AuthType::ApiKey => {
                eprintln!("[DEBUG] Using ApiKey auth");
                Ok(AuthSource::ApiKey(api_key))
            }
            AuthType::Custom => {
                if self.custom_headers.is_empty() {
                    eprintln!("[DEBUG] Using Bearer auth (custom empty)");
                    Ok(AuthSource::Bearer(api_key))
                } else {
                    // Раскрываем переменные окружения в заголовках
                    let headers: BTreeMap<String, String> = self.custom_headers
                        .iter()
                        .map(|(k, v)| (k.clone(), expand_env_vars(v)))
                        .collect();
                    eprintln!("[DEBUG] Using CustomHeaders auth: {:?}", headers.keys().collect::<Vec<_>>());
                    Ok(AuthSource::CustomHeaders(headers))
                }
            }
            AuthType::Command => {
                // Для custom-auth используем X-Auth-Token заголовок
                // api_key содержит JWT токен (полученный автоматически или из env)
                eprintln!("[DEBUG] Using CommandToken auth (custom-auth)");
                Ok(AuthSource::CommandToken(api_key))
            }
        }
    }

    /// Строит OpenAI-совместимый клиент из этого слота с правильной аутентификацией.
    #[must_use]
    pub fn openai_client(&self) -> OpenAiCompatClient {
        let auth = self.build_auth_source().unwrap_or_else(|e| {
            eprintln!("Warning: auth error, using fallback: {}", e);
            AuthSource::Bearer(self.api_key.clone())
        });
        OpenAiCompatClient::from_auth(auth, OpenAiCompatConfig::openai())
            .with_base_url(self.base_url.clone())
    }
}

/// Содержимое `providers.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<ProviderSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auxiliary: Option<ProviderSlot>,
    /// Зарезервировано под будущий RAG-эмбеддер; парсится, но пока не используется.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder: Option<ProviderSlot>,
}

/// Каталог конфигурации (`$CLAW_CONFIG_HOME` или `~/.claw`).
fn config_home() -> PathBuf {
    if let Some(explicit) = std::env::var_os("CLAW_CONFIG_HOME") {
        return PathBuf::from(explicit);
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".claw"),
        |home| PathBuf::from(home).join(".claw"),
    )
}

impl ProvidersConfig {
    /// Путь к общему файлу провайдеров.
    #[must_use]
    pub fn config_path() -> PathBuf {
        let path = config_home().join("providers.toml");
        eprintln!("[DEBUG] ProvidersConfig path: {:?}", path);
        path
    }

    /// Загружает конфиг; при отсутствии файла или ошибке парсинга — пустой конфиг
    /// (все слоты `None`), чтобы вызывающий мог корректно откатиться к дефолтам.
    #[must_use]
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::config_path()) else {
            eprintln!("[DEBUG] Failed to read providers.toml");
            return Self::default();
        };
        eprintln!("[DEBUG] providers.toml content ({} bytes)", text.len());
        match toml::from_str(&text) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("[DEBUG] Failed to parse providers.toml: {}", e);
                Self::default()
            }
        }
    }

    /// Сохраняет конфиг в `~/.claw/providers.toml` (права 600 на unix).
    ///
    /// # Errors
    /// Ошибки сериализации, создания каталога или записи файла.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let text = format!(
            "# claw shared provider config (local machine only — NOT in git).\n\
             # API keys live here directly; this file is created with 0600 perms.\n\n\
             {body}"
        );
        std::fs::write(&path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderSlotKind, ProvidersConfig};

    #[test]
    fn parses_primary_and_auxiliary_slots() {
        let toml = r#"
            [primary]
            kind = "openai"
            model = "qwen/qwen3-235b-a22b"
            base_url = "https://openrouter.ai/api/v1"
            api_key = "sk-or-test"

            [auxiliary]
            kind = "openai"
            model = "anthropic/claude-3.5-sonnet"
            base_url = "https://openrouter.ai/api/v1"
            api_key = "sk-or-aux"
            max_tokens = 4096
        "#;
        let config: ProvidersConfig = toml::from_str(toml).expect("parses");
        let primary = config.primary.expect("primary present");
        assert_eq!(primary.kind, ProviderSlotKind::Openai);
        assert_eq!(primary.model, "qwen/qwen3-235b-a22b");
        assert_eq!(primary.max_tokens, 8192, "default applies when omitted");
        let auxiliary = config.auxiliary.expect("auxiliary present");
        assert_eq!(auxiliary.max_tokens, 4096);
        assert!(config.embedder.is_none(), "embedder slot stays reserved");
    }

    #[test]
    fn type_key_selects_protocol_and_kind_is_accepted_as_alias() {
        // Канонический ключ — `type` (как `protocol` в qwen-code).
        let with_type = r#"
            [primary]
            type = "anthropic"
            model = "claude-opus-4-8"

            [auxiliary]
            type = "openai"
            model = "deepseek-chat"
            base_url = "https://api.deepseek.com/v1"
            api_key = "sk-deepseek"
        "#;
        let config: ProvidersConfig = toml::from_str(with_type).expect("parses `type`");
        assert_eq!(config.primary.expect("primary").kind, ProviderSlotKind::Anthropic);
        assert_eq!(
            config.auxiliary.expect("auxiliary").kind,
            ProviderSlotKind::Openai
        );

        // Старый ключ `kind` всё ещё принимается как алиас.
        let with_kind: ProvidersConfig = toml::from_str(
            "[primary]\nkind = \"anthropic\"\nmodel = \"claude-opus-4-8\"\n",
        )
        .expect("parses legacy `kind`");
        assert_eq!(
            with_kind.primary.expect("primary").kind,
            ProviderSlotKind::Anthropic
        );
    }

    #[test]
    fn serialized_slot_uses_type_key() {
        let config: ProvidersConfig =
            toml::from_str("[primary]\ntype=\"anthropic\"\nmodel=\"m\"\n").expect("parses");
        let serialized = toml::to_string_pretty(&config).expect("serializes");
        assert!(
            serialized.contains("type = \"anthropic\""),
            "save format must emit `type`, got: {serialized}"
        );
    }

    #[test]
    fn missing_file_yields_empty_config() {
        // Парсинг пустой строки даёт конфиг без слотов (как при отсутствии файла).
        let config: ProvidersConfig = toml::from_str("").expect("empty parses");
        assert!(config.primary.is_none());
        assert!(config.auxiliary.is_none());
    }

    #[test]
    fn round_trips_through_save_format() {
        let toml = r#"
            [primary]
            kind = "openai"
            model = "m"
            base_url = "u"
            api_key = "k"
        "#;
        let config: ProvidersConfig = toml::from_str(toml).expect("parses");
        let serialized = toml::to_string_pretty(&config).expect("serializes");
        let reparsed: ProvidersConfig = toml::from_str(&serialized).expect("reparses");
        assert_eq!(reparsed.primary.expect("primary").model, "m");
    }
}
