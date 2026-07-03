//! Скан секретов / кредов / PII перед отправкой чего-либо во внешнюю модель.
//!
//! Для коммерческих / критичных проектов главное правило: наружу не должно уйти
//! ничего чувствительного. Этот модуль — **fail-closed** предохранитель: если в
//! тексте найден хоть один секрет, вызывающий обязан НЕ отправлять payload и
//! попросить модель переформулировать вопрос как абстрактный пример.
//!
//! Модуль **чистый** (нет сети/IO), детекторы высокоточные (лучше пере-
//! заблокировать, чем «протечь»). Возвращаются только замаскированные образцы —
//! сам секрет наружу/в лог не попадает.

use regex::Regex;

/// Одна находка сканера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Категория (`api-key`, `private-key`, `password`, `email`, …).
    pub kind: &'static str,
    /// Замаскированный образец — безопасно показать в сообщении/логе.
    pub sample: String,
}

struct Detector {
    kind: &'static str,
    pattern: &'static str,
}

/// Высокоточные детекторы. Порядок не важен; каждая находка репортится один раз
/// (по замаскированному образцу).
const DETECTORS: &[Detector] = &[
    Detector { kind: "private-key", pattern: r"-----BEGIN[ A-Z]*PRIVATE KEY-----" },
    Detector { kind: "api-key", pattern: r"sk-ant-[A-Za-z0-9_\-]{16,}" },
    Detector { kind: "api-key", pattern: r"sk-[A-Za-z0-9]{20,}" },
    Detector { kind: "api-key", pattern: r"gh[pousr]_[A-Za-z0-9]{20,}" },
    Detector { kind: "api-key", pattern: r"AKIA[0-9A-Z]{16}" },
    Detector { kind: "api-key", pattern: r"xox[baprs]-[A-Za-z0-9\-]{10,}" },
    Detector { kind: "api-key", pattern: r"AIza[0-9A-Za-z_\-]{35}" },
    Detector { kind: "jwt", pattern: r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}" },
    Detector { kind: "bearer-token", pattern: r"(?i)bearer\s+[A-Za-z0-9._\-]{16,}" },
    Detector {
        kind: "credential-assignment",
        pattern: r#"(?i)(password|passwd|pwd|secret|api[_-]?key|access[_-]?token|client[_-]?secret|private[_-]?key)\s*[:=]\s*["']?[^\s"']{4,}"#,
    },
    Detector { kind: "url-credentials", pattern: r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:@/]+:[^\s:@/]+@" },
    Detector { kind: "high-entropy-token", pattern: r"\b[A-Fa-f0-9]{40,}\b" },
    Detector { kind: "email", pattern: r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}" },
];

/// Значения-заглушки из примеров/доков — их присутствие НЕ считаем утечкой,
/// чтобы легитимный «абстрактный пример» не блокировался зря.
const PLACEHOLDER_NEEDLES: &[&str] = &[
    "example.com", "example.org", "@example", "user@host", "foo@bar",
    "your-", "changeme", "xxxxx", "placeholder", "redacted", "<token>",
    "api_key_here", "sk-xxx", "sk-...", "dummy", "test@test",
];

fn is_placeholder(sample: &str) -> bool {
    let lower = sample.to_ascii_lowercase();
    PLACEHOLDER_NEEDLES
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Маскирует секрет для показа: `sk-ab…` (первые 4 символа + …). Никогда не
/// раскрывает более 4 символов и не показывает хвост.
fn mask(matched: &str) -> String {
    let head: String = matched.chars().take(4).collect();
    format!("{head}…")
}

/// Сканирует текст и возвращает найденные секреты/PII (пустой вектор = чисто).
#[must_use]
pub fn scan(text: &str) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    for detector in DETECTORS {
        let Ok(regex) = Regex::new(detector.pattern) else {
            continue;
        };
        for matched in regex.find_iter(text) {
            let raw = matched.as_str();
            if is_placeholder(raw) {
                continue;
            }
            let sample = mask(raw);
            let already = findings
                .iter()
                .any(|f| f.kind == detector.kind && f.sample == sample);
            if !already {
                findings.push(Finding {
                    kind: detector.kind,
                    sample,
                });
            }
        }
    }
    findings
}

/// Человекочитаемая сводка находок для сообщения модели (без самих секретов).
#[must_use]
pub fn summarize(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("{} ({})", f.kind, f.sample))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{scan, summarize};

    #[test]
    fn flags_common_secrets() {
        assert!(scan("key = sk-abcdefghijklmnopqrstuvwxyz012345")
            .iter()
            .any(|f| f.kind == "api-key"));
        assert!(scan("token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456")
            .iter()
            .any(|f| f.kind == "api-key"));
        assert!(scan("aws AKIAIOSFODNN7EXAMPLE0")
            .iter()
            .any(|f| f.kind == "api-key"));
        assert!(scan("Authorization: Bearer abcdef0123456789ABCDEF")
            .iter()
            .any(|f| f.kind == "bearer-token"));
        assert!(scan("password = hunter2secret")
            .iter()
            .any(|f| f.kind == "credential-assignment"));
        assert!(scan("db = postgres://admin:s3cr3t@db.internal/app")
            .iter()
            .any(|f| f.kind == "url-credentials"));
        assert!(scan("-----BEGIN RSA PRIVATE KEY-----")
            .iter()
            .any(|f| f.kind == "private-key"));
        assert!(scan("contact alice@corp.io")
            .iter()
            .any(|f| f.kind == "email"));
    }

    #[test]
    fn ignores_clean_abstract_example() {
        let example = "fn add(a: i32, b: i32) -> i32 { a + b }  // why does this overflow?";
        assert!(scan(example).is_empty(), "clean example must not be flagged");
    }

    #[test]
    fn ignores_placeholders() {
        assert!(scan("email user@example.com").is_empty());
        assert!(scan("token = your-api-key-here").is_empty());
    }

    #[test]
    fn masks_secret_in_report() {
        let findings = scan("key = sk-abcdefghijklmnopqrstuvwxyz012345");
        let summary = summarize(&findings);
        assert!(summary.contains("sk-a…"), "masked: {summary}");
        assert!(
            !summary.contains("abcdefghijkl"),
            "must not reveal the secret: {summary}"
        );
    }
}
