//! Тестовая реализация `FileSystem`, не касающаяся настоящей ФС.
//!
//! Моделирует absolute-path корень. `relative` пути в `canonicalize` просто
//! конкатенируются; `..` поддерживается по-простому (не через настоящую
//! канонизацию). Этого достаточно для use-case тестов.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::application::ports::{DirEntry, DirEntryKind, FileSystem, ReadOutcome};
use crate::domain::DomainError;

pub struct InMemoryFs {
    root: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
    dirs: BTreeSet<PathBuf>,
}

impl InMemoryFs {
    pub fn new(root: &Path) -> Self {
        let mut dirs = BTreeSet::new();
        dirs.insert(root.to_path_buf());
        Self {
            root: root.to_path_buf(),
            files: BTreeMap::new(),
            dirs,
        }
    }

    pub fn insert_file(&mut self, relative: &str, bytes: &[u8]) {
        let path = self.root.join(relative);
        self.ensure_parents(&path);
        self.files.insert(path, bytes.to_vec());
    }

    pub fn insert_dir(&mut self, relative: &str) {
        let path = self.root.join(relative);
        self.ensure_parents(&path);
        self.dirs.insert(path);
    }

    fn ensure_parents(&mut self, path: &Path) {
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            // Не регистрируем итоговый "файл" как каталог.
            if current == path && !path.to_string_lossy().ends_with('/') {
                continue;
            }
            self.dirs.insert(current.clone());
        }
    }

    fn normalize(&self, root: &Path, relative: &str) -> PathBuf {
        let joined = root.join(relative);
        let mut out = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
}

impl FileSystem for InMemoryFs {
    fn canonicalize(&self, root: &Path, relative: &str) -> Result<PathBuf, DomainError> {
        let normalized = self.normalize(root, relative);
        if self.files.contains_key(&normalized) || self.dirs.contains(&normalized) {
            Ok(normalized)
        } else {
            Err(DomainError::Filesystem(format!(
                "no such entry: {}",
                normalized.display()
            )))
        }
    }

    fn read_text(&self, path: &Path, max_bytes: u64) -> Result<ReadOutcome, DomainError> {
        let bytes = self
            .files
            .get(path)
            .ok_or_else(|| DomainError::NotAFile(path.to_path_buf()))?;
        let full_len = bytes.len() as u64;
        let take = std::cmp::min(full_len, max_bytes) as usize;
        let slice = &bytes[..take];
        let text = std::str::from_utf8(slice)
            .map_err(|_| DomainError::NotUtf8)?
            .to_string();
        Ok(ReadOutcome { text, full_len })
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, DomainError> {
        if !self.dirs.contains(path) {
            return Err(DomainError::NotADir(path.to_path_buf()));
        }
        let mut names: BTreeMap<String, DirEntryKind> = BTreeMap::new();
        for file in self.files.keys() {
            if file.parent() == Some(path) {
                if let Some(name) = file.file_name() {
                    names.insert(name.to_string_lossy().into_owned(), DirEntryKind::File);
                }
            }
        }
        for dir in &self.dirs {
            if dir.parent() == Some(path) {
                if let Some(name) = dir.file_name() {
                    names.insert(name.to_string_lossy().into_owned(), DirEntryKind::Dir);
                }
            }
        }
        Ok(names
            .into_iter()
            .map(|(name, kind)| DirEntry { name, kind })
            .collect())
    }

    fn glob(&self, root: &Path, pattern: &str) -> Result<Vec<PathBuf>, DomainError> {
        // Минимальная поддержка, достаточная для тестов: `*.ext` и `**/*.ext`.
        let mut out = Vec::new();
        if let Some(extension) = pattern.strip_prefix("*.") {
            for path in self.files.keys() {
                if path.parent() == Some(root)
                    && path
                        .extension()
                        .is_some_and(|ext| ext == extension)
                {
                    out.push(path.clone());
                }
            }
        } else if let Some(extension) = pattern.strip_prefix("**/*.") {
            for path in self.files.keys() {
                if path.starts_with(root)
                    && path
                        .extension()
                        .is_some_and(|ext| ext == extension)
                {
                    out.push(path.clone());
                }
            }
        } else {
            // Точный относительный путь.
            let explicit = root.join(pattern);
            if self.files.contains_key(&explicit) {
                out.push(explicit);
            }
        }
        Ok(out)
    }

    fn walk_files(&self, root: &Path) -> Result<Vec<PathBuf>, DomainError> {
        Ok(self
            .files
            .keys()
            .filter(|path| path.starts_with(root))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::InMemoryFs;
    use crate::application::ports::FileSystem;
    use std::path::PathBuf;

    #[test]
    fn canonicalize_returns_normalized_path_for_existing_entry() {
        let root = PathBuf::from("/r");
        let mut fs = InMemoryFs::new(&root);
        fs.insert_file("a.txt", b"hi");
        let result = fs.canonicalize(&root, "a.txt").unwrap();
        assert_eq!(result, PathBuf::from("/r/a.txt"));
    }

    #[test]
    fn canonicalize_reports_missing_entry() {
        let root = PathBuf::from("/r");
        let fs = InMemoryFs::new(&root);
        assert!(fs.canonicalize(&root, "missing").is_err());
    }

    #[test]
    fn glob_star_ext_matches_direct_children_only() {
        let root = PathBuf::from("/r");
        let mut fs = InMemoryFs::new(&root);
        fs.insert_file("a.rs", b"");
        fs.insert_file("sub/b.rs", b"");

        let matches = fs.glob(&root, "*.rs").unwrap();
        assert_eq!(matches, vec![PathBuf::from("/r/a.rs")]);
    }

    #[test]
    fn glob_double_star_matches_all_levels() {
        let root = PathBuf::from("/r");
        let mut fs = InMemoryFs::new(&root);
        fs.insert_file("a.rs", b"");
        fs.insert_file("sub/b.rs", b"");

        let mut matches = fs.glob(&root, "**/*.rs").unwrap();
        matches.sort();
        assert_eq!(
            matches,
            vec![PathBuf::from("/r/a.rs"), PathBuf::from("/r/sub/b.rs")]
        );
    }
}
