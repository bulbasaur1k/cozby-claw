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
    fn build_auth_source(&self) -> AuthSource {
        match &self.auth_type {
            AuthType::Bearer => {
                // Определяем тип токена по префиксу
                if self.api_key.starts_with("ory_at_") {
                    AuthSource::ApiKey(self.api_key.clone())
                } else {
                    AuthSource::Bearer(self.api_key.clone())
                }
            }
            AuthType::ApiKey => AuthSource::ApiKey(self.api_key.clone()),
            AuthType::Custom => {
                if self.custom_headers.is_empty() {
                    // Если кастомные заголовки не указаны, используем Bearer по умолчанию
                    AuthSource::Bearer(self.api_key.clone())
                } else {
                    AuthSource::CustomHeaders(self.custom_headers.clone())
                }
            }
        }
    }

    /// Строит OpenAI-совместимый клиент из этого слота с правильной аутентификацией.
    #[must_use]
    pub fn openai_client(&self) -> OpenAiCompatClient {
        let auth = self.build_auth_source();
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
        config_home().join("providers.toml")
    }

    /// Загружает конфиг; при отсутствии файла или ошибке парсинга — пустой конфиг
    /// (все слоты `None`), чтобы вызывающий мог корректно откатиться к дефолтам.
    #[must_use]
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::config_path()) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
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
