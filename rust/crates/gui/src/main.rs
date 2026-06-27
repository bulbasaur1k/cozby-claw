//! cozby-claw-gui — нативный Slint fluid-GUI поверх агентного рантайма.
//!
//! Фаза 1: одна сессия — лента сообщений + ввод, fluid-оформление, фоновый ход
//! агента со стримингом. Модалки прав/вопросов, вкладки/сессии, вложения и темы —
//! следующие фазы.

mod protocol;
mod worker;

use std::rc::Rc;
use std::time::{Duration, Instant};

use runtime::{PermissionMode, Session};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use protocol::{Activity, AgentHandle, AgentToUi, UiToAgent};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = api::ProvidersConfig::load()
        .primary
        .map_or_else(|| "claude-sonnet-4-6".to_string(), |slot| slot.model);
    let mode = PermissionMode::WorkspaceWrite;

    let handle = worker::spawn_agent(model.clone(), mode, Session::new());
    let AgentHandle {
        to_agent,
        from_agent,
        permission_reply,
        question_reply,
        cancel: _cancel,
    } = handle;

    let ui = AppWindow::new()?;
    ui.set_model_name(model.into());
    ui.set_status("готов".into());
    if let Some(branch) = git_branch() {
        ui.set_branch(branch.into());
    }

    let messages = Rc::new(VecModel::<Message>::default());
    ui.set_messages(ModelRc::from(messages.clone()));

    // Отправка запроса: читаем ввод, чистим поле, добавляем пузырь, шлём воркеру.
    {
        let ui_weak = ui.as_weak();
        let messages = messages.clone();
        ui.on_send(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text = ui.get_input_text().to_string().trim().to_string();
            if text.is_empty() {
                return;
            }
            ui.set_input_text(SharedString::new());
            messages.push(Message {
                role: "user".into(),
                code: false,
                text: text.clone().into(),
            });
            ui.invoke_scroll_to_bottom();
            ui.set_busy(true);
            ui.set_status("думает…".into());
            let _ = to_agent.send(UiToAgent::Prompt(text));
        });
    }

    // Ответы модалок воркеру (он блокируется в recv до ответа пользователя).
    {
        let ui_weak = ui.as_weak();
        let deny_tx = permission_reply.clone();
        ui.on_perm_deny(move || {
            let _ = deny_tx.send(false);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_perm_open(false);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_perm_allow(move || {
            let _ = permission_reply.send(true);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_perm_open(false);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let skip_tx = question_reply.clone();
        ui.on_q_skip(move || {
            let _ = skip_tx.send(String::new());
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_q_open(false);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_q_choose(move |opt| {
            let _ = question_reply.send(opt.to_string());
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_q_open(false);
            }
        });
    }

    // Дренаж событий воркера в UI (на потоке событий Slint, ~30 мс).
    let timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let messages = messages.clone();
        let mut in_tok: u64 = 0;
        let mut out_tok: u64 = 0;
        // Текущий стрим ответа: (индекс строки, роль, накопленный текст).
        // Текст копится в Rust, а строка модели обновляется РАЗ В КАДР (анти-лаг).
        let mut stream: Option<(usize, &'static str, String)> = None;
        // Текущий процесс агента и момент его старта — для статуса «процесс · Ns».
        let mut activity_label = String::new();
        let mut turn_start: Option<Instant> = None;
        let mut last_status = String::new();
        timer.start(TimerMode::Repeated, Duration::from_millis(30), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut changed = false;
            while let Ok(event) = from_agent.try_recv() {
                changed = true;
                match event {
                    AgentToUi::Text(text) => accumulate(&messages, &mut stream, "assistant", &text),
                    AgentToUi::Thinking(text) => {
                        accumulate(&messages, &mut stream, "thinking", &text);
                    }
                    AgentToUi::Activity(activity) => {
                        match activity {
                            Activity::Idle => {
                                turn_start = None;
                                ui.set_busy(false);
                                ui.set_status("готов".into());
                                last_status.clear();
                            }
                            Activity::Model => {
                                activity_label = "думает".into();
                                turn_start.get_or_insert_with(Instant::now);
                                ui.set_busy(true);
                            }
                            Activity::Tool { label } => {
                                activity_label = label;
                                turn_start.get_or_insert_with(Instant::now);
                                ui.set_busy(true);
                            }
                            Activity::Waiting { label } => {
                                activity_label = format!("ждёт: {label}");
                                ui.set_busy(true);
                            }
                        }
                    }
                    AgentToUi::TurnDone => {
                        finalize_blocks(&messages, &mut stream);
                        turn_start = None;
                        ui.set_busy(false);
                        ui.set_status("готов".into());
                        last_status.clear();
                    }
                    AgentToUi::Error(error) => {
                        finalize_blocks(&messages, &mut stream);
                        push(&messages, "error", &format!("✘ {error}"));
                        turn_start = None;
                        ui.set_busy(false);
                        ui.set_status("ошибка".into());
                        last_status.clear();
                    }
                    other => {
                        // ToolCall / ToolResult / PermissionAsk / AskUser / Usage —
                        // закрывают текущий ответ и пишут свою строку / открывают модалку.
                        finalize_blocks(&messages, &mut stream);
                        apply_event(&ui, &messages, &mut in_tok, &mut out_tok, other);
                    }
                }
            }
            // Коалесцированное обновление строки стрима — один раз за кадр.
            flush_stream(&messages, &mut stream);
            // Живой статус «процесс · Ns» (как в Claude Code), пока агент работает.
            if let Some(start) = turn_start {
                let status = format!("{activity_label} · {}s", start.elapsed().as_secs());
                if status != last_status {
                    ui.set_status(status.clone().into());
                    last_status = status;
                }
            }
            if changed {
                ui.invoke_scroll_to_bottom();
            }
        });
    }

    ui.run()?;
    Ok(())
}

/// Применяет одно событие воркера к UI-состоянию. Запросы прав/вопросы открывают
/// модалку; ответ шлёт воркеру коллбэк модалки (см. `main`), а не эта функция.
fn apply_event(
    ui: &AppWindow,
    messages: &VecModel<Message>,
    in_tok: &mut u64,
    out_tok: &mut u64,
    event: AgentToUi,
) {
    match event {
        // Стримовые события обрабатываются в таймере (коалесцированно); сюда не доходят.
        AgentToUi::Text(_) | AgentToUi::Thinking(_) => {}
        AgentToUi::ToolCall { name, input } => push(
            messages,
            "tool",
            &format!("{name}   {}", first_line(&input, 160)),
        ),
        AgentToUi::ToolResult { output, is_error } => {
            let role = if is_error { "error" } else { "system" };
            push(messages, role, &format!("⎿ {}", first_line(&output, 200)));
        }
        AgentToUi::PermissionAsk {
            tool_name,
            input,
            reason,
        } => {
            ui.set_perm_tool(tool_name.into());
            ui.set_perm_input(first_line(&input, 240).into());
            ui.set_perm_reason(reason.unwrap_or_default().into());
            ui.set_perm_open(true);
        }
        AgentToUi::AskUser { question, options } => {
            ui.set_q_text(question.into());
            let opts: Vec<SharedString> = options.iter().map(|o| o.as_str().into()).collect();
            ui.set_q_options(ModelRc::from(Rc::new(VecModel::from(opts))));
            ui.set_q_open(true);
        }
        AgentToUi::Usage {
            input_tokens,
            output_tokens,
        } => {
            *in_tok += u64::from(input_tokens);
            *out_tok += u64::from(output_tokens);
            ui.set_usage(format!("{}k↑ {}k↓", *in_tok / 1000, *out_tok / 1000).into());
        }
        // Text/Thinking/Activity/TurnDone/Error обрабатываются в таймере.
        _ => {}
    }
}

/// Копит стримящийся текст в Rust-строке `stream` (без записи в модель на каждый
/// токен). При смене роли финализирует прежний ряд и открывает новый.
fn accumulate(
    messages: &VecModel<Message>,
    stream: &mut Option<(usize, &'static str, String)>,
    role: &'static str,
    text: &str,
) {
    match stream {
        Some((_, current_role, buf)) if *current_role == role => buf.push_str(text),
        _ => {
            // Смена роли: финализируем прежний ряд в markdown-блоки, открываем новый.
            finalize_blocks(messages, stream);
            push(messages, role, text);
            *stream = Some((messages.row_count() - 1, role, text.to_string()));
        }
    }
}

/// Записывает накопленный текст стрима в строку модели — вызывается раз в кадр
/// (и при финализации). Это и есть анти-лаг: один relayout на кадр, не на токен.
fn flush_stream(messages: &VecModel<Message>, stream: &mut Option<(usize, &'static str, String)>) {
    if let Some((idx, _, buf)) = stream {
        if let Some(mut row) = messages.row_data(*idx) {
            if row.text.as_str() != buf.as_str() {
                row.text = buf.as_str().into();
                messages.set_row_data(*idx, row);
            }
        }
    }
}

fn push(messages: &VecModel<Message>, role: &str, text: &str) {
    messages.push(Message {
        role: role.into(),
        code: false,
        text: text.into(),
    });
}

/// Разбивает текст на блоки по огороженным ``` ``` фрагментам.
/// Возвращает пары `(is_code, content)`; обычный текст и код чередуются.
fn to_blocks(text: &str) -> Vec<(bool, String)> {
    let mut blocks = Vec::new();
    let mut in_code = false;
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches('\n').trim_start().starts_with("```") {
            let trimmed = current.trim_end();
            if !trimmed.is_empty() {
                blocks.push((in_code, trimmed.to_string()));
            }
            current.clear();
            in_code = !in_code;
            continue;
        }
        current.push_str(line);
    }
    let trimmed = current.trim_end();
    if !trimmed.is_empty() {
        blocks.push((in_code, trimmed.to_string()));
    }
    blocks
}

/// Завершает стрим: разбивает накопленный текст на markdown-блоки. Если блок один
/// и не код — оставляет строку как есть; иначе заменяет её на ряд блоков (стримовый
/// ряд всегда последний, поэтому remove+push безопасны). Сбрасывает `stream` в None.
fn finalize_blocks(messages: &VecModel<Message>, stream: &mut Option<(usize, &'static str, String)>) {
    let Some((idx, role, buf)) = stream.take() else {
        return;
    };
    let blocks = to_blocks(&buf);
    let single_plain = blocks.len() <= 1 && !blocks.first().is_some_and(|(code, _)| *code);
    if single_plain {
        if let Some(mut row) = messages.row_data(idx) {
            if row.text.as_str() != buf {
                row.text = buf.as_str().into();
                messages.set_row_data(idx, row);
            }
        }
        return;
    }
    messages.remove(idx);
    for (code, text) in blocks {
        messages.push(Message {
            role: role.into(),
            code,
            text: text.into(),
        });
    }
}

/// Текущая git-ветка для шапки (или `None`, если вне репозитория).
fn git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

/// Первая непустая строка, обрезанная до `max` символов.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let truncated: String = line.chars().take(max).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::{accumulate, finalize_blocks, first_line, flush_stream, to_blocks, Message};
    use slint::{Model, VecModel};

    #[test]
    fn to_blocks_splits_code_fences() {
        let blocks = to_blocks("привет\n```\nlet x = 1;\n```\nпока");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], (false, "привет".to_string()));
        assert_eq!(blocks[1], (true, "let x = 1;".to_string()));
        assert_eq!(blocks[2], (false, "пока".to_string()));
    }

    #[test]
    fn finalize_splits_streamed_code_into_block_rows() {
        let model = VecModel::<Message>::default();
        let mut stream = None;
        for delta in ["Вот код:\n", "```\n", "fn main() {}\n", "```\n", "готово"] {
            accumulate(&model, &mut stream, "assistant", delta);
        }
        finalize_blocks(&model, &mut stream);
        assert_eq!(model.row_count(), 3, "текст / код / текст");
        assert!(!model.row_data(0).unwrap().code);
        assert!(model.row_data(1).unwrap().code);
        assert_eq!(model.row_data(1).unwrap().text.as_str(), "fn main() {}");
        assert!(!model.row_data(2).unwrap().code);
        assert!(stream.is_none(), "finalize сбрасывает stream");
    }

    #[test]
    fn streaming_coalesces_deltas_into_one_row() {
        let model = VecModel::<Message>::default();
        let mut stream = None;
        accumulate(&model, &mut stream, "assistant", "Hel");
        accumulate(&model, &mut stream, "assistant", "lo, ");
        accumulate(&model, &mut stream, "assistant", "world");
        flush_stream(&model, &mut stream);
        assert_eq!(model.row_count(), 1, "все дельты одной роли — в одной строке");
        let row = model.row_data(0).unwrap();
        assert_eq!(row.role.as_str(), "assistant");
        assert_eq!(row.text.as_str(), "Hello, world");
    }

    #[test]
    fn role_change_starts_a_new_row_and_finalizes_previous() {
        let model = VecModel::<Message>::default();
        let mut stream = None;
        accumulate(&model, &mut stream, "assistant", "ответ");
        accumulate(&model, &mut stream, "thinking", "рассуждение");
        flush_stream(&model, &mut stream);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.row_data(0).unwrap().text.as_str(), "ответ");
        assert_eq!(model.row_data(1).unwrap().role.as_str(), "thinking");
        assert_eq!(model.row_data(1).unwrap().text.as_str(), "рассуждение");
    }

    #[test]
    fn flush_is_idempotent_without_new_text() {
        let model = VecModel::<Message>::default();
        let mut stream = None;
        accumulate(&model, &mut stream, "assistant", "x");
        flush_stream(&model, &mut stream);
        flush_stream(&model, &mut stream);
        assert_eq!(model.row_count(), 1);
        assert_eq!(model.row_data(0).unwrap().text.as_str(), "x");
    }

    #[test]
    fn first_line_takes_first_nonblank_and_truncates() {
        assert_eq!(first_line("\n\n  привет \nмир", 40), "привет");
        let long = "x".repeat(50);
        let cut = first_line(&long, 10);
        assert_eq!(cut.chars().count(), 11, "10 символов + …");
        assert!(cut.ends_with('…'));
    }
}
