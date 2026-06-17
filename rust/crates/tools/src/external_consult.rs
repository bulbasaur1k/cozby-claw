//! Обезличивание (anonymization) кода/текста перед отправкой во внешнюю LLM.
//!
//! Enterprise-сценарий: основная модель — внутренняя, но для отдельных сложных
//! вопросов агент может посоветоваться с более мощной внешней моделью. Имена
//! проектов/компаний обычно «утекают» через **неймспейсы/модули** и
//! **имена типов/классов**, поэтому перед отправкой они заменяются на
//! стабильные обезличенные плейсхолдеры (`T_1`, `N_1`, …). Маппинг обратимый —
//! ответ внешней модели де-анонимизируется назад в настоящие имена.
//!
//! Режим **консервативный** (по умолчанию): трогаем только namespace-сегменты
//! и PascalCase-типы. Логика кода, имена функций/переменных, общеизвестные
//! типы (`String`, `Vec`, `Option`, …) и стандартные модули (`std`, `core`, …)
//! остаются как есть, чтобы внешняя модель понимала структуру.
//!
//! Модуль **чистый** (нет сети/IO) — всё тестируется юнит-тестами.

use std::collections::BTreeMap;

use regex::{Captures, Regex};

/// Общеизвестные типы и стандартные модули, которые НЕ обезличиваем: они не
/// несут имён проектов/компаний, а их сокрытие только ухудшило бы понимание.
const ALLOWLIST: &[&str] = &[
    // --- ключевые слова / общие ---
    "Self", "self", "super", "crate",
    // --- стандартные модули (Rust + общие) ---
    "std", "core", "alloc", "collections", "sync", "io", "fmt", "vec", "string",
    "option", "result", "cell", "rc", "boxed", "borrow", "cmp", "ops", "iter",
    "path", "time", "thread", "error", "mem", "convert", "slice", "str", "num",
    "net", "process", "env", "fs", "future", "task", "pin", "marker", "hash",
    // --- общеизвестные типы Rust ---
    "Option", "Result", "Vec", "String", "Box", "Rc", "Arc", "HashMap", "BTreeMap",
    "HashSet", "BTreeSet", "Cell", "RefCell", "Mutex", "RwLock", "Cow", "Path",
    "PathBuf", "Some", "None", "Ok", "Err", "Ordering", "Duration", "Instant",
    "Error", "Display", "Debug", "Clone", "Copy", "Default", "Send", "Sync",
    "Sized", "Iterator", "IntoIterator", "From", "Into", "TryFrom", "TryInto",
    "Deref", "DerefMut", "Drop", "Future", "Pin", "Weak", "VecDeque", "Bytes",
    // --- общеизвестные типы из других экосистем ---
    "Integer", "Boolean", "Object", "List", "Map", "Set", "Array", "Exception",
    "Optional", "Stream", "Task", "Void", "Long", "Double", "Float", "Char",
    "Byte", "Short", "Number", "Date", "Promise", "Buffer", "Record",
];

/// Что именно скрыл плейсхолдер — для понятного отчёта в ревью/логе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Type,
    Namespace,
}

/// Накопитель обезличивания: хранит стабильный маппинг «настоящее имя →
/// плейсхолдер» и умеет применять его в обе стороны.
#[derive(Debug, Default, Clone)]
pub struct Anonymizer {
    /// real name → placeholder. Инъективен по построению (счётчики растут).
    forward: BTreeMap<String, String>,
    types: usize,
    namespaces: usize,
}

