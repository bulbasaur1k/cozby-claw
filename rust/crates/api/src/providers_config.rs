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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::providers::openai_compat::{AuthSource, OpenAiCompatClient, OpenAiCompatConfig};

/// Подробный лог провайдер-конфига/аутентификации. По умолчанию молчит; включить
/// через `CLAW_AUTH_DEBUG=1` (раньше эти строки печатались всегда и «шумели» в
/// stderr, попутно раскрывая длины токенов).
macro_rules! auth_debug {
    ($($arg:tt)*) => {
        if std::env::var("CLAW_AUTH_DEBUG").is_ok_and(|value| value != "0" && !value.is_empty()) {
            eprintln!($($arg)*);
        }
    };
}
pub(crate) use auth_debug;

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
    /// Кастомные заголовки (значения поддерживают `${env:}`/`${cmd:}`/`${file:}`)
    Custom,
    /// Токен из внешней команды/скрипта (`auth_command`) в произвольный заголовок.
    /// Напр. `auth_command = "my-auth-cli token"`.
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

/// Раскрывает плейсхолдеры в значении конфига (`api_key` и значения
/// `custom_headers`) — обобщённый, провайдер-агностичный механизм:
///
/// * `${env:VAR}` — переменная окружения;
/// * `${cmd:...}` — trimmed stdout команды (через системный shell), с TTL-кэшем;
/// * `${file:путь}` — содержимое файла (trimmed, `~` раскрывается).
///
/// Нераспознанный плейсхолдер заменяется пустой строкой. Число итераций
/// ограничено — защита от зацикливания, если подстановка вернёт свой же префикс.
fn expand_template(input: &str) -> String {
    let mut result = input.to_string();
    for _ in 0..256 {
        let Some(start) = result.find("${") else { break };
        let Some(end) = result[start..].find('}').map(|offset| start + offset) else {
            break;
        };
        let inner = &result[start + 2..end];
        let value = match inner.split_once(':') {
            Some(("env", name)) => std::env::var(name).unwrap_or_default(),
            Some(("cmd", command)) => run_auth_command(command),
            Some(("file", path)) => std::fs::read_to_string(expand_tilde(path.trim()))
                .map(|text| text.trim().to_string())
                .unwrap_or_default(),
            _ => {
                auth_debug!("[DEBUG] Unknown template placeholder: ${{{inner}}}");
                String::new()
            }
        };
        result.replace_range(start..=end, &value);
    }
    result
}

/// Выполняет команду и возвращает её trimmed stdout как значение (обычно токен).
/// Исполняется системным shell (`sh -c` / `cmd /C`) — как git `credential.helper`
/// или aws `credential_process`, поэтому пригодна для произвольных auth-скриптов.
/// Результат кэшируется на `CLAW_AUTH_CMD_TTL_SECS` секунд (дефолт 300, `0` — без
/// кэша), чтобы не форкать процесс на каждый билд клиента.
/// Кэш токенов от auth-команд: команда → (момент получения, токен).
/// Вынесен на уровень модуля, чтобы 401-обработчик мог инвалидировать запись
/// и следующий запрос получил свежий токен от скрипта.
fn auth_command_cache(
) -> &'static std::sync::Mutex<HashMap<String, (std::time::Instant, String)>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, (std::time::Instant, String)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Сбрасывает закэшированный токен команды — зовётся при 401/403, чтобы
/// перезапросить токен у скрипта, не дожидаясь истечения TTL.
pub(crate) fn invalidate_auth_command_cache(command: &str) {
    if let Ok(mut guard) = auth_command_cache().lock() {
        guard.remove(command.trim());
    }
}

