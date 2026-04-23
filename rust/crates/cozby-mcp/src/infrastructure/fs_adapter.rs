use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::application::ports::{DirEntry, DirEntryKind, FileSystem, ReadOutcome};
use crate::domain::DomainError;

/// Реальный адаптер над `std::fs`.
///
/// Единственное отступление от «pure stdlib» — зависимости `walkdir` и `glob`
/// для обхода и паттерн-матчинга. Они не втаскивают сеть или асинхронность,
/// и полностью локализованы в этом файле.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFileSystem;

impl StdFileSystem {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn io_to_domain(error: std::io::Error) -> DomainError {
    DomainError::Filesystem(error.to_string())
}

impl FileSystem for StdFileSystem {
    fn canonicalize(&self, root: &Path, relative: &str) -> Result<PathBuf, DomainError> {
        let joined = root.join(relative);
        fs::canonicalize(&joined).map_err(io_to_domain)
    }

    fn read_text(&self, path: &Path, max_bytes: u64) -> Result<ReadOutcome, DomainError> {
        let metadata = fs::metadata(path).map_err(io_to_domain)?;
        if !metadata.is_file() {
            return Err(DomainError::NotAFile(path.to_path_buf()));
        }
        let mut file = fs::File::open(path).map_err(io_to_domain)?;
        let mut buffer = Vec::with_capacity(std::cmp::min(metadata.len(), max_bytes) as usize);
        file.by_ref()
            .take(max_bytes)
            .read_to_end(&mut buffer)
            .map_err(io_to_domain)?;
        let text = String::from_utf8(buffer).map_err(|_| DomainError::NotUtf8)?;
        Ok(ReadOutcome {
            text,
            full_len: metadata.len(),
        })
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, DomainError> {
        let metadata = fs::metadata(path).map_err(io_to_domain)?;
        if !metadata.is_dir() {
            return Err(DomainError::NotADir(path.to_path_buf()));
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(path).map_err(io_to_domain)? {
            let entry = entry.map_err(io_to_domain)?;
            let file_type = entry.file_type().map_err(io_to_domain)?;
            let kind = if file_type.is_dir() {
                DirEntryKind::Dir
            } else if file_type.is_file() {
                DirEntryKind::File
            } else if file_type.is_symlink() {
                DirEntryKind::Symlink
            } else {
                DirEntryKind::Other
            };
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        Ok(out)
    }

    fn glob(&self, root: &Path, pattern: &str) -> Result<Vec<PathBuf>, DomainError> {
        let joined = root.join(pattern);
        let combined = joined
            .to_str()
            .ok_or_else(|| DomainError::InvalidGlob("pattern is not UTF-8".to_string()))?;
        let matches =
            glob::glob(combined).map_err(|error| DomainError::InvalidGlob(error.to_string()))?;
        let mut out = Vec::new();
        for entry in matches {
            let path = entry.map_err(|error| DomainError::Filesystem(error.to_string()))?;
            if let Ok(canonical) = fs::canonicalize(&path) {
                out.push(canonical);
            }
        }
        Ok(out)
    }

    fn walk_files(&self, root: &Path) -> Result<Vec<PathBuf>, DomainError> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(root).follow_links(false) {
            let entry = entry.map_err(|error| DomainError::Filesystem(error.to_string()))?;
            if entry.file_type().is_file() {
                out.push(entry.into_path());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::StdFileSystem;
    use crate::application::ports::FileSystem;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp_root(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "cozby-mcp-{label}-{}-{:08x}",
            std::process::id(),
            rand_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        fs::canonicalize(&base).unwrap()
    }

    fn rand_nanos() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn canonicalize_rejects_missing_path() {
        let root = temp_root("canon-missing");
        let result = StdFileSystem.canonicalize(&root, "does-not-exist");
        assert!(result.is_err());
        cleanup(&root);
    }

    #[test]
    fn read_text_surfaces_full_len_even_when_truncated() {
        let root = temp_root("read-trunc");
        let target = root.join("big.txt");
        fs::write(&target, vec![b'a'; 1024]).unwrap();

        let outcome = StdFileSystem.read_text(&target, 100).unwrap();
        assert_eq!(outcome.text.len(), 100);
        assert_eq!(outcome.full_len, 1024);
        cleanup(&root);
    }

    #[test]
    fn list_dir_reports_file_and_subdir() {
        let root = temp_root("list-mixed");
        fs::write(root.join("a.txt"), "x").unwrap();
        fs::create_dir(root.join("sub")).unwrap();

        let entries = StdFileSystem.list_dir(&root).unwrap();
        let names: Vec<_> = entries
            .iter()
            .map(|entry| (entry.kind.as_str(), entry.name.clone()))
            .collect();
        assert!(names.contains(&("file", "a.txt".to_string())));
        assert!(names.contains(&("dir", "sub".to_string())));
        cleanup(&root);
    }

    #[test]
    fn walk_files_skips_directories() {
        let root = temp_root("walk");
        fs::write(root.join("a.txt"), "x").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.txt"), "y").unwrap();

        let files = StdFileSystem.walk_files(&root).unwrap();
        assert_eq!(files.len(), 2, "walk should return only files, got {files:?}");
        cleanup(&root);
    }
}
