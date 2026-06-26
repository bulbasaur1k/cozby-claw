# cozby-claw — руководство пользователя

Локальный кодинг-агент на Rust с терминальным интерфейсом.

- **`cozby-claw-cli`** — терминальный REPL (как Claude Code в консоли).

Движок (`ConversationRuntime`), набор инструментов и файл конфигурации провайдеров
`~/.claw/providers.toml` — общие для всех запусков.

---

## 1. Установка

Сборка release-бинаря и установка в `~/.local/bin` (на PATH):

```bash
./release.sh
# другой каталог:  COZBY_BIN_DIR=/usr/local/bin ./release.sh
```

После этого из любого терминала:

```bash
cozby-claw-cli      # REPL
```

Запуск без установки — из каталога `rust/`:

```bash
cargo run -p rusty-claude-cli      # CLI (быстрее: --release)
```

---

## 2. Настройка провайдера

Конфиг: **`~/.claw/providers.toml`** (вне репозитория, права 600). Ключи лежат
прямо в файле. Каждый слот указывает **протокол** провайдера ключом `type`
(как `protocol` в qwen-code):

| `type` | Что это | Endpoint |
|---|---|---|
| `anthropic` | нативный Anthropic API | `/v1/messages` |
| `openai` | любой OpenAI-совместимый провайдер | `/v1/chat/completions` |

`openai` покрывает **любой** OpenAI-совместимый сервис — OpenRouter, qwen/DashScope,
DeepSeek, локальный llama.cpp / Ollama / LM Studio и т.п. «Кастомный» провайдер =
`type = "openai"` + свой `base_url`.

```toml
[primary]                       # основная модель агента
type   = "openai"               # "openai" | "anthropic"
model  = "qwen/qwen3-coder"
base_url = "https://openrouter.ai/api/v1"
api_key  = "sk-or-…"
max_tokens = 8192
permission_mode = "workspace-write"

[auxiliary]                     # опц. — «более сильная» модель для consult-инструмента
type   = "openai"
model  = "qwen/qwen3-235b-a22b-2507"
base_url = "https://openrouter.ai/api/v1"
api_key  = "sk-or-…"

# [embedder]                    # зарезервировано под будущий RAG (пока не используется)
```

Примеры разных протоколов:

```toml
# Нативный Anthropic (api.anthropic.com) — ключ из файла или OAuth/env
[primary]
type  = "anthropic"
model = "claude-opus-4-8"
api_key = "sk-ant-…"            # пусто → берётся OAuth/ANTHROPIC_API_KEY из окружения

# DeepSeek (OpenAI-совместимый)
[primary]
type  = "openai"
model = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"
api_key  = "sk-…"

# Локальный сервер (Ollama / LM Studio / llama.cpp)
[primary]
type  = "openai"
model = "qwen2.5-coder"
base_url = "http://localhost:11434/v1"
api_key  = "ollama"            # многие локальные серверы игнорируют ключ
```

> Старый ключ `kind` (вместо `type`) всё ещё принимается для совместимости с
> файлами от прежних версий.

Альтернатива — переменные окружения:

| Провайдер | Переменные |
|---|---|
| OpenAI-совместимый | `OPENAI_API_KEY`, `OPENAI_BASE_URL` |
| Anthropic | `ANTHROPIC_API_KEY` (или `ANTHROPIC_AUTH_TOKEN`), `ANTHROPIC_BASE_URL` |

**Через OpenRouter всегда `type = "openai"`** (даже для Opus: `model = "anthropic/claude-opus-4.8"`).
`type = "anthropic"` — только для нативного `api.anthropic.com`.

> ⚠️ «Думающие» модели вроде `qwen/qwen3-235b-a22b` на OpenRouter иногда уводят весь
> ответ в reasoning и оставляют content пустым → агент остаётся без текста. Для
> работы с tools используйте `qwen/qwen3-coder`, `qwen/qwen3-32b` или
> `qwen/qwen3-235b-a22b-2507` (не-«думающий»).

---

## 3. CLI — как работать

### Запуск

```bash
cozby-claw-cli                              # интерактивный REPL
cozby-claw-cli "summarize this repo"        # один запрос и выход
cozby-claw-cli prompt "explain src/main.rs" # то же, явной командой
cozby-claw-cli --resume latest              # продолжить последнюю сессию
```

### Полезные флаги

| Флаг | Что даёт |
|---|---|
| `--model <id>` | переопределить модель на запуск |
| `--permission-mode <mode>` | `read-only` / `workspace-write` / `danger-full-access` |
| `--allowedTools read,glob,bash` | ограничить набор инструментов |
| `--output-format text\|json` | формат вывода в неинтерактивном режиме |
| `--resume [SESSION\|latest]` | продолжить сохранённую сессию |
| `--dangerously-skip-permissions` | пропустить все проверки прав |

### Сабкоманды (без входа в REPL)

```bash
cozby-claw-cli status        # статус воркспейса, git, последние коммиты
cozby-claw-cli config show   # итоговый merged-конфиг (JSON)
cozby-claw-cli agents        # список доступных под-агентов
cozby-claw-cli mcp           # MCP-серверы
cozby-claw-cli skills        # доступные навыки
cozby-claw-cli hook list     # зарегистрированные хуки
cozby-claw-cli sandbox       # снимок изоляции
cozby-claw-cli login/logout  # OAuth для Anthropic
cozby-claw-cli help          # полная справка
```

### Слэш-команды внутри REPL (что дают)

