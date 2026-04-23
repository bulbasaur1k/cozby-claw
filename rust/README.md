# cozby-claw — Rust workspace

Rust-реализация CLI-агента и окружения для `cozby-claw`.

Для task-ориентированного гайда с copy-paste примерами см. [`../USAGE.md`](../USAGE.md). Для гида по деплою в закрытых сетях с локальными LLM — [`../SECURITY.md`](../SECURITY.md).

## Быстрый старт

```bash
cd rust/
cargo build --workspace

# Помощь
cargo run -p rusty-claude-cli -- --help

# Интерактивный REPL
cargo run -p rusty-claude-cli -- --model claude-opus-4-6

# Одношаговый prompt
cargo run -p rusty-claude-cli -- prompt "explain this codebase"

# JSON-вывод
cargo run -p rusty-claude-cli -- --output-format json prompt "summarize src/main.rs"
```

## Конфигурация

```bash
# Прямой API-ключ (Anthropic-совместимый эндпоинт)
export ANTHROPIC_API_KEY="sk-ant-..."
# Локальный inference-сервер или корпоративный прокси
export ANTHROPIC_BASE_URL="http://inference.internal:8443"

# Либо OpenAI-совместимый провайдер (vLLM, llama.cpp, Ollama и т.п.)
export OPENAI_API_KEY="dummy"
export OPENAI_BASE_URL="http://inference.internal:8080/v1"
```

OAuth-логин, если нужен:

```bash
cargo run -p rusty-claude-cli -- login
```

## Состав workspace

```
rust/
├── Cargo.toml
├── Cargo.lock
└── crates/
    ├── api/                   # HTTP-клиент, SSE-парсер, провайдеры
    ├── commands/              # Slash-команды
    ├── compat-harness/        # Behaviour-harness
    ├── cozby-mcp/             # MCP stdio-сервер (hex-архитектура — см. ниже)
    ├── mock-anthropic-service/# Детерминированный мок `/v1/messages`
    ├── plugins/               # Plugin registry и hook-points
    ├── runtime/               # Session, Config, Permissions, MCP-клиент, Prompt
    ├── rusty-claude-cli/      # CLI-бинарь `claw`
    ├── telemetry/             # Типы trace-событий (только локальный JSONL sink)
    └── tools/                 # Встроенные инструменты (Bash/Read/Write/Grep/...)
```

## Эталонная архитектура — крейт `cozby-mcp`

Новые крейты проекта следуют hexagonal / clean-архитектуре. Пример —
`cozby-mcp`:

```
src/
├── domain/              # Чистый Rust: правила, ошибки, лимиты, описания инструментов.
│   ├── errors.rs
│   ├── limits.rs
│   ├── path_guard.rs    # ensure_under_root — чистая функция, unit-тесты без FS
│   └── tools.rs
├── application/
│   ├── ports.rs         # trait FileSystem (DirEntry, ReadOutcome)
│   └── use_cases.rs     # read_file / list_dir / glob / grep — тестируются на InMemoryFs
├── infrastructure/
│   ├── config.rs        # argv parser
│   ├── fs_adapter.rs    # StdFileSystem над std::fs + walkdir + glob
│   ├── mcp_bridge.rs    # Мост к `runtime::McpServer` (специфика MCP-транспорта)
│   └── in_memory_fs.rs  # Тест-дубль для application-слоя
├── bootstrap.rs         # wire_server(args, fs) -> McpServer
├── lib.rs               # Публичный фасад
└── main.rs              # Тонкий entrypoint
```

Правила:

- Направление зависимостей: `infrastructure → application → domain`. Никогда наоборот.
- Domain не знает про `tokio`, `reqwest`, `std::fs`, сеть. Только чистый Rust.
- Application зависит только от portов (trait-интерфейсов). Тесты используют mock-реализации.
- Infrastructure реализует порты. Сюда же попадают fraзмементы, специфичные для framework'ов (MCP, Axum, sqlx, Ractor).
- Бизнес-логика **не** живёт в handler'ах / транспортных адаптерах.

Ractor не использован в `cozby-mcp` намеренно — у stdio-сервера нет
долгоживущего mutable state. Когда понадобится (worker, session, cron),
акторы появятся в `infrastructure/actors/`.

## Лицензия

См. `../LICENSE`.
