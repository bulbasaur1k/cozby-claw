use std::fs;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("--root requires a path argument")]
    MissingRootValue,
    #[error("--brain-url requires a URL argument")]
    MissingBrainUrlValue,
    #[error("--contract requires a file path argument")]
    MissingContractValue,
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    #[error("--root is not a directory: {0}")]
    RootNotADir(PathBuf),
    #[error("cannot canonicalize --root {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("help printed")]
    HelpRequested,
    #[error("version printed")]
    VersionRequested,
}

/// Имя переменной окружения, включающей brain-инструменты, если флаг
/// `--brain-url` не задан.
pub const BRAIN_URL_ENV: &str = "COZBY_BRAIN_URL";

/// Доп. контракты из env (пути через `:`), если не заданы флагом `--contract`.
pub const CONTRACTS_ENV: &str = "COZBY_MCP_CONTRACTS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub root: PathBuf,
    /// Базовый URL cozby-brain. `Some` → подключить встроенный brain-контракт;
    /// `None` → brain выключен (только файловые + явные `--contract`).
    pub brain_url: Option<String>,
    /// Файлы пользовательских TOML-контрактов (HTTP-сервисы как MCP-tools).
    pub contracts: Vec<PathBuf>,
}

/// Разбирает argv (без argv[0]). Исключения: `-h/--help` и `-V/--version`
/// — специальные варианты, их бинарь печатает и выходит.
///
/// `--brain-url <url>` подключает встроенный контракт cozby-brain (fallback на
/// env [`BRAIN_URL_ENV`]). `--contract <file>` (можно повторять) подключает
/// пользовательский TOML-контракт (fallback на [`CONTRACTS_ENV`], пути через `:`).
pub fn parse_args<I, S>(argv: I) -> Result<Args, ConfigError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = argv.into_iter();
    let mut root: Option<PathBuf> = None;
    let mut brain_url: Option<String> = None;
    let mut contracts: Vec<PathBuf> = Vec::new();

    while let Some(arg) = iter.next() {
        match arg.as_ref() {
            "--root" => {
                let value = iter
                    .next()
                    .ok_or(ConfigError::MissingRootValue)?;
                root = Some(PathBuf::from(value.as_ref()));
            }
            "--brain-url" => {
                let value = iter.next().ok_or(ConfigError::MissingBrainUrlValue)?;
                brain_url = Some(value.as_ref().to_string());
            }
            "--contract" => {
                let value = iter.next().ok_or(ConfigError::MissingContractValue)?;
                contracts.push(PathBuf::from(value.as_ref()));
            }
            "-h" | "--help" => return Err(ConfigError::HelpRequested),
            "-V" | "--version" => return Err(ConfigError::VersionRequested),
            other => return Err(ConfigError::UnknownArgument(other.to_string())),
        }
    }

    let root = root.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });
    let canonical = fs::canonicalize(&root).map_err(|source| ConfigError::Canonicalize {
        path: root.clone(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(ConfigError::RootNotADir(canonical));
    }

    // Флаг приоритетнее env; пустая строка трактуется как «выключено».
    let brain_url = brain_url
        .or_else(|| std::env::var(BRAIN_URL_ENV).ok())
        .filter(|url| !url.trim().is_empty());

    // Если флагов нет, берём пути контрактов из env (через `:`).
    if contracts.is_empty() {
        if let Ok(joined) = std::env::var(CONTRACTS_ENV) {
            contracts = joined
                .split(':')
                .filter(|segment| !segment.trim().is_empty())
                .map(PathBuf::from)
                .collect();
        }
    }

    Ok(Args {
        root: canonical,
        brain_url,
        contracts,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_args, ConfigError};

    #[test]
    fn rejects_unknown_flag() {
        let err = parse_args(["--what"]).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownArgument(ref s) if s == "--what"));
    }

    #[test]
    fn missing_root_value_is_reported() {
        let err = parse_args(["--root"]).unwrap_err();
        assert!(matches!(err, ConfigError::MissingRootValue));
    }

    #[test]
    fn missing_brain_url_value_is_reported() {
        let err = parse_args(["--brain-url"]).unwrap_err();
        assert!(matches!(err, ConfigError::MissingBrainUrlValue));
    }

    #[test]
    fn brain_url_flag_enables_integration() {
        let parsed = parse_args(["--brain-url", "http://localhost:8081"]).unwrap();
        assert_eq!(parsed.brain_url.as_deref(), Some("http://localhost:8081"));
    }

    #[test]
    fn missing_contract_value_is_reported() {
        let err = parse_args(["--contract"]).unwrap_err();
        assert!(matches!(err, ConfigError::MissingContractValue));
    }

    #[test]
    fn contract_flag_repeats() {
        let parsed = parse_args(["--contract", "a.toml", "--contract", "b.toml"]).unwrap();
        assert_eq!(parsed.contracts.len(), 2);
        assert!(parsed.contracts.iter().any(|p| p.ends_with("a.toml")));
    }

    #[test]
    fn help_and_version_are_distinguishable() {
        assert!(matches!(parse_args(["--help"]), Err(ConfigError::HelpRequested)));
        assert!(matches!(parse_args(["-h"]), Err(ConfigError::HelpRequested)));
        assert!(matches!(parse_args(["-V"]), Err(ConfigError::VersionRequested)));
    }

    #[test]
    fn missing_root_defaults_to_cwd_and_canonicalizes() {
        // cwd обязан существовать для любого процесса, так что этот вызов
        // должен пройти без ошибок и дать канонизированный путь.
        let parsed = parse_args::<_, &str>(std::iter::empty::<&str>()).unwrap();
        assert!(parsed.root.is_absolute());
        assert!(parsed.root.is_dir());
    }
}