| Команда | Что делает |
|---|---|
| `/help` | список команд |
| `/clear` | очистить контекст диалога |
| `/status` | живой контекст: модель, токены, git, права |
| `/cost` | стоимость и расход токенов |
| `/model [id]` | показать/сменить модель |
| `/diff` | дифф рабочего дерева |
| `/commit` | сделать git-коммит изменений |
| `/compact` | сжать историю (компакция контекста) |
| `/memory` | работа с долговременной памятью |
| `/session` `/resume` | управление сессиями |
| `/export <файл>` | выгрузить транскрипт |
| `/mcp` `/plugins` `/skills` `/agents` | управление расширениями |
| `/permissions` `/sandbox` `/config` | права, изоляция, конфиг |
| `/external` | спросить внешнюю (вспомогательную) модель |
| `/pr` `/issue` `/init` `/doctor` `/stats` | git-PR/issue, init проекта, диагностика |
| `/exit` `/quit` | выход |

> Часть команд (`/plan /review /context /usage /rename /copy /effort /branch …`)
> пока заглушки и печатают «registered but not yet implemented».

---

## 4. Режимы прав (что дают)

| Режим | Поведение |
|---|---|
| `read-only` | только чтение (read/glob/grep/web); запись/bash — запрет/запрос |
| `workspace-write` | + правки файлов в воркспейсе без подтверждения; `bash` спрашивает |
| `danger-full-access` | всё разрешено без вопросов |
| `prompt` | спрашивать на каждый чувствительный вызов |

Правила `allow/deny/ask` можно задать в `.claw`-конфиге проекта — они применяются
поверх режима. Запросы к **основной** модели и запуск под-агентов **не требуют
подтверждения**; платная **внешняя** модель (`consult_external_model`) спрашивает
отдельно (показывает payload → y/N).

---

## 5. Что умеет агент (инструменты)

| Инструмент | Назначение |
|---|---|
| `read_file` / `write_file` / `edit_file` | чтение и точечные правки файлов |
| `glob_search` / `grep_search` | поиск файлов по маске и по содержимому |
| `bash` | выполнить команду; `run_in_background: true` — **в фоне** (вернёт task-id) |
| `WebFetch` / `WebSearch` | загрузить страницу / веб-поиск |
| `Agent` | запустить **под-агента** на подзадачу (см. ниже) |
| `Task*` | реестр фоновых задач (create/get/list/stop/update/output) |
| `Worker*` | долгоживущие воркеры (create/observe/send-prompt/await-ready/…) |
| `consult_external_model` | спросить вспомогательную (более сильную) модель |
| `mcp__<server>__<tool>` | вызвать инструмент подключённого MCP-сервера |
| `TodoWrite` `Skill` `Memory` … | задачи, навыки, память |

---

## 6. Фоновые задачи и под-агенты

- **Фоновый bash** — `bash` с `run_in_background: true` спавнит процесс и
  возвращает `background_task_id`, не блокируя ход.
- **Под-агенты (`Agent`)** — запускаются **асинхронно**: отдельный поток с
  собственным `ConversationRuntime`, ответ сразу (`status: running` + манифест),
  по завершении — `completed` + финальный текст. Главный агент не блокируется и
  может опрашивать статус.
- **Task/Worker** — слои для трекинга задач и управления долгими воркерами.

Чтобы агент делегировал — попросите явно: «используй Agent-tool, чтобы под-агент
сделал X».

---

## 7. Расширения (из `.claw`-конфига)

CLI читает `.claw` проекта и подключает:

- **MCP-серверы** — инструменты внешних серверов (объявляются модели как `mcp__*`).
- **Hooks** — команды до/после вызова инструментов.
- **Plugins** — дополнительные инструменты.
- **Permission-rules** — allow/deny/ask поверх режима прав.
- **Auto-compaction** — автосжатие длинного контекста.
- **Sandbox** — изоляция выполнения.

---

## 8. Сессии и память

- Каждый ход автосохраняется в `<cwd>/.claw/sessions/<session_id>.jsonl`.
- `--resume latest` / `/resume` / `/session` — вернуться к сессии;
  `/memory` — долговременная память между сессиями; `/export` — выгрузка в markdown.

---

## 9. Быстрый старт

```bash
# 1) положить ключ
mkdir -p ~/.claw && cat > ~/.claw/providers.toml <<'TOML'
[primary]
type = "openai"
model = "qwen/qwen3-coder"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-…"
TOML

# 2) собрать и установить
./release.sh

# 3) поехали
cozby-claw-cli          # терминал
```

---

## 10. Конфиг и разработка

### Порядок загрузки runtime-конфига

Конфиг проекта (`.claw`) мёржится в таком порядке (последующее перекрывает предыдущее):

1. `~/.claw.json`
2. `~/.config/claw/settings.json`
3. `<repo>/.claw.json`
4. `<repo>/.claw/settings.json`
5. `<repo>/.claw/settings.local.json`

> Файл провайдеров `~/.claw/providers.toml` (см. §2) — отдельный от этого, отвечает
> за выбор модели/ключа; `.claw/settings*` — за фичи (hooks, permission-rules, MCP,
> sandbox, compaction).

### Сборка и тесты

```bash
cd rust
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets
```

### Mock-parity harness

В воркспейсе есть детерминированный Anthropic-совместимый mock-сервис и harness для
проверки поведенческого паритета:

```bash
cd rust
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0   # поднять мок вручную
```

### Крейты воркспейса

`api`, `commands`, `compat-harness`, `cozby-mcp`, `mock-anthropic-service`,
`plugins`, `runtime`, `rusty-claude-cli`, `telemetry`, `tools`.

- `rusty-claude-cli` → бинарь `cozby-claw-cli`.
- `api` — клиенты провайдеров (Anthropic + OpenAI-совместимые) и `providers.toml`.
