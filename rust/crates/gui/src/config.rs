//! Конфигурация подключения GUI к модели. Хранится в общем для CLI и GUI
//! файле `~/.claw/providers.toml` (секция `[primary]`), вне репозитория.
//! По умолчанию — OpenAI-совместимый endpoint под qwen.

use std::path::PathBuf;

use api::{ProviderSlot, ProviderSlotKind, ProvidersConfig};
use runtime::PermissionMode;

/// Модель/endpoint, к которым подключается агент (вид секции `[primary]`).
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub kind: ProviderSlotKind,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub max_tokens: u32,
    pub permission_mode: PermissionMode,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            kind: ProviderSlotKind::Openai,
            // qwen3-coder надёжно отдаёт content вместе с tool-calls; «думающие»
            // модели вроде qwen3-235b на OpenRouter иногда уводят весь ответ в
            // reasoning и оставляют content пустым (агент остаётся без текста).
            model: "qwen/qwen3-coder".to_string(),
            base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string()),
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            max_tokens: 8192,
            permission_mode: PermissionMode::WorkspaceWrite,
        }
    }
}

fn mode_from_str(value: &str) -> PermissionMode {
    match value {
        "read-only" => PermissionMode::ReadOnly,
        "danger-full-access" => PermissionMode::DangerFullAccess,
        "prompt" => PermissionMode::Prompt,
        "allow" => PermissionMode::Allow,
        _ => PermissionMode::WorkspaceWrite,
    }
}

impl ModelConfig {
    /// Путь к общему файлу провайдеров.
    #[must_use]
    pub fn config_path() -> PathBuf {
        ProvidersConfig::config_path()
    }

    /// Загружает секцию `[primary]`; при отсутствии — значения по умолчанию.
    #[must_use]
    pub fn load() -> Self {
        ProvidersConfig::load()
            .primary
            .map_or_else(Self::default, Self::from_slot)
    }

    fn from_slot(slot: ProviderSlot) -> Self {
        let defaults = Self::default();
        Self {
            kind: slot.kind,
            base_url: if slot.base_url.trim().is_empty() {
                defaults.base_url
            } else {
                slot.base_url
            },
            permission_mode: slot
                .permission_mode
                .as_deref()
                .map_or(defaults.permission_mode, mode_from_str),
            model: slot.model,
            api_key: slot.api_key,
            max_tokens: slot.max_tokens,
        }
    }

    /// Сохраняет настройки в секцию `[primary]` общего `providers.toml`,
    /// не затирая `[auxiliary]`/`[embedder]` (права 600 на unix).
    ///
    /// # Errors
    /// Ошибки сериализации, создания каталога или записи файла.
    pub fn save(&self) -> std::io::Result<()> {
        let mut config = ProvidersConfig::load();
        config.primary = Some(ProviderSlot {
            kind: self.kind,
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            max_tokens: self.max_tokens,
            permission_mode: Some(self.permission_mode.as_str().to_string()),
        });
        config.save()
    }
}
