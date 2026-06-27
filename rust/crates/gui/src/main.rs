//! cozby-claw-gui — нативный Slint fluid-GUI поверх агентного рантайма.
//!
//! Мультиплексер: несколько сессий-вкладок, каждая со своим воркером, лентой и
//! статусом; фоновые сессии продолжают выполняться. Стриминг коалесцирован,
//! markdown-блоки кода, модалки прав/вопросов, прерывание хода (Esc/стоп).

mod protocol;
mod worker;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use runtime::{ContentBlock, MessageRole, PermissionMode, Session};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use protocol::{Activity, AgentHandle, AgentToUi, UiToAgent};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = api::ProvidersConfig::load()
        .primary
        .map_or_else(|| "claude-sonnet-4-6".to_string(), |slot| slot.model);
    let mode = PermissionMode::WorkspaceWrite;

    let ui = AppWindow::new()?;
    ui.set_model_name(model.clone().into());
    ui.set_status("готов".into());
    if let Some(branch) = git_branch() {
        ui.set_branch(branch.into());
    }

    let state = Rc::new(RefCell::new(AppState::new(model, mode)));
    apply_active(&ui, &state.borrow());

    // Отправка запроса в активную сессию.
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_send(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text = ui.get_input_text().to_string().trim().to_string();
            if text.is_empty() {
                return;
            }
            ui.set_input_text(SharedString::new());
            let mut st = state.borrow_mut();
            let active = st.active;
            // Первое сообщение задаёт осмысленный заголовок вкладки.
            let first = st.sessions[active].messages.row_count() == 0;
            if first {
                st.sessions[active].title = first_line(&text, 24);
            }
            {
                let tab = &st.sessions[active];
                tab.messages.push(Message {
                    role: "user".into(),
                    code: false,
                    text: text.clone().into(),
                });
                let _ = tab.handle.to_agent.send(UiToAgent::Prompt(text));
            }
            if first {
                set_tabs(&ui, &st);
            }
            ui.set_busy(true);
            ui.set_status("думает…".into());
            ui.invoke_scroll_to_bottom();
        });
    }

    // Прерывание активной сессии (Esc / стоп).
    {
        let state = state.clone();
        ui.on_interrupt(move || {
            let st = state.borrow();
            st.sessions[st.active]
                .handle
                .cancel
                .store(true, Ordering::SeqCst);
        });
    }

    // Новая вкладка-сессия.
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_new_tab(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let model = st.model.clone();
                let mode = st.mode;
                let id = st.next_id();
                st.sessions.push(SessionTab::new(model, mode, id));
                st.active = st.sessions.len() - 1;
            }
            apply_active(&ui, &state.borrow());
        });
    }

    // Переключение вкладки.
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_select_tab(move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let idx = usize::try_from(idx).unwrap_or(0);
                if idx < st.sessions.len() {
                    st.active = idx;
                }
            }
            apply_active(&ui, &state.borrow());
        });
    }

    // Закрытие вкладки (последняя остаётся; drop сессии завершает её воркер).
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_close_tab(move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let idx = usize::try_from(idx).unwrap_or(0);
                if st.sessions.len() > 1 && idx < st.sessions.len() {
                    st.sessions.remove(idx);
                    if st.active >= st.sessions.len() {
                        st.active = st.sessions.len() - 1;
                    } else if idx < st.active {
                        st.active -= 1;
                    }
                }
            }
            apply_active(&ui, &state.borrow());
        });
    }

    // Закрыть активную вкладку (⌘/Ctrl+W).
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_close_current(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let active = st.active;
                if st.sessions.len() > 1 {
                    st.sessions.remove(active);
                    if st.active >= st.sessions.len() {
                        st.active = st.sessions.len() - 1;
                    }
                }
            }
            apply_active(&ui, &state.borrow());
        });
    }

    // Ответы модалок — той сессии, что их запросила.
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_perm_allow(move || reply_perm(&state, &ui_weak, true));
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_perm_deny(move || reply_perm(&state, &ui_weak, false));
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_q_skip(move || reply_question(&state, &ui_weak, String::new()));
    }
    {
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_q_choose(move |opt| reply_question(&state, &ui_weak, opt.to_string()));
    }

    // Дренаж событий ВСЕХ сессий (фоновые продолжают идти), ~30 мс.
    let timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(30), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let active = st.active;
            let mut tabs_dirty = false;
            let mut active_changed = false;
            let mut modal_for: Option<usize> = None;
            for idx in 0..st.sessions.len() {
                let was_busy = st.sessions[idx].busy;
                let (changed, opened) = drain_session(&ui, &mut st.sessions[idx], idx == active);
                if changed && idx == active {
                    active_changed = true;
                }
                if st.sessions[idx].busy != was_busy {
                    tabs_dirty = true;
                }
                if opened {
                    modal_for = Some(idx);
                }
            }
            // Живой статус активной сессии «процесс · Ns».
            if let Some(start) = st.sessions[active].turn_start {
                let label = st.sessions[active].activity_label.clone();
                let status = format!("{label} · {}s", start.elapsed().as_secs());
                if status != st.sessions[active].last_status {
                    ui.set_status(status.clone().into());
                    st.sessions[active].last_status = status;
                }
            }
            // Модалку запросила сессия `i` — она становится активной.
            if let Some(i) = modal_for {
                st.modal_session = Some(i);
                if i != active {
                    st.active = i;
                    drop(st);
                    apply_active(&ui, &state.borrow());
                    return;
                }
            }
            if tabs_dirty {
                set_tabs(&ui, &st);
            }
            if active_changed {
                ui.invoke_scroll_to_bottom();
            }
        });
    }

    ui.run()?;
    Ok(())
}

