use std::path::{Path, PathBuf};

use crate::domain::DomainError;

/// Тип элемента каталога, возвращаемый `FileSystem::list_dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl DirEntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: DirEntryKind,
}

/// Результат чтения файла: сам текст + полный размер исходного файла
/// (до обрезки). Полный размер нужен, чтобы доменная функция
/// `format_read_body` могла дописать пометку «[truncated …]».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    pub text: String,
    pub full_len: u64,
}

/// Файловая система — единственный порт, через который use-case'ы
/// взаимодействуют с внешним миром. Методы синхронные: операции локальные,
/// async им ничего не даёт, зато упрощает мокирование и pure-function style
/// для use-case'ов.
pub trait FileSystem: Send + Sync {
    /// Канонизирует `relative` относительно `root` (join + canonicalize) и
    /// возвращает абсолютный путь. Проверку «внутри root» выполняет вызывающий
    /// use-case через `domain::ensure_under_root`, чтобы доменная проверка
    /// оставалась чистой функцией.
    fn canonicalize(&self, root: &Path, relative: &str) -> Result<PathBuf, DomainError>;

    /// Читает UTF-8 текст до `max_bytes` байт. Возвращает полный размер файла
    /// даже если содержимое обрезано — чтобы use-case мог это отразить.
    fn read_text(&self, path: &Path, max_bytes: u64) -> Result<ReadOutcome, DomainError>;

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, DomainError>;

    /// Выполняет glob-поиск, начиная от `root`. Возвращает *канонизированные*
    /// пути; use-case сам проверяет, что каждый из них лежит под `root`.
    fn glob(&self, root: &Path, pattern: &str) -> Result<Vec<PathBuf>, DomainError>;

    /// Рекурсивный обход, не следуя символическим ссылкам.
    fn walk_files(&self, root: &Path) -> Result<Vec<PathBuf>, DomainError>;
}
