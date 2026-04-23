use std::path::Path;

use super::errors::DomainError;

/// Проверяет, что *уже канонизированный* путь лежит под *уже канонизированным*
/// корнем. Ничего не делает с файловой системой — чистая функция.
///
/// Канонизация делегирована порту `FileSystem` (infrastructure); domain
/// отвечает только за безопасное сравнение префиксов.
pub fn ensure_under_root(canonical: &Path, root: &Path) -> Result<(), DomainError> {
    if canonical.starts_with(root) {
        Ok(())
    } else {
        Err(DomainError::PathEscape {
            candidate: canonical.to_path_buf(),
            root: root.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_under_root;
    use super::DomainError;
    use std::path::PathBuf;

    #[test]
    fn accepts_direct_child() {
        let root = PathBuf::from("/srv/project");
        let canonical = PathBuf::from("/srv/project/src/main.rs");
        assert!(ensure_under_root(&canonical, &root).is_ok());
    }

    #[test]
    fn accepts_root_itself() {
        let root = PathBuf::from("/srv/project");
        assert!(ensure_under_root(&root, &root).is_ok());
    }

    #[test]
    fn rejects_parent_escape() {
        let root = PathBuf::from("/srv/project");
        let canonical = PathBuf::from("/etc/passwd");
        let err = ensure_under_root(&canonical, &root).expect_err("must reject");
        assert!(matches!(err, DomainError::PathEscape { .. }));
    }

    #[test]
    fn rejects_sibling_prefix_trap() {
        // Classic starts_with trap: /srv/project2 starts with "/srv/project" as a
        // string but is NOT under /srv/project. starts_with on Path is
        // component-aware, so this must be rejected.
        let root = PathBuf::from("/srv/project");
        let canonical = PathBuf::from("/srv/project2/file.rs");
        assert!(ensure_under_root(&canonical, &root).is_err());
    }

    #[test]
    fn path_escape_error_carries_both_sides() {
        let root = PathBuf::from("/a");
        let canonical = PathBuf::from("/b");
        let err = ensure_under_root(&canonical, &root).unwrap_err();
        match err {
            DomainError::PathEscape { candidate, root: reported_root } => {
                assert_eq!(candidate, canonical);
                assert_eq!(reported_root, root);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