pub(crate) fn run_auth_command(command: &str) -> String {
    use std::time::Instant;

    let command = command.trim();
    if command.is_empty() {
        return String::new();
    }

    let ttl = std::env::var("CLAW_AUTH_CMD_TTL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(300);

    let cache = auth_command_cache();

    if ttl > 0 {
        if let Ok(guard) = cache.lock() {
            if let Some((stored_at, value)) = guard.get(command) {
                if stored_at.elapsed().as_secs() < ttl {
                    auth_debug!("[DEBUG] auth cmd cache hit (len={})", value.len());
                    return value.clone();
                }
            }
        }
    }

    let output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", command])
            .output()
    } else {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
    };

    let value = match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        Ok(out) => {
            auth_debug!(
                "[DEBUG] auth cmd failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            String::new()
        }
        Err(error) => {
            auth_debug!("[DEBUG] auth cmd spawn error: {error}");
            String::new()
        }
    };

    if ttl > 0 && !value.is_empty() {
        if let Ok(mut guard) = cache.lock() {
            guard.insert(command.to_string(), (Instant::now(), value.clone()));
        }
    }
    value
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
    /// Кастомные заголовки аутентификации (используется при `auth_type = "custom"`).
    /// Значения раскрывают `${env:VAR}` / `${cmd:...}` / `${file:путь}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_headers: BTreeMap<String, String>,
    /// Команда/скрипт для получения токена (при `auth_type = "command"`).
    /// Исполняется системным shell; trimmed stdout идёт в заголовок `auth_header`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_command: Option<String>,
    /// Заголовок для токена из `auth_command` (дефолт `Authorization`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// Шаблон значения заголовка; `{token}` → вывод команды (дефолт `Bearer {token}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_format: Option<String>,
    /// Используется GUI для запоминания режима прав; CLI берёт права из флагов/конфига.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

impl ProviderSlot {
    /// Строит AuthSource из конфигурации слота
    fn build_auth_source(&self) -> Result<AuthSource, String> {
        // Раскрываем ${env:}/${cmd:}/${file:} в api_key
        let api_key = expand_template(&self.api_key);
        auth_debug!(
            "[DEBUG] build_auth_source: auth_type={:?}, api_key len={}",
            self.auth_type,
            api_key.len()
        );

        match &self.auth_type {
            AuthType::Bearer => {
                // Определяем тип токена по префиксу
                if api_key.starts_with("ory_at_") {
                    auth_debug!("[DEBUG] Using ApiKey auth (ory token)");
                    Ok(AuthSource::ApiKey(api_key))
                } else {
                    auth_debug!("[DEBUG] Using Bearer auth (JWT)");
                    Ok(AuthSource::Bearer(api_key))
                }
            }
            AuthType::ApiKey => {
                auth_debug!("[DEBUG] Using ApiKey auth");
                Ok(AuthSource::ApiKey(api_key))
            }
            AuthType::Custom => {
                if self.custom_headers.is_empty() {
                    auth_debug!("[DEBUG] Using Bearer auth (custom empty)");
                    Ok(AuthSource::Bearer(api_key))
                } else {
                    let headers = self.expanded_custom_headers();
                    auth_debug!(
                        "[DEBUG] Using CustomHeaders auth: {:?}",
                        headers.keys().collect::<Vec<_>>()
                    );
                    Ok(AuthSource::CustomHeaders(headers))
                }
            }
            AuthType::Command => {
                // Токен из внешней команды → произвольный заголовок, плюс любые
                // дополнительные custom_headers. Провайдер-агностично. Токен НЕ
                // запекается при построении клиента: он разрешается на каждый
                // запрос (сквозь TTL-кэш), поэтому протухший токен обновляется
                // без рестарта процесса, а 401 инвалидирует кэш и ретраится.
                let headers = self.expanded_custom_headers();
                match self.auth_command.as_deref().map(str::trim) {
                    Some(command) if !command.is_empty() => Ok(AuthSource::CommandAuth {
                        command: command.to_string(),
                        header: self
                            .auth_header
                            .clone()
                            .unwrap_or_else(|| "Authorization".to_string()),
                        format: self
                            .auth_format
                            .clone()
                            .unwrap_or_else(|| "Bearer {token}".to_string()),
                        extra_headers: headers,
                    }),
                    _ => {
                        auth_debug!("[DEBUG] auth_type=command but auth_command is unset");
                        Ok(AuthSource::CustomHeaders(headers))
                    }
                }
            }
        }
    }

    /// Раскрывает шаблоны в значениях `custom_headers`.
    fn expanded_custom_headers(&self) -> BTreeMap<String, String> {
        self.custom_headers
            .iter()
            .map(|(key, value)| (key.clone(), expand_template(value)))
            .collect()
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
        auth_debug!("[DEBUG] ProvidersConfig path: {:?}", path);
        path
    }

    /// Загружает конфиг; при отсутствии файла или ошибке парсинга — пустой конфиг
    /// (все слоты `None`), чтобы вызывающий мог корректно откатиться к дефолтам.
    #[must_use]
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::config_path()) else {
            auth_debug!("[DEBUG] Failed to read providers.toml");
            return Self::default();
        };
        auth_debug!("[DEBUG] providers.toml content ({} bytes)", text.len());
        match toml::from_str(&text) {
            Ok(config) => config,
            Err(e) => {
                auth_debug!("[DEBUG] Failed to parse providers.toml: {}", e);
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
    use super::{expand_template, ProviderSlotKind, ProvidersConfig};
    use crate::providers::openai_compat::AuthSource;

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

    #[test]
    fn expand_template_resolves_env_cmd_and_file() {
        // ${env:} — из окружения.
        std::env::set_var("CLAW_TPL_TEST", "sekret");
        assert_eq!(expand_template("v=${env:CLAW_TPL_TEST}"), "v=sekret");
        std::env::remove_var("CLAW_TPL_TEST");

        // ${cmd:} — trimmed stdout (уникальная команда, чтобы не ловить кэш).
        assert_eq!(expand_template("t=${cmd:printf claw-tpl-9f}"), "t=claw-tpl-9f");

        // ${file:} — содержимое файла, trimmed.
        let file = std::env::temp_dir().join(format!("claw-tpl-{}.txt", std::process::id()));
        std::fs::write(&file, "filetoken\n").expect("write temp file");
        assert_eq!(
            expand_template(&format!("f=${{file:{}}}", file.display())),
            "f=filetoken"
        );
        let _ = std::fs::remove_file(&file);

        // Нераспознанный плейсхолдер → пустая строка.
        assert_eq!(expand_template("a${bogus:x}b"), "ab");
    }

    #[test]
    fn command_auth_type_injects_token_into_header() {
        let toml = r#"
            [primary]
            type = "openai"
            model = "m"
            base_url = "u"
            auth_type = "command"
            auth_command = "printf tok-123"
        "#;
        let slot = toml::from_str::<ProvidersConfig>(toml)
            .expect("parses")
            .primary
            .expect("primary");
        // Токен больше не запекается при построении клиента: командная
        // аутентификация ленивая, разрешается на каждый запрос (обновление
        // протухшего токена без рестарта).
        match slot.build_auth_source().expect("auth") {
            AuthSource::CommandAuth {
                command,
                header,
                format,
                extra_headers,
            } => {
                assert_eq!(command, "printf tok-123");
                assert_eq!(header, "Authorization", "default header");
                assert_eq!(format, "Bearer {token}", "default format");
                assert!(extra_headers.is_empty());
            }
            other => panic!("expected CommandAuth, got {other:?}"),
        }
    }

    #[test]
    fn command_auth_type_honours_custom_header_and_format() {
        let toml = r#"
            [primary]
            type = "openai"
            model = "m"
            auth_type = "command"
            auth_command = "printf abc"
            auth_header = "X-Token"
            auth_format = "{token}"
        "#;
        let slot = toml::from_str::<ProvidersConfig>(toml)
            .expect("parses")
            .primary
            .expect("primary");
        match slot.build_auth_source().expect("auth") {
            AuthSource::CommandAuth { command, header, format, .. } => {
                assert_eq!(command, "printf abc");
                assert_eq!(header, "X-Token");
                assert_eq!(format, "{token}");
            }
            other => panic!("expected CommandAuth, got {other:?}"),
        }
    }

    #[test]
    fn invalidate_auth_command_cache_forces_refetch() {
        // Скрипт пишет разный вывод при каждом запуске (счётчик в файле).
        let dir = std::env::temp_dir().join(format!(
            "claw-auth-cache-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let counter = dir.join("counter");
        std::fs::write(&counter, "0").expect("seed counter");
        let command = format!(
            "n=$(cat {path}); n=$((n+1)); printf %s $n > {path}; printf token-$n",
            path = counter.display()
        );

        let first = super::run_auth_command(&command);
        assert_eq!(first, "token-1");
        // Повторный вызов внутри TTL — из кэша, скрипт не перезапускается.
        assert_eq!(super::run_auth_command(&command), "token-1");
        // 401-путь: инвалидация заставляет перезапросить токен у скрипта.
        super::invalidate_auth_command_cache(&command);
        assert_eq!(super::run_auth_command(&command), "token-2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_headers_expand_command_placeholders() {
        // Произвольный заголовок с токеном из команды через ${cmd:}.
        let toml = r#"
            [primary]
            type = "openai"
            model = "m"
            auth_type = "custom"

            [primary.custom_headers]
            X-Auth-Token = "${cmd:printf jwt-xyz}"
        "#;
        let slot = toml::from_str::<ProvidersConfig>(toml)
            .expect("parses")
            .primary
            .expect("primary");
        match slot.build_auth_source().expect("auth") {
            AuthSource::CustomHeaders(headers) => assert_eq!(
                headers.get("X-Auth-Token").map(String::as_str),
                Some("jwt-xyz")
            ),
            other => panic!("expected CustomHeaders, got {other:?}"),
        }
    }
}
