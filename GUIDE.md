# cozby-claw — руководство пользователя

Локальный кодинг-агент на Rust: два фронтенда поверх общего ядра.

- **`cozby-claw-cli`** — терминальный REPL (как Claude Code в консоли).
- **`cozby-claw-gui`** — десктоп-окно на egui.

Оба используют один движок (`ConversationRuntime`), один набор инструментов и
один файл конфигурации провайдеров `~/.claw/providers.toml`.

---

## 1. Установка

Сборка release-бинарей и установка в `~/.local/bin` (на PATH):

```bash
./release.sh
# другой каталог:  COZBY_BIN_DIR=/usr/local/bin ./release.sh
```

После этого из любого терминала:

```bash
cozby-claw-cli      # REPL
cozby-claw-gui      # GUI
```

Запуск без установки — из каталога `rust/`:

```bash
cargo run -p rusty-claude-cli      # CLI
cargo run -p gui                    # GUI (быстрее: --release)
```

---

## 2. Настройка провайдера

Конфиг общий для CLI и GUI: **`~/.claw/providers.toml`** (вне репозитория, права 600).
Ключи лежат прямо в файле.

```toml
[primary]                       # основная модель агента
kind   = "openai"               # "openai" (любой OpenAI-совместимый) | "anthropic"
model  = "qwen/qwen3-coder"
base_url = "https://openrouter.ai/api/v1"
api_key  = "sk-or-…"
max_tokens = 8192
permission_mode = "workspace-write"

[auxiliary]                     # опц. — «более сильная» модель для consult-инструмента
kind   = "openai"
model  = "qwen/qwen3-235b-a22b-2507"
base_url = "https://openrouter.ai/api/v1"
api_key  = "sk-or-…"

# [embedder]                    # зарезервировано под будущий RAG (пока не используется)
```

Альтернатива — переменные окружения:

| Провайдер | Переменные |
|---|---|
| OpenAI-совместимый | `OPENAI_API_KEY`, `OPENAI_BASE_URL` |
| Anthropic | `ANTHROPIC_API_KEY` (или `ANTHROPIC_AUTH_TOKEN`), `ANTHROPIC_BASE_URL` |

**Через OpenRouter всегда `kind = "openai"`** (даже для Opus: `model = "anthropic/claude-opus-4.8"`).
`kind = "anthropic"` — только для нативного `api.anthropic.com`.

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

## 4. GUI — как работать

```bash
cozby-claw-gui
```

- **Левая панель** — сессии: «➕ New session», список с заголовками (по первому
  сообщению), 🗑 для удаления. Сессии автосохраняются в `<cwd>/.claw/sessions/`.
- **⚙ Settings** — провайдер (Openai/Anthropic), модель, base-url, ключ,
  max-tokens, режим прав. «Save & reconnect» пишет в `~/.claw/providers.toml`.
- **Шапка** — модель, статус, токены, галка «reasoning», живой индикатор активности
  («LLM request…», «running: shell/files/MCP · …», «sub-agent: …»).
- **Поле ввода** — Ctrl+Enter отправить. Поддерживает слэш-команды:
  `/help /clear /new /cost /status /diff /model /export`.
- **Модалки** — подтверждение опасного инструмента (Allow/Deny) и вопрос модели
  (`AskUserQuestion`: варианты + свободный ответ + Skip).
- **⏹ Stop** — кооперативно отменяет текущий ход (и под-агентов).

---

## 5. Режимы прав (что дают)

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

## 6. Что умеет агент (инструменты)

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

## 7. Фоновые задачи и под-агенты

- **Фоновый bash** — `bash` с `run_in_background: true` спавнит процесс и
  возвращает `background_task_id`, не блокируя ход.
- **Под-агенты (`Agent`)** — в **CLI** запускаются **асинхронно**: отдельный поток
  с собственным `ConversationRuntime`, ответ сразу (`status: running` + манифест),
  по завершении — `completed` + финальный текст. Главный агент не блокируется и
  может опрашивать статус. В **GUI** под-агент пока **синхронный** (родитель ждёт),
  его шаги стримятся вложенным блоком `⤷ sub-agent`.
- **Task/Worker** — слои для трекинга задач и управления долгими воркерами.

Чтобы агент делегировал — попросите явно: «используй Agent-tool, чтобы под-агент
сделал X».

---

## 8. Расширения (из `.claw`-конфига)

GUI и CLI читают `.claw` проекта и подключают:

- **MCP-серверы** — инструменты внешних серверов (объявляются модели как `mcp__*`).
- **Hooks** — команды до/после вызова инструментов.
- **Plugins** — дополнительные инструменты (в CLI; в GUI пока нет).
- **Permission-rules** — allow/deny/ask поверх режима прав.
- **Auto-compaction** — автосжатие длинного контекста.
- **Sandbox** — изоляция выполнения.

---

## 9. Сессии и память

- Каждый ход автосохраняется в `<cwd>/.claw/sessions/<session_id>.jsonl`.
- В GUI: переключение/удаление сессий в сайдбаре; `/export` — выгрузка в markdown.
- В CLI: `--resume latest` / `/resume` / `/session` — вернуться к сессии;
  `/memory` — долговременная память между сессиями.

---

## 10. Быстрый старт

```bash
# 1) положить ключ
mkdir -p ~/.claw && cat > ~/.claw/providers.toml <<'TOML'
[primary]
kind = "openai"
model = "qwen/qwen3-coder"
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-…"
TOML

# 2) собрать и установить
./release.sh

# 3) поехали
cozby-claw-cli          # терминал
cozby-claw-gui          # окно
```

---

## 11. Конфиг и разработка

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

`api`, `appcore`, `commands`, `compat-harness`, `cozby-mcp`, `gui`,
`mock-anthropic-service`, `plugins`, `runtime`, `rusty-claude-cli`, `telemetry`, `tools`.

- `appcore` — общее ядро фронтендов (выбор провайдера, системный промпт, фич-конфиг).
- `rusty-claude-cli` → бинарь `cozby-claw-cli`; `gui` → бинарь `cozby-claw-gui`.
