# Конфигурация cozby-claw

## Где лежат файлы

| Scope | Путь |
|---|---|
| User | `~/.claw/` (или `$CLAW_CONFIG_HOME`) |
| Project | `<repo>/.claw/` |
| Local | `<repo>/.claw/` (только `*.local.*`) |

**Приоритет:** `user < project < local`. Внутри одного scope **TOML переопределяет JSON**.

Читаемые файлы (в порядке возрастания приоритета):
```
~/.claw/settings.json → ~/.claw/settings.toml → ~/.claw/mcp.toml
<repo>/.claw/settings.json → .claw/settings.toml → .claw/mcp.toml
<repo>/.claw/settings.local.json → .claw/settings.local.toml
```

`~/.claw` — единый дом пользовательской конфигурации: модели (`providers.toml`),
настройки (`settings.*`), MCP (`mcp.toml`), скилы (`skills/`) и агенты (`agents/`).

### Скилы и агенты

| Тип | User (дом) | Project |
|---|---|---|
| Скилы | `~/.claw/skills/<name>/SKILL.md` | `<repo>/.claw/skills/…` |
| Агенты | `~/.claw/agents/<name>.toml` | `<repo>/.claw/agents/…` |

`~/.claw` имеет приоритет над легаси-каталогами (`~/.codex`, `~/.claude`,
`~/.agents`), которые по-прежнему подхватываются. Смотреть подхваченное:
`claw skills`, `claw agents`.

### Дефолты при первом запуске

Если в `~/.claw` ещё нет соответствующих файлов, при первом запуске claw
**один раз** засевает (и больше не перезаписывает — правь/удаляй свободно).
Всё засеваемое **локально и офлайн** — ничего не уходит наружу (дефолт для
коммерческих / критичных проектов):

- **Прозрачные скилы** (чистые локальные процедуры, без внешних вызовов):
  `verify`, `grounding`, `plan`, `debug`, `test-first`, `refactor`,
  `code-review`, `security-review`, `commit`, `pr-description`, `changelog`,
  `adr`, `rfc`, `mermaid`.
- **`hooks/verify.sh` + `settings.toml`** — self-repair loop: после каждой
  правки файла запускает локальный check/lint по стеку проекта
  (`cargo check`/`clippy` · `tsc`/`biome` · `dotnet build` · `hadolint` ·
  `shellcheck`) и возвращает ошибки модели как контекст. Не блокирует правку.
  Выключить: `CLAW_VERIFY=0`; только check без clippy/тестов: `CLAW_VERIFY_FAST=1`.
- **`mcp.toml`** — внешние серверы (Context7/fetch) **закомментированы**: по
  умолчанию claw не поднимает ничего с исходящим трафиком. Раскомментируй
  осознанно.
- **`plugins.toml`** — список git-плагинов (по умолчанию наш `cozby-docs`). На
  интерактивном старте claw сообщит «N новых плагинов — запусти `claw plugins sync`»;
  установка **явная** (`sync` клонирует → собирает пакеты → ставит).
- пустой каталог `agents/`.

stdio-MCP с командой, которой нет в `PATH`, **автоматически пропускается** при
старте (не запускается и не спамит ошибками).

## settings.json / settings.toml — общие настройки

JSON и TOML взаимозаменяемы (TOML удобнее писать руками). Ключи одинаковые.

```toml
model = "internal-model"          # модель по умолчанию
permissionMode = "workspace-write"

[permissions]
allow = ["Read"]
deny  = ["Bash(rm -rf)"]

[mcpServers.corp_api]              # MCP-серверы можно и тут
type = "http"
url  = "https://gw.internal/mcp"
```

Эквивалент в JSON:
```json
{ "model": "internal-model", "permissionMode": "workspace-write",
  "permissions": { "allow": ["Read"], "deny": ["Bash(rm -rf)"] } }
```

## mcp.toml — MCP-серверы отдельным файлом

Каждая верхнеуровневая таблица = сервер (любой транспорт: `stdio`/`http`/`sse`/`ws`).

```toml
[corp_api]
type = "http"
url  = "https://gw.internal/mcp"

[local_tool]
type = "stdio"
command = "my-mcp"
args = ["--flag"]
```

Посмотреть, что подхватилось: `claw mcp list`.

## providers.toml — выбор провайдера и модели

`~/.claw/providers.toml` (вне git, права 600) задаёт, **каким провайдером и моделью**
думает агент. Отдельный от `settings.*` файл — там фичи, тут модель/ключи.

Слоты: `primary` (основная модель агента) и `auxiliary` (опц. — «более сильная»
модель для инструмента `consult_external_model`). У каждого слота ключ `type`
выбирает **протокол** провайдера (как `protocol` в qwen-code):

| `type` | Протокол | Endpoint |
|---|---|---|
| `anthropic` | нативный Anthropic | `/v1/messages` |
| `openai` | любой OpenAI-совместимый | `/v1/chat/completions` |

