# Архитектура приложения `cozby-claw`

## Обзор
`cozby-claw` — локальный кодинг-агент на Rust с терминальным интерфейсом. Он
объединяет CLI-фронтенд, движок диалога с агентом, работу с внешними API моделей,
управление инструментами и расширениями (MCP, hooks, plugins).

Репозиторий — это Cargo workspace (`rust/`) из нескольких крейтов:

| Крейт | Путь | Предназначение |
| :--- | :--- | :--- |
| `rusty-claude-cli` | `rust/crates/rusty-claude-cli` | CLI-фронтенд → бинарь `cozby-claw-cli` |
| `runtime` | `rust/crates/runtime` | Движок агента (`ConversationRuntime`), сессии, права, MCP/hooks/plugins, конфиг |
| `tools` | `rust/crates/tools` | Встроенные инструменты агента |
| `api` | `rust/crates/api` | Клиенты провайдеров (Anthropic + OpenAI-совместимые), `providers.toml` |
| `commands` | `rust/crates/commands` | Слэш-команды |
| `plugins` | `rust/crates/plugins` | Загрузка плагинов и их хуков |
| `cozby-mcp` | `rust/crates/cozby-mcp` | Отдельный stdio MCP-сервер: файловые инструменты + HTTP-контракты |
| `telemetry` | `rust/crates/telemetry` | Телеметрия |
| `compat-harness` / `mock-anthropic-service` | `rust/crates/…` | Harness и mock для проверки паритета |

---

## Роль ключевых крейтов

### 1. `rusty-claude-cli`
Терминальный фронтенд (REPL + неинтерактивный режим). Парсит аргументы и слэш-команды,
строит рантайм из конфигурации (`providers.toml` + `.claw/`), рендерит транскрипт,
запросы разрешений и индикаторы прогресса в терминале. Бинарь — `cozby-claw-cli`.

### 2. `runtime`
Ядро бизнес-логики агента.

- `ConversationRuntime` — цикл хода: принимает промпт, зовёт LLM через `ApiClient`,
  обрабатывает tool-calls, применяет права, возвращает результат.
- `Session` — история сообщений (user/assistant/tool), автосохранение в `.claw/sessions/`.
- `PermissionPolicy` / `PermissionEnforcer` — режимы прав и allow/deny/ask-правила.
- Подсистемы: MCP-клиент, hooks, plugin-lifecycle, auto-compaction, sandbox, конфиг-лоадер.

### 3. `tools`
Встроенные инструменты как функции Rust: парсят `serde_json::Value`, возвращают JSON
или ошибку. `bash`, `read_file`/`write_file`/`edit_file`, `glob_search`/`grep_search`,
`WebFetch`/`WebSearch`, `Agent` (под-агенты), `consult_external_model`, и др.

### 4. `api`
Сетевой слой к моделям. Единый `ProviderClient` поверх двух протоколов:

- **Anthropic** (`AnthropicClient`) — нативный `/v1/messages`.
- **OpenAI-совместимый** (`OpenAiCompatClient`) — `/v1/chat/completions` для любого
  совместимого endpoint (OpenRouter, qwen/DashScope, DeepSeek, локальный Ollama/LM Studio).

Выбор протокола — поле `type` в слоте `providers.toml` (`anthropic` | `openai`).
`runtime` не зависит от конкретного провайдера, а работает с `ApiClient`.

---

## Точки входа и основной поток

1. **Запуск:** `cozby-claw-cli` (REPL или один запрос).
2. **Сборка рантайма:** из `~/.claw/providers.toml` выбирается провайдер и модель,
   из `.claw/` проекта — фичи (права, hooks, MCP, sandbox, compaction).
3. **Цикл хода:** `ConversationRuntime` принимает промпт → зовёт LLM →
   обрабатывает tool-calls → проверяет права → стримит результат в терминал.
4. **Инструменты:** вызов инструмента делегируется в `tools` (или в MCP-сервер для
   `mcp__*`); результат возвращается агенту.

---

## Выводы
- **`rusty-claude-cli`** — фронтенд/клиент.
- **`runtime`** — ядро бизнес-логики агента.
- **`tools`** — библиотека инструментов.
- **`api`** — слой интеграции с LLM (Anthropic + OpenAI-совместимые).

Приложение построено вокруг идеи **агента, управляющего выполнением инструментов**,
настраиваемого через TOML-конфиги и работающего с разными провайдерами моделей.