/// Одна сессия-вкладка: свой воркер, своя лента, своё состояние стрима/статуса.
struct SessionTab {
    handle: AgentHandle,
    messages: Rc<VecModel<Message>>,
    title: String,
    stream: Option<(usize, &'static str, String)>,
    in_tok: u64,
    out_tok: u64,
    activity_label: String,
    turn_start: Option<Instant>,
    last_status: String,
    busy: bool,
}

impl SessionTab {
    fn new(model: String, mode: PermissionMode, id: usize) -> Self {
        Self::from_session(model, mode, Session::new(), Some(format!("Сессия {id}")))
    }

    /// Строит вкладку из (новой или загруженной) сессии: путь сохранения по
    /// её id, заголовок из первого сообщения, восстановленная лента.
    fn from_session(
        model: String,
        mode: PermissionMode,
        session: Session,
        default_title: Option<String>,
    ) -> Self {
        let save_path = sessions_dir().map(|dir| dir.join(format!("{}.jsonl", session.session_id)));
        let title = title_from_session(&session)
            .or(default_title)
            .unwrap_or_else(|| "Сессия".to_string());
        let messages = Rc::new(VecModel::from(rebuild_messages(&session)));
        Self {
            handle: worker::spawn_agent(model, mode, session, save_path),
            messages,
            title,
            stream: None,
            in_tok: 0,
            out_tok: 0,
            activity_label: String::new(),
            turn_start: None,
            last_status: String::new(),
            busy: false,
        }
    }
}

/// Состояние мультиплексера: набор сессий + активная + конфиг новых сессий.
struct AppState {
    sessions: Vec<SessionTab>,
    active: usize,
    model: String,
    mode: PermissionMode,
    next_id: usize,
    /// Сессия, чью модалку прав/вопроса сейчас показывает UI.
    modal_session: Option<usize>,
}

impl AppState {
    fn new(model: String, mode: PermissionMode) -> Self {
        let mut sessions = load_recent_sessions(&model, mode);
        if sessions.is_empty() {
            sessions.push(SessionTab::new(model.clone(), mode, 1));
        }
        let next_id = sessions.len() + 1;
        Self {
            sessions,
            active: 0,
            model,
            mode,
            next_id,
            modal_session: None,
        }
    }

