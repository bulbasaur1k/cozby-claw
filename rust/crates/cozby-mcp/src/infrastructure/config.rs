use std::fs;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("--root requires a path argument")]
    MissingRootValue,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub root: PathBuf,
}

/// Разбирает argv (без argv[0]). Исключения: `-h/--help` и `-V/--version`
/// — специальные варианты, их бинарь печатает и выходит.
pub fn parse_args<I, S>(argv: I) -> Result<Args, ConfigError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = argv.into_iter();
    let mut root: Option<PathBuf> = None;

    while let Some(arg) = iter.next() {
        match arg.as_ref() {
            "--root" => {
                let value = iter
                    .next()
                    .ok_or(ConfigError::MissingRootValue)?;
                root = Some(PathBuf::from(value.as_ref()));
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

    Ok(Args { root: canonical })
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
