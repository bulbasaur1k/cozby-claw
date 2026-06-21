//! Слэш-команды поля ввода GUI (как в CLI/Claude Code). Парсинг отделён от UI,
//! чтобы покрыть тестами; исполнение — в [`crate::app`].

/// Разобранная слэш-команда. `None` из [`parse`] означает «это не команда».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiCommand {
    /// Показать список команд.
    Help,
    /// Очистить транскрипт (история сессии на диске не трогается).
    Clear,
    /// Начать новую сессию.
    New,
    /// Показать использование токенов.
    Cost,
    /// Показать сводку: модель, каталог, сессия, режим прав, токены.
    Status,
    /// `git diff --stat` текущего рабочего дерева.
    Diff,
    /// Сменить модель основного провайдера (аргумент — id модели).
    Model(String),
    /// Экспортировать транскрипт в файл (опц. путь, иначе авто-имя).
    Export(Option<String>),
    /// Неизвестная команда (для подсказки пользователю).
    Unknown(String),
}

/// Разбирает ввод. Возвращает `None`, если строка не начинается с `/`.
#[must_use]
pub fn parse(input: &str) -> Option<GuiCommand> {
    let trimmed = input.trim();
    let body = trimmed.strip_prefix('/')?;
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let arg = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    Some(match name {
        "help" | "h" | "?" => GuiCommand::Help,
        "clear" | "cls" => GuiCommand::Clear,
        "new" => GuiCommand::New,
        "cost" | "tokens" => GuiCommand::Cost,
        "status" => GuiCommand::Status,
        "diff" => GuiCommand::Diff,
        "model" => GuiCommand::Model(arg.unwrap_or_default().to_string()),
        "export" => GuiCommand::Export(arg.map(str::to_string)),
        other => GuiCommand::Unknown(other.to_string()),
    })
}

/// Текст справки по доступным командам (для `/help`).
#[must_use]
pub fn help_text() -> String {
    [
        "Available commands:",
        "  /help            — show this list",
        "  /clear           — clear the transcript view",
        "  /new             — start a new session",
        "  /cost            — show token usage",
        "  /status          — model, directory, session, permissions, tokens",
        "  /diff            — git diff --stat of the working tree",
        "  /model <id>      — switch the primary model",
        "  /export [path]   — export the transcript to a markdown file",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{parse, GuiCommand};

    #[test]
    fn non_command_returns_none() {
        assert_eq!(parse("hello world"), None);
        assert_eq!(parse("  not a slash"), None);
    }

    #[test]
    fn parses_bare_commands_and_aliases() {
        assert_eq!(parse("/help"), Some(GuiCommand::Help));
        assert_eq!(parse("/h"), Some(GuiCommand::Help));
        assert_eq!(parse("/clear"), Some(GuiCommand::Clear));
        assert_eq!(parse("/cls"), Some(GuiCommand::Clear));
        assert_eq!(parse("/new"), Some(GuiCommand::New));
        assert_eq!(parse("/cost"), Some(GuiCommand::Cost));
        assert_eq!(parse("/tokens"), Some(GuiCommand::Cost));
        assert_eq!(parse("/status"), Some(GuiCommand::Status));
        assert_eq!(parse("/diff"), Some(GuiCommand::Diff));
    }

    #[test]
    fn parses_arguments_and_trims() {
        assert_eq!(
            parse("/model  qwen/qwen3-coder "),
            Some(GuiCommand::Model("qwen/qwen3-coder".to_string()))
        );
        assert_eq!(
            parse("/export notes.md"),
            Some(GuiCommand::Export(Some("notes.md".to_string())))
        );
        assert_eq!(parse("/export"), Some(GuiCommand::Export(None)));
        // Пустой аргумент модели не превращается в Some("").
        assert_eq!(parse("/model"), Some(GuiCommand::Model(String::new())));
    }

    #[test]
    fn unknown_command_is_reported() {
        assert_eq!(
            parse("/frobnicate x"),
            Some(GuiCommand::Unknown("frobnicate".to_string()))
        );
    }
}
