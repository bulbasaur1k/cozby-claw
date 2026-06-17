use serde_json::{json, Value as JsonValue};

/// Идентификатор инструмента, видимый по MCP-протоколу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    ReadFile,
    ListDir,
    Glob,
    Grep,
}

impl ToolKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListDir => "list_dir",
            Self::Glob => "glob",
            Self::Grep => "grep",
        }
    }

    /// Обратная связка `name -> kind`. Возвращает `None` для неизвестных имён.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "read_file" => Some(Self::ReadFile),
            "list_dir" => Some(Self::ListDir),
            "glob" => Some(Self::Glob),
            "grep" => Some(Self::Grep),
            _ => None,
        }
    }
}

/// Транспортонезависимое описание файлового инструмента. Инфраструктурный слой
/// переводит его в `runtime::McpTool`. Поле `name` — MCP-имя (`ToolKind::as_str`).
///
/// Инструменты внешних сервисов описываются не здесь, а контрактами
/// ([`crate::domain::contract`]) и собираются в `McpTool` напрямую.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: JsonValue,
    /// MCP-аннотации файловых инструментов: строго read-only и closed-world.
    pub annotations: JsonValue,
}

#[must_use]
pub fn tool_descriptors() -> Vec<ToolDescriptor> {
    let safe_annotations = json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "openWorldHint": false,
    });

    vec![
        ToolDescriptor {
            name: ToolKind::ReadFile.as_str(),
            description: "Read a UTF-8 text file inside --root. Returns up to 256 KiB of \
                          content. Paths are resolved relative to --root and rejected if \
                          they escape it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to --root" }
                },
                "required": ["path"],
            }),
            annotations: safe_annotations.clone(),
        },
        ToolDescriptor {
            name: ToolKind::ListDir.as_str(),
            description: "List entries of a directory inside --root. Returns name + kind \
                          for each entry.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Dir path relative to --root (default: .)"
                    }
                },
            }),
            annotations: safe_annotations.clone(),
        },
        ToolDescriptor {
            name: ToolKind::Glob.as_str(),
            description: "Glob-match files relative to --root. Returns a list of matching \
                          relative paths.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "e.g. '**/*.rs'" }
                },
                "required": ["pattern"],
            }),
            annotations: safe_annotations.clone(),
        },
        ToolDescriptor {
            name: ToolKind::Grep.as_str(),
            description: "Regex-search inside --root. Returns at most 500 matches formatted \
                          as path:line: text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust regex" },
                    "glob":    { "type": "string", "description": "Optional glob (e.g. '**/*.rs')" }
                },
                "required": ["pattern"],
            }),
            annotations: safe_annotations,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{tool_descriptors, ToolKind};

    #[test]
    fn exposes_exactly_four_tools() {
        let descriptors = tool_descriptors();
        assert_eq!(descriptors.len(), 4);
    }

    #[test]
    fn names_round_trip() {
        for kind in [
            ToolKind::ReadFile,
            ToolKind::ListDir,
            ToolKind::Glob,
            ToolKind::Grep,
        ] {
            assert_eq!(ToolKind::from_name(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(ToolKind::from_name("write_file").is_none());
    }

    #[test]
    fn all_tools_are_read_only_and_closed_world() {
        for descriptor in tool_descriptors() {
            assert_eq!(descriptor.annotations["readOnlyHint"], true);
            assert_eq!(descriptor.annotations["destructiveHint"], false);
            assert_eq!(descriptor.annotations["openWorldHint"], false);
        }
    }

    #[test]
    fn required_fields_are_declared_for_read_file_and_glob_and_grep() {
        let descriptors = tool_descriptors();
        let find = |name: &str| descriptors.iter().find(|d| d.name == name).unwrap();
        assert_eq!(find("read_file").input_schema["required"][0], "path");
        assert_eq!(find("glob").input_schema["required"][0], "pattern");
        assert_eq!(find("grep").input_schema["required"][0], "pattern");
    }
}