`openai` покрывает любой совместимый сервис (OpenRouter, qwen/DashScope, DeepSeek,
локальный Ollama/LM Studio/llama.cpp) — «кастомный» провайдер = `type = "openai"` +
свой `base_url`.

```toml
[primary]
type   = "openai"          # "openai" | "anthropic"  (алиас старого ключа: kind)
model  = "qwen/qwen3-coder"
base_url = "https://openrouter.ai/api/v1"
api_key  = "sk-or-…"        # пусто для anthropic → берётся OAuth/ANTHROPIC_API_KEY
max_tokens = 8192

[auxiliary]
type   = "openai"
model  = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"
api_key  = "sk-…"
```

Env-альтернатива: `OPENAI_API_KEY`/`OPENAI_BASE_URL`,
`ANTHROPIC_API_KEY`/`ANTHROPIC_BASE_URL`. Подробности и примеры — в **[GUIDE.md](GUIDE.md)** §2.

## externalConsult — консультация у внешней модели (опционально)

Основная (слабая) модель остаётся внутренней; когда она застревает, агент
**сам эскалирует** к более сильной внешней модели через инструмент
`consult_external_model`. Выключено, пока не задан блок и не выставлен ключ в env
(или пока не задан слот `[auxiliary]` в `providers.toml`).

```toml
[externalConsult]
enabled  = true
model    = "big-external-model"
baseUrl  = "https://external-gw.internal/v1"   # OpenAI-совместимый
apiKeyEnv = "EXT_LLM_KEY"                       # имя env с ключом (не сам ключ)
auditLog = ".claw/external-consult-audit.log"   # опционально
autoApprove = false                             # опц.: пропустить ручное ревью (см. ниже)
```

### Защита данных (для коммерческих / критичных проектов)

Наружу не должно уйти ничего чувствительного. Перед отправкой payload проходит
конвейер (fail-closed):

1. **Скан секретов/PII** — если найдены ключи/токены/пароли/private-key/JWT/
   `user:pass@…`/почты, отправка **жёстко блокируется**, модели возвращается
   требование переформулировать вопрос как абстрактный пример (сам секрет в лог
   не пишется — только замаскированный образец).
2. **Контракт «только пример»** — инструмент принимает `question` +
   `example` (минимальный самодостаточный **абстрактный** пример с плейсхолдерами,
   не реальный код); скил `consult` учит модель так эскалировать.
3. **Обезличивание** — оставшиеся имена типов/namespace → `T_1`/`N_1` (обратимо).
4. **Ревью** — точный payload показывается пользователю (y/N); без интерактивного
   терминала отправка не происходит (fail-closed). Пропустить ревью можно только
   явным opt-in `autoApprove = true` (для доверенного внутреннего endpoint) —
   **скан секретов (п.1) при этом всё равно работает и блокирует**; autoApprove
   снимает только ручное подтверждение, не защиту данных.
5. **Аудит** — каждое решение (`sent`/`sent-auto`/`declined`/`blocked-secrets`)
   пишется строкой JSON в `auditLog`.

Проверить состояние: `claw external status` (показывает, включён ли autoApprove).

## cozby-mcp — HTTP-сервисы как MCP-инструменты (контракты)

`cozby-mcp` — отдельный stdio MCP-сервер: read-only файловые инструменты +
инструменты из контрактов.

```
cozby-mcp [--root <dir>] [--brain-url <url>] [--contract <file.toml> ...]
```

| Флаг | Назначение | Env-fallback |
|---|---|---|
| `--root` | каталог для файловых инструментов (по умолч. cwd) | — |
| `--brain-url` | подключить встроенный контракт cozby-brain | `COZBY_BRAIN_URL` |
| `--contract` | свой TOML-контракт (повторяемо) | `COZBY_MCP_CONTRACTS` (через `:`) |

### Формат контракта (`weather.toml`)
```toml
name = "weather"
base_url = "https://api.weather.example"

[headers]                                    # опц.; ${env:VAR} резолвится при вызове
Authorization = "Bearer ${env:WEATHER_TOKEN}"

[[tools]]
name = "forecast"
description = "Get forecast for a city"
method = "GET"           # GET|POST|PUT|DELETE|PATCH
path = "/v1/forecast"    # поддерживает {param}
response = "data"        # точечный путь до полезной части (пусто = всё тело)

  [[tools.params]]
  name = "city"
  location = "query"     # path | query | body
  required = true        # по умолч. false
  type = "string"        # по умолч. string
  wire_name = "q"        # имя на проводе (по умолч. = name)
```

Подключить (в `mcp.toml` или `settings.json`):
```toml
[cozby]
type = "stdio"
command = "cozby-mcp"
args = ["--contract", "/path/weather.toml", "--brain-url", "http://localhost:8081"]
```

## Удобные команды

| Команда | Что делает |
|---|---|
| `claw mcp list` / `claw mcp show <srv>` | показать MCP-серверы |
| `claw brain on [url]` / `off` / `status` | вкл/выкл встроенный cozby-brain (пишет в user `settings.json`) |
| `claw external status` | статус внешней консультации |
