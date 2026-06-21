# cozby-claw Usage

📖 **Полное руководство пользователя: [GUIDE.md](GUIDE.md)**

cozby-claw — локальный кодинг-агент на Rust с двумя фронтендами:

- **`cozby-claw-cli`** — терминальный REPL (крейт `rusty-claude-cli`).
- **`cozby-claw-gui`** — десктоп-окно на egui (крейт `gui`).

## TL;DR

```bash
# ключ провайдера
mkdir -p ~/.claw && printf '[primary]\nkind="openai"\nmodel="qwen/qwen3-coder"\nbase_url="https://openrouter.ai/api/v1"\napi_key="sk-or-…"\n' > ~/.claw/providers.toml

# сборка + установка в ~/.local/bin
./release.sh

# запуск
cozby-claw-cli      # REPL
cozby-claw-gui      # GUI
```

Подробности — настройка провайдеров, флаги CLI, слэш-команды, GUI, режимы прав,
инструменты агента, фоновые под-агенты, MCP/hooks/plugins, сессии — в **[GUIDE.md](GUIDE.md)**.

Для разработчиков:

```bash
cd rust
cargo build --workspace
cargo test  --workspace
```
