/// Верхняя граница размера файла, который `read_file` вернёт целиком.
pub const MAX_READ_BYTES: u64 = 256 * 1024;

/// Верхняя граница количества совпадений, возвращаемых `grep` / `glob`.
pub const MAX_GREP_MATCHES: usize = 500;

/// Описывает, как был обрезан результат чтения файла. Используется
/// use-case'ом `read_file` для формирования итогового текста с припиской
/// «[truncated …]».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimit {
    pub max_bytes: u64,
}

impl Default for ReadLimit {
    fn default() -> Self {
        Self {
            max_bytes: MAX_READ_BYTES,
        }
    }
}

/// Декорирует прочитанный текст пометкой об обрезке, если полный размер
/// файла превысил лимит.
///
/// Чистая функция: не трогает ни FS, ни tokio.
#[must_use]
pub fn format_read_body(text: &str, full_len: u64, limit: ReadLimit) -> String {
    if full_len > limit.max_bytes {
        format!(
            "{text}\n\n[truncated at {} bytes; file size {full_len} bytes]",
            limit.max_bytes
        )
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{format_read_body, ReadLimit, MAX_READ_BYTES};

    #[test]
    fn default_limit_matches_public_constant() {
        assert_eq!(ReadLimit::default().max_bytes, MAX_READ_BYTES);
    }

    #[test]
    fn small_file_is_not_decorated() {
        let body = format_read_body("hello", 5, ReadLimit { max_bytes: 100 });
        assert_eq!(body, "hello");
    }

    #[test]
    fn file_at_limit_boundary_is_not_decorated() {
        // full_len == limit → not truncated (we actually read the whole file)
        let body = format_read_body("abc", 3, ReadLimit { max_bytes: 3 });
        assert_eq!(body, "abc");
    }

    #[test]
    fn oversized_file_gets_truncation_note() {
        let body = format_read_body("abc", 10_000, ReadLimit { max_bytes: 3 });
        assert!(body.starts_with("abc\n\n[truncated at 3 bytes; file size 10000 bytes]"));
    }
}