impl Anonymizer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает плейсхолдер для имени, переиспользуя уже выданный.
    fn placeholder(&mut self, name: &str, kind: Kind) -> String {
        if let Some(existing) = self.forward.get(name) {
            return existing.clone();
        }
        let placeholder = match kind {
            Kind::Type => {
                self.types += 1;
                format!("T_{}", self.types)
            }
            Kind::Namespace => {
                self.namespaces += 1;
                format!("N_{}", self.namespaces)
            }
        };
        self.forward.insert(name.to_string(), placeholder.clone());
        placeholder
    }

    /// Обезличивает текст: namespace-сегменты → `N_*`, PascalCase-типы → `T_*`.
    /// Идемпотентно по отношению к уже вставленным плейсхолдерам (они инертны:
    /// `T_1`/`N_1` не содержат строчных букв и не классифицируются как типы).
    #[must_use]
    pub fn anonymize(&mut self, text: &str) -> String {
        let after_decls = self.anonymize_declaration_paths(text);
        self.anonymize_paths_and_idents(&after_decls)
    }

    /// Пер-проход по декларациям пространств имён: `package a.b.C;`,
    /// `namespace A.B`, `using A.B;`, `import a.b.C`, `from a.b import C`.
    /// Точечные пути трогаем ТОЛЬКО здесь — иначе можно покалечить вызовы
    /// методов вида `obj.method()`.
    fn anonymize_declaration_paths(&mut self, text: &str) -> String {
        let re = Regex::new(
            r"(?m)^(?P<kw>\s*(?:package|namespace|using|import|from)\s+)(?P<path>[A-Za-z_][A-Za-z0-9_.]*)",
        )
        .expect("declaration regex is valid");
        re.replace_all(text, |caps: &Captures| {
            let keyword = &caps["kw"];
            let path = &caps["path"];
            let rewritten = path
                .split('.')
                .map(|segment| self.map_segment(segment, true))
                .collect::<Vec<_>>()
                .join(".");
            format!("{keyword}{rewritten}")
        })
        .into_owned()
    }

    /// Основной проход: `::`-пути (Rust/C++) и одиночные идентификаторы.
    /// `Regex::replace_all` сканирует исходную строку один раз и не
    /// перечитывает подстановки — поэтому вставленные плейсхолдеры безопасны.
    fn anonymize_paths_and_idents(&mut self, text: &str) -> String {
        let re = Regex::new(
            r"(?P<path>(?:[A-Za-z_][A-Za-z0-9_]*::)+[A-Za-z_][A-Za-z0-9_]*)|(?P<id>[A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("path/ident regex is valid");
        re.replace_all(text, |caps: &Captures| {
            if let Some(path) = caps.name("path") {
                let segments: Vec<&str> = path.as_str().split("::").collect();
                let last = segments.len() - 1;
                segments
                    .iter()
                    .enumerate()
                    .map(|(index, segment)| {
                        // Последний строчный сегмент `::` — это обычно функция /
                        // константа (`Type::new`), а не неймспейс: его не трогаем.
                        let is_path_position = index < last;
                        self.map_segment(segment, is_path_position)
                    })
                    .collect::<Vec<_>>()
                    .join("::")
            } else {
                self.map_segment(&caps["id"], false)
            }
        })
        .into_owned()
    }

    /// Классифицирует один сегмент. `namespace_position` = сегмент стоит как
    /// часть пути (не последним) → строчное имя считается неймспейсом.
    fn map_segment(&mut self, segment: &str, namespace_position: bool) -> String {
        if is_allowlisted(segment) {
            return segment.to_string();
        }
        if is_type_like(segment) {
            return self.placeholder(segment, Kind::Type);
        }
        if namespace_position && is_plain_identifier(segment) {
            return self.placeholder(segment, Kind::Namespace);
        }
        segment.to_string()
    }

    /// Возвращает текст внешней модели с восстановленными настоящими именами.
    #[must_use]
    pub fn deanonymize(&self, text: &str) -> String {
        let reverse: BTreeMap<&str, &str> = self
            .forward
            .iter()
            .map(|(real, placeholder)| (placeholder.as_str(), real.as_str()))
            .collect();
        let re = Regex::new(r"\b[TN]_\d+\b").expect("placeholder regex is valid");
        re.replace_all(text, |caps: &Captures| {
            let matched = &caps[0];
            (*reverse.get(matched).unwrap_or(&matched)).to_string()
        })
        .into_owned()
    }

    /// Список замен «настоящее → плейсхолдер» для показа в ревью и audit-логе.
    #[must_use]
    pub fn redactions(&self) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = self
            .forward
            .iter()
            .map(|(real, placeholder)| (placeholder.clone(), real.clone()))
            .collect();
        // Сортируем по номеру плейсхолдера для стабильного отчёта (T_1, T_2, …).
        entries.sort_by_key(|entry| placeholder_order(&entry.0));
        entries
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }
}

fn is_allowlisted(name: &str) -> bool {
    ALLOWLIST.contains(&name)
}

/// `PascalCase`: начинается с заглавной и содержит хотя бы одну строчную. Это
/// исключает `ALL_CAPS` константы, одиночные `T` и сами плейсхолдеры `T_1`.
fn is_type_like(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_uppercase())
        && name.chars().any(|c| c.is_ascii_lowercase())
}

