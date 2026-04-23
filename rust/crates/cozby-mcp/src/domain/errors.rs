use std::path::PathBuf;

use thiserror::Error;

/// Ошибки, которые может породить доменный слой.
///
/// Сюда не попадают «низкоуровневые» причины (I/O errno, permission denied) —
/// это преобразуется в [`DomainError`] адаптерами infrastructure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("path {candidate} escapes root {root}")]
    PathEscape { candidate: PathBuf, root: PathBuf },

    #[error("not a regular file: {0}")]
    NotAFile(PathBuf),

    #[error("not a directory: {0}")]
    NotADir(PathBuf),

    #[error("file is not valid UTF-8")]
    NotUtf8,

    #[error("invalid regex: {0}")]
    InvalidPattern(String),

    #[error("invalid glob: {0}")]
    InvalidGlob(String),

    #[error("missing required argument: {0}")]
    MissingArgument(&'static str),

    #[error("filesystem error: {0}")]
    Filesystem(String),
}
