//! Общее ядро фронтендов claw (CLI + GUI).
//!
//! Цель — паритет возможностей между CLI и GUI без дублирования: построение
//! клиента модели и системного промпта из общей конфигурации (providers.toml,
//! проектный/git-контекст). Сюда же будут вынесены остальные части построения
//! рантайма (feature-config, MCP, hooks, plugins) по мере выноса из CLI.

use std::path::Path;

use api::{
    max_tokens_for_model, read_base_url, resolve_startup_auth_source, AnthropicClient, ApiError,
    AuthSource, PromptCache, ProviderClient, ProviderSlotKind, ProvidersConfig,
};
use runtime::{load_system_prompt, ConfigLoader, RuntimeConfig, RuntimeFeatureConfig};

/// Строит клиента основной модели из секции `[primary]` файла providers.toml.
/// Поддерживает Anthropic и любые OpenAI-совместимые провайдеры — ровно как CLI.
///
/// Возвращает `(клиент, фактическая_модель, max_tokens)`. Если запрошенная модель
/// не совпала с `[primary]` (или файла нет) — откат на нативный Anthropic с
/// авторизацией из окружения/сохранённого OAuth.
///
/// # Errors
/// Ошибка разрешения Anthropic-авторизации (нет ключа/OAuth).
pub fn build_provider_client(
    session_id: &str,
    requested_model: &str,
) -> Result<(ProviderClient, String, u32), ApiError> {
    if let Some(slot) = ProvidersConfig::load().primary {
        if slot.model == requested_model {
            match slot.kind {
                ProviderSlotKind::Openai => {
                    return Ok((
                        ProviderClient::OpenAi(slot.openai_client()),
                        slot.model,
                        slot.max_tokens,
                    ));
                }
                ProviderSlotKind::Anthropic => {
                    let auth = if slot.api_key.trim().is_empty() {
                        resolve_auth_source()?
                    } else {
                        AuthSource::ApiKey(slot.api_key.clone())
                    };
                    let base_url = if slot.base_url.trim().is_empty() {
                        read_base_url()
                    } else {
                        slot.base_url.clone()
                    };
                    let client = AnthropicClient::from_auth(auth)
                        .with_base_url(base_url)
                        .with_prompt_cache(PromptCache::new(session_id));
                    return Ok((ProviderClient::Anthropic(client), slot.model, slot.max_tokens));
                }
            }
        }
    }
    let client = AnthropicClient::from_auth(resolve_auth_source()?)
        .with_base_url(read_base_url())
        .with_prompt_cache(PromptCache::new(session_id));
    let max_tokens = max_tokens_for_model(requested_model);
    Ok((
        ProviderClient::Anthropic(client),
        requested_model.to_string(),
        max_tokens,
    ))
}

/// Загружает merged runtime-config из `.claw` (как CLI). `None` при ошибке/отсутствии.
#[must_use]
pub fn load_runtime_config(cwd: &Path) -> Option<RuntimeConfig> {
    ConfigLoader::default_for(cwd).load().ok()
}

/// Фич-конфиг (hooks, permission-rules, compaction, sandbox, external-consult) из
/// загруженного runtime-config; при его отсутствии — значения по умолчанию.
#[must_use]
pub fn feature_config(config: Option<&RuntimeConfig>) -> RuntimeFeatureConfig {
    config.map_or_else(RuntimeFeatureConfig::default, |config| {
        config.feature_config().clone()
    })
}

/// Anthropic-авторизация из окружения или сохранённого OAuth (как в CLI).
fn resolve_auth_source() -> Result<AuthSource, ApiError> {
    resolve_startup_auth_source(|| {
        let cwd = std::env::current_dir().map_err(ApiError::from)?;
        let config = ConfigLoader::default_for(&cwd)
            .load()
            .map_err(|error| ApiError::Auth(format!("failed to load OAuth config: {error}")))?;
        Ok(config.oauth().cloned())
    })
}

/// Богатый системный промпт с проектным/git-контекстом (как в CLI). При ошибке
/// обнаружения контекста — откат на минимальный промпт с указанием cwd.
#[must_use]
pub fn system_prompt(cwd: &Path, date: &str) -> Vec<String> {
    load_system_prompt(
        cwd.to_path_buf(),
        date.to_string(),
        std::env::consts::OS,
        "unknown",
    )
    .unwrap_or_else(|_| vec![fallback_system_prompt(cwd)])
}

fn fallback_system_prompt(cwd: &Path) -> String {
    format!(
        "You are claw, a precise coding assistant running locally inside the project at `{}`. \
         You are ALREADY inside the project — inspect it with the tools (glob_search, \
         grep_search, read_file, bash) instead of asking the user. Keep answers concise.",
        cwd.display()
    )
}

#[cfg(test)]
mod tests {
    use super::feature_config;
    use runtime::RuntimeFeatureConfig;

    #[test]
    fn feature_config_defaults_without_runtime_config() {
        assert_eq!(feature_config(None), RuntimeFeatureConfig::default());
    }
}