/// Обычный идентификатор (для namespace-сегментов): начинается с буквы/`_`.
fn is_plain_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Порядок сортировки плейсхолдеров: сперва по букве (N/T), затем по номеру.
fn placeholder_order(placeholder: &str) -> (char, usize) {
    let mut chars = placeholder.chars();
    let prefix = chars.next().unwrap_or('Z');
    let number = placeholder
        .trim_start_matches(['T', 'N', '_'])
        .parse::<usize>()
        .unwrap_or(0);
    (prefix, number)
}

#[cfg(test)]
mod tests {
    use super::Anonymizer;

    #[test]
    fn redacts_pascal_case_types_but_keeps_common_types() {
        let mut anon = Anonymizer::new();
        let out = anon.anonymize("let client: InvoiceService = Vec::new();");
        assert!(out.contains("T_1"), "type redacted: {out}");
        assert!(!out.contains("InvoiceService"), "real name leaked: {out}");
        assert!(out.contains("Vec"), "common type kept: {out}");
    }

    #[test]
    fn redacts_namespace_segments_keeps_function_and_std() {
        let mut anon = Anonymizer::new();
        let out = anon.anonymize("acme::billing::InvoiceService::process()");
        // acme, billing → namespaces; InvoiceService → type; process → kept.
        assert!(!out.contains("acme"));
        assert!(!out.contains("billing"));
        assert!(!out.contains("InvoiceService"));
        assert!(out.contains("::process()"), "function name kept: {out}");

        let std_path = anon.anonymize("std::collections::HashMap");
        assert_eq!(std_path, "std::collections::HashMap", "std path untouched");
    }

    #[test]
    fn redacts_declaration_paths() {
        let mut anon = Anonymizer::new();
        let java = anon.anonymize("package com.acme.billing;");
        assert!(!java.contains("acme"), "company name leaked: {java}");
        assert!(java.starts_with("package "), "keyword kept: {java}");

        let mut anon2 = Anonymizer::new();
        let cs = anon2.anonymize("namespace Acme.Billing.Core");
        assert!(!cs.contains("Acme"));
        assert!(!cs.contains("Billing"));
    }

    #[test]
    fn round_trips_back_to_real_names() {
        let mut anon = Anonymizer::new();
        let source = "fn run(svc: acme::billing::InvoiceService) -> Result<Order, Error> { svc.charge() }";
        let hidden = anon.anonymize(source);
        // External model echoes the anonymized identifiers back; we restore them.
        let restored = anon.deanonymize(&hidden);
        assert_eq!(restored, source, "round-trip mismatch:\n{hidden}");
    }

    #[test]
    fn same_name_maps_consistently() {
        let mut anon = Anonymizer::new();
        let out = anon.anonymize("InvoiceService a; InvoiceService b;");
        let first = out.find("T_1").expect("first occurrence");
        let second = out.rfind("T_1").expect("second occurrence");
        assert_ne!(first, second, "both occurrences use the same placeholder");
        assert_eq!(anon.redactions().len(), 1, "one distinct redaction");
    }

    #[test]
    fn placeholders_are_inert_on_reanonymize() {
        let mut anon = Anonymizer::new();
        let once = anon.anonymize("struct InvoiceService;");
        let twice = anon.anonymize(&once);
        assert_eq!(once, twice, "re-anonymizing must not touch placeholders");
        assert_eq!(anon.redactions().len(), 1);
    }

    #[test]
    fn redactions_report_lists_real_to_placeholder() {
        let mut anon = Anonymizer::new();
        let _ = anon.anonymize("acme::InvoiceService");
        let redactions = anon.redactions();
        // Sorted: namespaces (N_*) then types (T_*).
        assert!(redactions.iter().any(|(ph, real)| ph == "N_1" && real == "acme"));
        assert!(redactions
            .iter()
            .any(|(ph, real)| ph == "T_1" && real == "InvoiceService"));
    }

    #[test]
    fn keeps_all_caps_constants_and_keywords() {
        let mut anon = Anonymizer::new();
        let out = anon.anonymize("const MAX_RETRIES: usize = 3; let x = Self::default();");
        assert!(out.contains("MAX_RETRIES"), "ALL_CAPS kept: {out}");
        assert!(out.contains("Self"), "Self kept: {out}");
        assert!(anon.is_empty(), "nothing should be redacted: {out}");
    }
}