    fn next_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Каталог сессий проекта (`<cwd>/.claw/sessions`), создаёт при отсутствии.
fn sessions_dir() -> Option<PathBuf> {
    let dir = std::env::current_dir().ok()?.join(".claw").join("sessions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Загружает до 6 последних непустых сессий (новые — первыми) как вкладки.
fn load_recent_sessions(model: &str, mode: PermissionMode) -> Vec<SessionTab> {
    let Some(dir) = sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((mtime, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .filter_map(|(_, path)| Session::load_from_path(&path).ok())
        .filter(|session| !session.messages.is_empty())
        .take(6)
        .map(|session| SessionTab::from_session(model.to_string(), mode, session, None))
        .collect()
}

/// Восстанавливает строки ленты из истории сессии.
fn rebuild_messages(session: &Session) -> Vec<Message> {
    let mut rows = Vec::new();
    for message in &session.messages {
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    match message.role {
                        MessageRole::User => rows.push(mk_row("user", false, text)),
                        MessageRole::Assistant => {
                            for (code, part) in to_blocks(text) {
                                rows.push(mk_row("assistant", code, &part));
                            }
                        }
                        MessageRole::System | MessageRole::Tool => {}
                    }
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    rows.push(mk_row(
                        "tool",
                        false,
                        &format!("{name}   {}", first_line(input, 160)),
                    ));
                }
                ContentBlock::ToolResult {
                    output, is_error, ..
                } => {
                    let role = if *is_error { "error" } else { "system" };
                    rows.push(mk_row(role, false, &format!("⎿ {}", first_line(output, 200))));
                }
            }
        }
    }
    rows
}

fn mk_row(role: &str, code: bool, text: &str) -> Message {
    Message {
        role: role.into(),
        code,
        text: text.into(),
    }
}

/// Заголовок вкладки из первого пользовательского сообщения сессии.
fn title_from_session(session: &Session) -> Option<String> {
    session.messages.iter().find_map(|message| {
        if message.role != MessageRole::User {
            return None;
        }
        message.blocks.iter().find_map(|block| match block {
            ContentBlock::Text { text } if !text.trim().is_empty() => Some(first_line(text, 24)),
            _ => None,
        })
    })
}

/// Привязывает к UI активную сессию (лента, статус, токены) и таб-бар.
fn apply_active(ui: &AppWindow, st: &AppState) {
    let tab = &st.sessions[st.active];
    ui.set_messages(ModelRc::from(tab.messages.clone()));
    ui.set_busy(tab.busy);
    ui.set_usage(format!("{}k↑ {}k↓", tab.in_tok / 1000, tab.out_tok / 1000).into());
    ui.set_status(
        if tab.busy {
            tab.last_status.clone()
        } else {
            "готов".to_string()
        }
        .into(),
    );
    set_tabs(ui, st);
    ui.invoke_scroll_to_bottom();
}

/// Перестраивает модель таб-бара из текущих сессий.
fn set_tabs(ui: &AppWindow, st: &AppState) {
    let infos: Vec<TabInfo> = st
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| TabInfo {
            title: s.title.as_str().into(),
            busy: s.busy,
            active: i == st.active,
        })
        .collect();
    ui.set_tabs(ModelRc::from(Rc::new(VecModel::from(infos))));
}

fn reply_perm(state: &Rc<RefCell<AppState>>, ui_weak: &Weak<AppWindow>, allow: bool) {
    let mut st = state.borrow_mut();
    if let Some(i) = st.modal_session.take() {
        if let Some(tab) = st.sessions.get(i) {
            let _ = tab.handle.permission_reply.send(allow);
        }
    }
    drop(st);
    if let Some(ui) = ui_weak.upgrade() {
        ui.set_perm_open(false);
    }
}

fn reply_question(state: &Rc<RefCell<AppState>>, ui_weak: &Weak<AppWindow>, answer: String) {
    let mut st = state.borrow_mut();
    if let Some(i) = st.modal_session.take() {
        if let Some(tab) = st.sessions.get(i) {
            let _ = tab.handle.question_reply.send(answer);
        }
    }
    drop(st);
    if let Some(ui) = ui_weak.upgrade() {
        ui.set_q_open(false);
    }
}

/// Применяет события одной сессии к её ленте/состоянию. Для активной сессии также
/// обновляет busy/usage в UI. Возвращает `(были_события, открыта_модалка)`.
fn drain_session(ui: &AppWindow, tab: &mut SessionTab, active: bool) -> (bool, bool) {
    let mut changed = false;
    let mut opened_modal = false;
    while let Ok(event) = tab.handle.from_agent.try_recv() {
        changed = true;
        match event {
            AgentToUi::Text(text) => accumulate(&tab.messages, &mut tab.stream, "assistant", &text),
            AgentToUi::Thinking(text) => {
                accumulate(&tab.messages, &mut tab.stream, "thinking", &text);
            }
            AgentToUi::Activity(activity) => match activity {
                Activity::Idle => {
                    tab.turn_start = None;
                    tab.busy = false;
                    tab.last_status.clear();
                    if active {
                        ui.set_busy(false);
                        ui.set_status("готов".into());
                    }
                }
                Activity::Model => {
                    tab.activity_label = "думает".into();
                    tab.turn_start.get_or_insert_with(Instant::now);
                    tab.busy = true;
                    if active {
                        ui.set_busy(true);
                    }
                }
                Activity::Tool { label } => {
                    tab.activity_label = label;
                    tab.turn_start.get_or_insert_with(Instant::now);
                    tab.busy = true;
                    if active {
                        ui.set_busy(true);
                    }
                }
                Activity::Waiting { label } => {
                    tab.activity_label = format!("ждёт: {label}");
                    tab.busy = true;
                    if active {
                        ui.set_busy(true);
                    }
                }
            },
            AgentToUi::TurnDone => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                tab.turn_start = None;
                tab.busy = false;
                tab.last_status.clear();
                if active {
                    ui.set_busy(false);
                    ui.set_status("готов".into());
                }
            }
            AgentToUi::Error(error) => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                push(&tab.messages, "error", &format!("✘ {error}"));
                tab.turn_start = None;
                tab.busy = false;
                tab.last_status.clear();
                if active {
                    ui.set_busy(false);
                    ui.set_status("ошибка".into());
                }
            }
            AgentToUi::ToolCall { name, input } => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                push(
                    &tab.messages,
                    "tool",
                    &format!("{name}   {}", first_line(&input, 160)),
                );
            }
            AgentToUi::ToolResult { output, is_error } => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                let role = if is_error { "error" } else { "system" };
                push(&tab.messages, role, &format!("⎿ {}", first_line(&output, 200)));
            }
            AgentToUi::Usage {
                input_tokens,
                output_tokens,
            } => {
                tab.in_tok += u64::from(input_tokens);
                tab.out_tok += u64::from(output_tokens);
                if active {
                    ui.set_usage(
                        format!("{}k↑ {}k↓", tab.in_tok / 1000, tab.out_tok / 1000).into(),
                    );
                }
            }
            AgentToUi::PermissionAsk {
                tool_name,
                input,
                reason,
            } => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                ui.set_perm_tool(tool_name.into());
                ui.set_perm_input(first_line(&input, 240).into());
                ui.set_perm_reason(reason.unwrap_or_default().into());
                ui.set_perm_open(true);
                opened_modal = true;
            }
            AgentToUi::AskUser { question, options } => {
                finalize_blocks(&tab.messages, &mut tab.stream);
                ui.set_q_text(question.into());
                let opts: Vec<SharedString> = options.iter().map(|o| o.as_str().into()).collect();
                ui.set_q_options(ModelRc::from(Rc::new(VecModel::from(opts))));
                ui.set_q_open(true);
                opened_modal = true;
            }
        }
    }
    flush_stream(&tab.messages, &mut tab.stream);
    (changed, opened_modal)
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
