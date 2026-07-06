//! Полноэкранное интерактивное приложение (ratatui) — основной интерфейс
//! `cozby-claw-cli`: вкладки-сессии (параллельные агенты в процессе), история со
//! стримингом, многострочный ввод с автодополнением, футер, подтверждения, темы.
//! Мышь НЕ захватывается — нативное выделение/копирование терминала работает.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use pulldown_cmark::{
    CodeBlockKind, Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use runtime::{ContentBlock, MessageRole, PermissionMode, Session};
use tui_textarea::{CursorMove, TextArea};

use super::highlight;
use super::icons;
use super::protocol::{Activity, AgentHandle, AgentToUi, UiToAgent};
use super::worker;

const COMMANDS: &[&str] = &[
    "/commit", "/review", "/pr", "/security-review", "/init", "/model", "/plan", "/permissions",
    "/compact", "/rewind", "/tasks", "/rename", "/session", "/mcp", "/skills", "/agents",
    "/external", "/brain", "/hooks", "/sandbox", "/memory", "/diff", "/config", "/version",
    "/keymap", "/theme", "/clear", "/help", "/quit",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    Thinking,
    Tool,
    ToolResult,
    Error,
    System,
    /// Раскрашенный unified-diff (правки Edit/Write) — рендерится моно-панелью.
    Diff,
}

struct Entry {
    role: Role,
    text: String,
}

#[derive(Clone, Copy)]
struct Theme {
    name: &'static str,
    text: Color,
    muted: Color,
    accent: Color,
    accent2: Color,
    working: Color,
    success: Color,
    error: Color,
    bar_bg: Color,
}

const THEMES: &[Theme] = &[
    Theme {
        name: "mocha",
        text: Color::Rgb(205, 214, 244),
        muted: Color::Rgb(127, 132, 156),
        accent: Color::Rgb(203, 166, 247),
        accent2: Color::Rgb(137, 220, 235),
        working: Color::Rgb(249, 226, 175),
        success: Color::Rgb(166, 227, 161),
        error: Color::Rgb(243, 139, 168),
        bar_bg: Color::Rgb(24, 24, 37),
    },
    Theme {
        name: "tokyo",
        text: Color::Rgb(192, 202, 245),
        muted: Color::Rgb(86, 95, 137),
        accent: Color::Rgb(122, 162, 247),
        accent2: Color::Rgb(125, 207, 255),
        working: Color::Rgb(224, 175, 104),
        success: Color::Rgb(158, 206, 106),
        error: Color::Rgb(247, 118, 142),
        bar_bg: Color::Rgb(22, 23, 33),
    },
    Theme {
        name: "gruvbox",
        text: Color::Rgb(235, 219, 178),
        muted: Color::Rgb(146, 131, 116),
        accent: Color::Rgb(250, 189, 47),
        accent2: Color::Rgb(131, 165, 152),
        working: Color::Rgb(254, 128, 25),
        success: Color::Rgb(184, 187, 38),
        error: Color::Rgb(251, 73, 52),
        bar_bg: Color::Rgb(34, 34, 34),
    },
];

enum Modal {
    None,
    Permission {
        tool: String,
        input: String,
        reason: Option<String>,
    },
    Question {
        question: String,
        options: Vec<String>,
    },
}

struct Completion {
    start: usize,
    row: usize,
    items: Vec<String>,
    selected: usize,
}

/// Одна вкладка — независимая сессия-агент (свой воркер и состояние).
struct Tab {
    id: usize,
    title: String,
    handle: AgentHandle,
    entries: Vec<Entry>,
    activity: Activity,
    running: bool,
    modal: Modal,
    answer: String,
    in_tok: u64,
    out_tok: u64,
    turn_start: Option<Instant>,
    scroll_back: u16,
    pinned: bool,
    save_path: Option<PathBuf>,
    model: String,
    mode: PermissionMode,
}

impl Tab {
    fn new(id: usize, model: &str, mode: PermissionMode) -> Self {
        Self::from_session(id, model, mode, Session::new(), None)
    }

    /// Строит вкладку из (новой или загруженной) сессии: восстанавливает историю
    /// и заголовок, продолжает ту же сессию в воркере, сохраняет в тот же файл.
    fn from_session(
        id: usize,
        model: &str,
        mode: PermissionMode,
        session: Session,
        fallback_title: Option<String>,
    ) -> Self {
        let save_path = sessions_path(&session.session_id);
        let title = title_from_session(&session)
            .or(fallback_title)
            .unwrap_or_else(|| format!("Сессия {id}"));
        let entries = rebuild_entries(&session);
        let handle = worker::spawn_agent(model.to_string(), mode, session, save_path.clone());
        Self {
            id,
            title,
            handle,
            entries,
            activity: Activity::Idle,
            running: false,
            modal: Modal::None,
            answer: String::new(),
            in_tok: 0,
            out_tok: 0,
            turn_start: None,
            scroll_back: 0,
            pinned: false,
            save_path,
            model: model.to_string(),
            mode,
        }
    }

    fn push(&mut self, role: Role, text: String) {
        self.entries.push(Entry { role, text });
    }

    fn append_stream(&mut self, role: Role, text: &str) {
        if let Some(last) = self.entries.last_mut() {
            if last.role == role {
                last.text.push_str(text);
                return;
            }
        }
        self.push(role, text.to_string());
    }

    /// Применяет одно событие воркера к состоянию вкладки.
    fn apply(&mut self, event: AgentToUi) {
        match event {
            AgentToUi::Text(text) => self.append_stream(Role::Assistant, &text),
            AgentToUi::Thinking(text) => self.append_stream(Role::Thinking, &text),
            AgentToUi::ToolCall { name, input } => {
                if let Some(diff) = build_edit_diff(&name, &input) {
                    let path = edit_path(&input).unwrap_or_default();
                    self.push(Role::Tool, format!("{name}  {path}"));
                    self.push(Role::Diff, diff);
                } else {
                    self.push(Role::Tool, format!("{name}  {}", first_line(&input, 200)));
                }
            }
            AgentToUi::ToolResult { output, is_error } => self.push(
                if is_error { Role::Error } else { Role::ToolResult },
                format!("⎿ {}", first_line(&output, 400)),
            ),
            AgentToUi::PermissionAsk {
                tool_name,
                input,
                reason,
            } => {
                self.modal = Modal::Permission {
                    tool: tool_name,
                    input,
                    reason,
                };
            }
            AgentToUi::AskUser { question, options } => {
                self.modal = Modal::Question { question, options };
            }
            AgentToUi::Usage {
                input_tokens,
                output_tokens,
            } => {
                self.in_tok += u64::from(input_tokens);
                self.out_tok += u64::from(output_tokens);
            }
            AgentToUi::Activity(activity) => self.activity = activity,
            AgentToUi::TurnDone => {
                self.running = false;
                self.activity = Activity::Idle;
                self.turn_start = None;
                self.handle.cancel.store(false, Ordering::SeqCst);
            }
            AgentToUi::Error(error) => {
                self.push(Role::Error, error);
                self.running = false;
                self.activity = Activity::Idle;
                self.turn_start = None;
                self.handle.cancel.store(false, Ordering::SeqCst);
            }
        }
    }

}

struct App {
    theme: Theme,
    model: String,
    mode: PermissionMode,
    cwd: String,
    branch: Option<String>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    input: TextArea<'static>,
    compl: Option<Completion>,
    frame_tick: usize,
    sidebar_collapsed: bool,
    should_quit: bool,
    /// Кэш подсвеченных блоков кода: hash(lang, code, width) → строки.
    hl_cache: RefCell<HashMap<u64, Vec<Line<'static>>>>,
    /// Текущий vim-режим ввода (модальный ввод — всегда включён).
    vim_mode: VimMode,
    /// Ожидающий оператор vim (`d`/`c`/`g`) для двухклавишных команд (dd, dw, gg, gt).
    vim_pending: Option<char>,
    /// Куда направлен фокус клавиатуры: основная область или меню сессий.
    focus: Focus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimMode {
    Normal,
    Insert,
}

/// Фокус клавиатуры: ввод/история или список сессий (сайдбар).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Main,
    Sidebar,
}

/// Запускает приложение.
///
/// # Errors
/// Ошибки терминала/ввода-вывода.
pub fn run(model: String, mode: PermissionMode) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = ratatui::init();
    // Mouse capture — чтобы ловить прокрутку колесом/тачпадом. Побочный эффект:
    // нативное выделение мышью в терминале требует Shift (стандартно для TUI).
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    let mut app = App::new(model, mode);
    let result = app.event_loop(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

fn sessions_path(id: &str) -> Option<PathBuf> {
    let dir = std::env::current_dir().ok()?.join(".claw").join("sessions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{id}.jsonl")))
}

impl App {
    fn new(model: String, mode: PermissionMode) -> Self {
        let cwd = std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |path| path.display().to_string());
        let theme = THEMES[0];
        let mut input = TextArea::default();
        configure_input(&mut input, theme);
        let mut tabs = load_recent_sessions(&model, mode);
        if tabs.is_empty() {
            tabs.push(Tab::new(1, &model, mode));
        }
        let next_id = tabs.len() + 1;
        let mut app = Self {
            theme,
            model,
            mode,
            cwd,
            branch: git_branch(),
            tabs,
            active: 0,
            next_id,
            input,
            compl: None,
            frame_tick: 0,
            sidebar_collapsed: false,
            should_quit: false,
            hl_cache: RefCell::new(HashMap::new()),
            vim_mode: VimMode::Insert,
            vim_pending: None,
            focus: Focus::Main,
        };
        if app.active().entries.is_empty() {
            app.active_mut().push(Role::System, keymap_text());
        }
        app
    }

    fn active(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn active_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn event_loop(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut dirty = true;
        while !self.should_quit {
            if dirty {
                terminal.draw(|frame| self.draw(frame))?;
                dirty = false;
            }
            if self.drain_worker() {
                dirty = true;
            }
            if self.drain_agent_notices() {
                dirty = true;
            }
            let busy = self.tabs.iter().any(|tab| tab.running);
            let timeout = if busy {
                Duration::from_millis(120)
            } else {
                Duration::from_millis(250)
            };
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
                    Event::Paste(text) => {
                        self.input.insert_str(&text);
                        self.recompute_completion();
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => self.scroll_history(3),
                        MouseEventKind::ScrollDown => self.scroll_history(-3),
                        // move/drag/click событий много — не перерисовываемся зря
                        _ => continue,
                    },
                    Event::Resize(_, _) => {}
                    _ => continue,
                }
                dirty = true;
            } else if busy {
                self.frame_tick = self.frame_tick.wrapping_add(1);
                dirty = true;
            }
        }
        Ok(())
    }

    /// Показывает завершившиеся фоновые под-агенты (`Agent` tool) системными
    /// строками активной вкладки. Возвращает `true`, если что-то добавлено.
    fn drain_agent_notices(&mut self) -> bool {
        let notices = tools::drain_agent_notifications();
        if notices.is_empty() {
            return false;
        }
        for notice in notices {
            self.active_mut().push(
                Role::System,
                format!(
                    "agent {} ({}) → {}",
                    notice.name, notice.agent_id, notice.status
                ),
            );
        }
        true
    }

    /// Дренирует воркеры ВСЕХ вкладок (фоновые продолжают идти). Возвращает
    /// `true`, если активная вкладка что-то получила (нужна перерисовка).
    fn drain_worker(&mut self) -> bool {
        let active = self.active;
        let mut changed = false;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            while let Ok(event) = tab.handle.from_agent.try_recv() {
                if index == active {
                    changed = true;
                }
                tab.apply(event);
            }
        }
        changed
    }

    /// Прокручивает историю активной вкладки: положительный `delta` — вверх
    /// (в прошлое), отрицательный — вниз (к свежим сообщениям). Ограничение по
    /// краям делает [`Self::draw_history`].
    fn scroll_history(&mut self, delta: i16) {
        let s = &mut self.active_mut().scroll_back;
        *s = if delta >= 0 {
            s.saturating_add(delta.unsigned_abs())
        } else {
            s.saturating_sub(delta.unsigned_abs())
        };
    }

    // --- клавиши ------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        if !matches!(self.active().modal, Modal::None) {
            self.on_modal_key(key);
            return;
        }
        if self.focus == Focus::Sidebar {
            self.handle_sidebar_key(key);
            return;
        }
        if self.compl.is_some() {
            match key.code {
                KeyCode::Up => {
                    self.compl_move(-1);
                    return;
                }
                KeyCode::Down => {
                    self.compl_move(1);
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_completion();
                    return;
                }
                KeyCode::Esc => {
                    self.compl = None;
                    return;
                }
                _ => {}
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('n') if ctrl => self.new_tab(),
            KeyCode::Char('w') if ctrl => self.close_tab(),
            KeyCode::Char('p') if ctrl => self.toggle_pin(),
            KeyCode::Char('e') if ctrl => self.sidebar_collapsed = !self.sidebar_collapsed,
            KeyCode::Char('f') if ctrl => self.switch(1),
            KeyCode::Char('b') if ctrl => self.switch(-1),
            KeyCode::Right if ctrl => self.switch(1),
            KeyCode::Left if ctrl => self.switch(-1),
            KeyCode::Char(c @ '1'..='9') if alt => {
                let index = (c as usize) - ('1' as usize);
                if index < self.tabs.len() {
                    self.active = index;
                }
            }
            KeyCode::Char('c') if ctrl => {
                if self.active().running {
                    self.cancel_turn();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('d') if ctrl => self.should_quit = true,
            KeyCode::Char('l') if ctrl => {
                self.active_mut().entries.clear();
                self.active_mut().scroll_back = 0;
            }
            KeyCode::Char('t') if ctrl => self.theme = next_theme(self.theme),
            KeyCode::Esc => {
                if self.vim_mode == VimMode::Insert {
                    self.vim_mode = VimMode::Normal;
                    self.vim_pending = None;
                } else if self.active().running {
                    self.cancel_turn();
                } else {
                    self.vim_pending = None;
                    self.reset_input();
                }
            }
            KeyCode::Up if shift => self.scroll_history(3),
            KeyCode::Down if shift => self.scroll_history(-3),
            KeyCode::PageUp => self.scroll_history(8),
            KeyCode::PageDown => self.scroll_history(-8),
            KeyCode::Enter if alt || shift => self.input.insert_newline(),
            KeyCode::Enter => self.submit(),
            _ => {
                if self.vim_mode == VimMode::Normal {
                    self.handle_vim_normal(key);
                } else {
                    self.input.input(key);
                }
            }
        }
        self.recompute_completion();
    }

    fn on_modal_key(&mut self, key: KeyEvent) {
        let reply_perm = self.active().handle.permission_reply.clone();
        let reply_question = self.active().handle.question_reply.clone();
        match &mut self.tabs[self.active].modal {
            Modal::Permission { .. } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    let _ = reply_perm.send(true);
                    self.active_mut().modal = Modal::None;
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    let _ = reply_perm.send(false);
                    self.active_mut().modal = Modal::None;
                }
                _ => {}
            },
            Modal::Question { options, .. } => match key.code {
                KeyCode::Char(c @ '1'..='9') => {
                    let index = (c as usize) - ('1' as usize);
                    if let Some(option) = options.get(index).cloned() {
                        let _ = reply_question.send(option);
                        let tab = self.active_mut();
                        tab.modal = Modal::None;
                        tab.answer.clear();
                    }
                }
                KeyCode::Esc => {
                    let _ = reply_question.send(String::new());
                    let tab = self.active_mut();
                    tab.modal = Modal::None;
                    tab.answer.clear();
                }
                KeyCode::Enter => {
                    let answer = self.active().answer.clone();
                    let _ = reply_question.send(answer);
                    let tab = self.active_mut();
                    tab.modal = Modal::None;
                    tab.answer.clear();
                }
                KeyCode::Backspace => {
                    self.active_mut().answer.pop();
                }
                KeyCode::Char(c) => self.active_mut().answer.push(c),
                _ => {}
            },
            Modal::None => {}
        }
    }

    // --- вкладки ------------------------------------------------------------

    fn new_tab(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let mut tab = Tab::new(id, &self.model, self.mode);
        tab.push(Role::System, keymap_text());
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        let tab = self.tabs.remove(self.active);
        // Явное удаление вкладки убирает и сохранённую сессию с диска.
        if let Some(path) = &tab.save_path {
            let _ = std::fs::remove_file(path);
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    fn switch(&mut self, delta: i32) {
        let len = i32::try_from(self.tabs.len()).unwrap_or(1).max(1);
        let next = i32::try_from(self.active).unwrap_or(0) + delta;
        self.active = usize::try_from(((next % len) + len) % len).unwrap_or(0);
    }

    fn toggle_pin(&mut self) {
        let id = self.active().id;
        let tab = self.active_mut();
        tab.pinned = !tab.pinned;
        // Закреплённые — первыми (стабильно); активная следует за своей вкладкой.
        self.tabs.sort_by_key(|tab| !tab.pinned);
        self.active = self.tabs.iter().position(|tab| tab.id == id).unwrap_or(0);
    }

    // --- ввод ---------------------------------------------------------------

    fn reset_input(&mut self) {
        self.input = TextArea::default();
        configure_input(&mut self.input, self.theme);
    }

    fn submit(&mut self) {
        let text = self.input.lines().join("\n").trim().to_string();
        if text.is_empty() || self.active().running {
            return;
        }
        self.reset_input();
        self.active_mut().scroll_back = 0;
        if let Some(command) = text.strip_prefix('/') {
            if self.handle_command(command.trim()) {
                return;
            }
        }
        // Первое сообщение задаёт название вкладки (как в Claude Code).
        if !self.active().entries.iter().any(|e| e.role == Role::User) {
            let title = first_line(&text, 24);
            if !title.is_empty() {
                self.active_mut().title = title;
            }
        }
        self.active_mut().push(Role::User, text.clone());
        let handle_tx = self.active().handle.to_agent.clone();
        let tab = self.active_mut();
        tab.running = true;
        tab.activity = Activity::Model;
        tab.turn_start = Some(Instant::now());
        let _ = handle_tx.send(UiToAgent::Prompt(text));
    }

    fn handle_command(&mut self, command: &str) -> bool {
        let name = command.split_whitespace().next().unwrap_or_default();
        // Сырой остаток после имени команды (для хендлеров, берущих строку аргументов).
        let rest = command[name.len()..].trim();
        let args = (!rest.is_empty()).then_some(rest);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match name {
            "quit" | "exit" | "q" => {
                self.should_quit = true;
                true
            }
            "clear" => {
                let tab = self.active_mut();
                tab.entries.clear();
                tab.scroll_back = 0;
                true
            }
            "theme" => {
                self.theme = args.map_or_else(|| next_theme(self.theme), theme_by_name);
                configure_input(&mut self.input, self.theme);
                let name = self.theme.name.to_string();
                self.active_mut().push(Role::System, format!("тема: {name}"));
                true
            }
            "keymap" | "keys" => {
                self.active_mut().push(Role::System, keymap_text());
                true
            }
            // --- read-only отчёты (движок общий с REPL) --------------------------
            "memory" => self.report(crate::render_memory_report()),
            "diff" => self.report(crate::render_diff_report()),
            "config" => self.report(crate::render_config_report(args)),
            "mcp" => self.report(commands::handle_mcp_slash_command(args, &cwd).map_err(Into::into)),
            "skills" => {
                self.report(commands::handle_skills_slash_command(args, &cwd).map_err(Into::into))
            }
            "agents" => {
                self.report(commands::handle_agents_slash_command(args, &cwd).map_err(Into::into))
            }
            "external" => {
                self.report(commands::handle_external_slash_command(args, &cwd).map_err(Into::into))
            }
            "brain" => {
                self.report(commands::handle_brain_slash_command(args, &cwd).map_err(Into::into))
            }
            "hooks" => self.report(crate::render_hooks_report_for(&cwd)),
            "sandbox" => self.report(crate::render_sandbox_report_for(&cwd)),
            "version" => {
                self.active_mut()
                    .push(Role::System, crate::render_version_report());
                true
            }
            "tasks" => self.report(
                tools::execute_tool("TaskList", &serde_json::json!({})).map_err(Into::into),
            ),
            // --- stateful -------------------------------------------------------
            "compact" => {
                self.compact_active();
                true
            }
            "plan" => {
                self.toggle_plan_mode();
                true
            }
            "rewind" => {
                let n = args.and_then(|a| a.parse::<usize>().ok()).unwrap_or(1);
                self.rewind_active(n);
                true
            }
            "rename" => {
                match args {
                    Some(title) => {
                        let title = first_line(title, 24);
                        self.active_mut().title = title;
                    }
                    None => self
                        .active_mut()
                        .push(Role::System, "использование: /rename <название>".to_string()),
                }
                true
            }
            "permissions" | "perm" => {
                match args {
                    None => {
                        let mode = mode_label(self.active().mode);
                        self.active_mut().push(
                            Role::System,
                            format!("режим доступа: {mode}\nсменить: /permissions <read-only|workspace-write|danger-full-access>"),
                        );
                    }
                    Some(_) if self.active().running => self.active_mut().push(
                        Role::System,
                        "дождитесь завершения хода, затем /permissions <режим>".to_string(),
                    ),
                    Some(label) => match parse_permission_label(label) {
                        Some(mode) => {
                            let model = self.active().model.clone();
                            if self.respawn_active(model, mode) {
                                self.active_mut()
                                    .push(Role::System, format!("режим доступа: {}", mode_label(mode)));
                            }
                        }
                        None => self.active_mut().push(
                            Role::Error,
                            "режимы: read-only · workspace-write · danger-full-access".to_string(),
                        ),
                    },
                }
                true
            }
            "session" | "sessions" => {
                let tab = self.active();
                let path = tab
                    .save_path
                    .as_ref()
                    .map_or_else(|| "(не сохранена)".to_string(), |p| p.display().to_string());
                let msg = format!(
                    "сессия активной вкладки:\n  файл       {path}\n  записей    {}\n  вкладок    {}\n  (переключение — Ctrl+F/Ctrl+B, новая — Ctrl+N, сайдбар — Ctrl+E)",
                    tab.entries.len(),
                    self.tabs.len(),
                );
                self.active_mut().push(Role::System, msg);
                true
            }
            // --- prompt-макросы: формируют промпт и запускают обычный ход -------
            "commit" | "review" | "pr" | "security-review" | "init" => {
                self.dispatch_macro(macro_prompt(name, args));
                true
            }
            "model" => {
                match args {
                    None => {
                        let current = self.active().model.clone();
                        let hint = providers_hint();
                        self.active_mut()
                            .push(Role::System, format!("модель: {current}\n{hint}"));
                    }
                    Some(_) if self.active().running => {
                        self.active_mut().push(
                            Role::System,
                            "дождитесь завершения хода, затем /model <id>".to_string(),
                        );
                    }
                    Some(id) => self.switch_model(id),
                }
                true
            }
            "help" => {
                self.active_mut().push(Role::System, app_help_text());
                true
            }
            _ => {
                self.active_mut().push(
                    Role::Error,
                    format!("неизвестная команда /{name} — /help покажет доступные"),
                );
                true
            }
        }
    }

    /// Рендерит результат текстовой команды: успех → System, ошибка → Error.
    /// Всегда «поглощает» команду (возвращает true).
    fn report(&mut self, result: Result<String, Box<dyn std::error::Error>>) -> bool {
        self.run_report(result);
        true
    }

    /// Запускает prompt-макрос: показывает промпт как ход пользователя и шлёт его
    /// воркеру обычным ходом (с инструментами). Макрос ссылается на скил, так что
    /// процедура берётся из ~/.claw/skills — правится без пересборки.
    fn dispatch_macro(&mut self, prompt: String) {
        if self.active().running {
            self.active_mut()
                .push(Role::System, "дождитесь завершения хода".to_string());
            return;
        }
        self.active_mut().push(Role::User, prompt.clone());
        let handle_tx = self.active().handle.to_agent.clone();
        let tab = self.active_mut();
        tab.running = true;
        tab.activity = Activity::Model;
        tab.turn_start = Some(Instant::now());
        let _ = handle_tx.send(UiToAgent::Prompt(prompt));
    }

    /// Обрабатывает клавишу в vim NORMAL-режиме (движения и правки поверх
    /// tui-textarea). Двухклавишные команды идут через `vim_pending` (d/c/g).
    /// Клавиши, когда фокус в меню сессий (сайдбаре): j/k — листать сессии,
    /// Tab/Enter/Esc/h/l — вернуться в основную область.
    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'd') if ctrl => self.should_quit = true,
            KeyCode::Char('n') if ctrl => self.new_tab(),
            KeyCode::Char('w') if ctrl => self.close_tab(),
            KeyCode::Char('p') if ctrl => self.toggle_pin(),
            KeyCode::Char('j') | KeyCode::Down => self.switch(1),
            KeyCode::Char('k') | KeyCode::Up => self.switch(-1),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter | KeyCode::Esc
            | KeyCode::Char('l' | 'h' | 'q') => self.focus = Focus::Main,
            _ => {}
        }
    }

    fn handle_vim_normal(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.focus = Focus::Sidebar;
            self.sidebar_collapsed = false;
            return;
        }
        let KeyCode::Char(c) = key.code else {
            match key.code {
                KeyCode::Left => self.input.move_cursor(CursorMove::Back),
                KeyCode::Right => self.input.move_cursor(CursorMove::Forward),
                KeyCode::Up => self.input.move_cursor(CursorMove::Up),
                KeyCode::Down => self.input.move_cursor(CursorMove::Down),
                _ => {}
            }
            return;
        };
        if ctrl {
            if c == 'r' {
                self.input.redo();
            }
            return;
        }
        if let Some(op) = self.vim_pending.take() {
            self.vim_operator(op, c);
            return;
        }
        match c {
            'h' => self.input.move_cursor(CursorMove::Back),
            'l' => self.input.move_cursor(CursorMove::Forward),
            'j' => self.input.move_cursor(CursorMove::Down),
            'k' => self.input.move_cursor(CursorMove::Up),
            'w' => self.input.move_cursor(CursorMove::WordForward),
            'b' => self.input.move_cursor(CursorMove::WordBack),
            'e' => self.input.move_cursor(CursorMove::WordEnd),
            '0' => self.input.move_cursor(CursorMove::Head),
            '$' => self.input.move_cursor(CursorMove::End),
            'G' => self.input.move_cursor(CursorMove::Bottom),
            'i' => self.vim_mode = VimMode::Insert,
            'a' => {
                self.input.move_cursor(CursorMove::Forward);
                self.vim_mode = VimMode::Insert;
            }
            'A' => {
                self.input.move_cursor(CursorMove::End);
                self.vim_mode = VimMode::Insert;
            }
            'I' => {
                self.input.move_cursor(CursorMove::Head);
                self.vim_mode = VimMode::Insert;
            }
            'o' => {
                self.input.move_cursor(CursorMove::End);
                self.input.insert_newline();
                self.vim_mode = VimMode::Insert;
            }
            'O' => {
                self.input.move_cursor(CursorMove::Head);
                self.input.insert_newline();
                self.input.move_cursor(CursorMove::Up);
                self.vim_mode = VimMode::Insert;
            }
            'x' => {
                self.input.delete_next_char();
            }
            'D' => {
                self.input.delete_line_by_end();
            }
            'C' => {
                self.input.delete_line_by_end();
                self.vim_mode = VimMode::Insert;
            }
            'u' => {
                self.input.undo();
            }
            'p' => {
                self.input.paste();
            }
            'd' | 'c' | 'g' => self.vim_pending = Some(c),
            _ => {}
        }
    }

    /// Двухклавишные vim-команды: dd, dw, d$, cc, cw, gg.
    fn vim_operator(&mut self, op: char, motion: char) {
        match (op, motion) {
            ('g', 'g') => self.input.move_cursor(CursorMove::Top),
            ('g', 't') => self.switch(1),
            ('g', 'T') => self.switch(-1),
            ('d', 'd') => {
                self.input.move_cursor(CursorMove::Head);
                self.input.delete_line_by_end();
                self.input.delete_next_char();
            }
            ('d', 'w') => {
                self.input.delete_next_word();
            }
            ('d', '$') => {
                self.input.delete_line_by_end();
            }
            ('c', 'c') => {
                self.input.move_cursor(CursorMove::Head);
                self.input.delete_line_by_end();
                self.vim_mode = VimMode::Insert;
            }
            ('c', 'w') => {
                self.input.delete_next_word();
                self.vim_mode = VimMode::Insert;
            }
            _ => {}
        }
    }

    /// `/compact`: сжимает сессию активной вкладки (как в REPL) и перезапускает
    /// воркер на сжатой сессии, сохраняя её на диск.
    fn compact_active(&mut self) {
        if self.active().running {
            self.active_mut().push(
                Role::System,
                "дождитесь завершения хода, затем /compact".to_string(),
            );
            return;
        }
        let Some(path) = self.active().save_path.clone() else {
            self.active_mut().push(
                Role::System,
                "нечего сжимать: сессия ещё не сохранена — сделайте ход".to_string(),
            );
            return;
        };
        let Ok(session) = Session::load_from_path(&path) else {
            self.active_mut()
                .push(Role::Error, "не удалось прочитать сессию для сжатия".to_string());
            return;
        };
        let result = runtime::compact_session(
            &session,
            runtime::CompactionConfig {
                max_estimated_tokens: 0,
                ..runtime::CompactionConfig::default()
            },
        );
        let removed = result.removed_message_count;
        if let Err(error) = result.compacted_session.save_to_path(&path) {
            self.active_mut()
                .push(Role::Error, format!("не удалось сохранить сжатую сессию: {error}"));
            return;
        }
        let tab = self.active_mut();
        tab.handle = worker::spawn_agent(
            tab.model.clone(),
            tab.mode,
            result.compacted_session,
            Some(path),
        );
        let message = if removed == 0 {
            "сжатие пропущено: сессия ниже порога".to_string()
        } else {
            format!("сжато: удалено {removed} сообщений в резюме")
        };
        tab.push(Role::System, message);
    }

    fn run_report(&mut self, result: Result<String, Box<dyn std::error::Error>>) {
        match result {
            Ok(text) => self.active_mut().push(Role::System, text),
            Err(error) => self.active_mut().push(Role::Error, error.to_string()),
        }
    }

    /// Перезапускает воркер активной вкладки на сохранённой сессии с заданными
    /// моделью и режимом (сохраняя историю). Общая основа для /model, /plan.
    /// `false` + сообщение, если сессия ещё не сохранена/не читается.
    fn respawn_active(&mut self, model: String, mode: PermissionMode) -> bool {
        let tab = self.active_mut();
        let Some(path) = tab.save_path.clone() else {
            tab.push(
                Role::System,
                "сначала сделайте ход — сессия ещё не сохранена".to_string(),
            );
            return false;
        };
        let Ok(session) = Session::load_from_path(&path) else {
            tab.push(Role::Error, "не удалось прочитать сессию".to_string());
            return false;
        };
        tab.handle = worker::spawn_agent(model.clone(), mode, session, Some(path));
        tab.model = model;
        tab.mode = mode;
        true
    }

    /// Переключает модель активной вкладки (режим сохраняется).
    fn switch_model(&mut self, id: &str) {
        let model = crate::resolve_model_alias(id).to_string();
        let mode = self.active().mode;
        if self.respawn_active(model.clone(), mode) {
            self.active_mut()
                .push(Role::System, format!("модель переключена: {model}"));
        }
    }

    /// `/plan`: тумблер режима планирования (read-only) — агент рассуждает и
    /// планирует, но не может писать в файлы, пока план не выключен.
    fn toggle_plan_mode(&mut self) {
        if self.active().running {
            self.active_mut().push(
                Role::System,
                "дождитесь завершения хода, затем /plan".to_string(),
            );
            return;
        }
        let (mode, message) = if self.active().mode == PermissionMode::ReadOnly {
            (PermissionMode::WorkspaceWrite, "plan mode выключен — правки разрешены")
        } else {
            (PermissionMode::ReadOnly, "plan mode включён — правки заблокированы, только чтение/план")
        };
        let model = self.active().model.clone();
        if self.respawn_active(model, mode) {
            self.active_mut().push(Role::System, message.to_string());
        }
    }

    /// `/rewind [n]`: откатывает последние N ходов пользователя — обрезает
    /// сессию до нужного места, сохраняет, перезапускает воркер и перестраивает
    /// историю в UI. По умолчанию N=1.
    fn rewind_active(&mut self, n: usize) {
        if self.active().running {
            self.active_mut().push(
                Role::System,
                "дождитесь завершения хода, затем /rewind".to_string(),
            );
            return;
        }
        let Some(path) = self.active().save_path.clone() else {
            self.active_mut().push(
                Role::System,
                "нечего откатывать: сессия ещё не сохранена".to_string(),
            );
            return;
        };
        let Ok(mut session) = Session::load_from_path(&path) else {
            self.active_mut()
                .push(Role::Error, "не удалось прочитать сессию".to_string());
            return;
        };
        let user_positions: Vec<usize> = session
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.role == MessageRole::User)
            .map(|(index, _)| index)
            .collect();
        if user_positions.is_empty() {
            self.active_mut()
                .push(Role::System, "нечего откатывать".to_string());
            return;
        }
        let keep_turns = user_positions.len().saturating_sub(n.max(1));
        let truncate_at = if keep_turns == 0 {
            0
        } else {
            user_positions[keep_turns]
        };
        let removed = user_positions.len() - keep_turns;
        session.messages.truncate(truncate_at);
        if let Err(error) = session.save_to_path(&path) {
            self.active_mut()
                .push(Role::Error, format!("не удалось сохранить сессию: {error}"));
            return;
        }
        let entries = rebuild_entries(&session);
        let model = self.active().model.clone();
        let mode = self.active().mode;
        let tab = self.active_mut();
        tab.handle = worker::spawn_agent(model, mode, session, Some(path));
        tab.entries = entries;
        tab.scroll_back = 0;
        tab.push(Role::System, format!("↩ откат: убрано {removed} ход(ов)"));
    }

    fn cancel_turn(&mut self) {
        self.active().handle.cancel.store(true, Ordering::SeqCst);
        self.active_mut()
            .push(Role::System, "⏹ отмена хода…".to_string());
    }

    // --- автодополнение -----------------------------------------------------

    fn recompute_completion(&mut self) {
        let (row, col) = self.input.cursor();
        let line = self.input.lines().get(row).cloned().unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let col = col.min(chars.len());
        let mut start = col;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let token: String = chars[start..col].iter().collect();
        if start == 0 && token.starts_with('/') {
            let partial = &token[1..];
            let items: Vec<String> = COMMANDS
                .iter()
                .filter(|cmd| cmd[1..].starts_with(partial))
                .map(|cmd| (*cmd).to_string())
                .collect();
            self.compl =
                (!items.is_empty()).then_some(Completion { start, row, items, selected: 0 });
        } else if let Some(partial) = token.strip_prefix('@') {
            let items = file_completions(partial);
            self.compl =
                (!items.is_empty()).then_some(Completion { start, row, items, selected: 0 });
        } else {
            self.compl = None;
        }
    }

    fn compl_move(&mut self, delta: i32) {
        if let Some(compl) = self.compl.as_mut() {
            let len = i32::try_from(compl.items.len()).unwrap_or(1).max(1);
            let next = i32::try_from(compl.selected).unwrap_or(0) + delta;
            compl.selected = usize::try_from(((next % len) + len) % len).unwrap_or(0);
        }
    }

    fn accept_completion(&mut self) {
        let Some(compl) = self.compl.take() else {
            return;
        };
        let Some(item) = compl.items.get(compl.selected).cloned() else {
            return;
        };
        let row = compl.row;
        let mut lines: Vec<String> = self.input.lines().to_vec();
        let Some(line) = lines.get(row).cloned() else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let (_, col) = self.input.cursor();
        let col = col.min(chars.len());
        let start = compl.start.min(chars.len());
        let prefix: String = chars[..start].iter().collect();
        let suffix: String = chars[col..].iter().collect();
        lines[row] = format!("{prefix}{item}{suffix}");
        let new_col = start + item.chars().count();
        let mut input = TextArea::new(lines);
        configure_input(&mut input, self.theme);
        input.move_cursor(CursorMove::Jump(
            u16::try_from(row).unwrap_or(0),
            u16::try_from(new_col).unwrap_or(0),
        ));
        self.input = input;
    }

    // --- отрисовка ----------------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        let sidebar_w = if self.sidebar_collapsed { 3 } else { 26 };
        let outer = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_w), Constraint::Min(20)])
            .split(frame.area());
        self.draw_sidebar(frame, outer[0]);

        let input_lines = u16::try_from(self.input.lines().len().clamp(1, 6)).unwrap_or(6);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(input_lines + 2),
                Constraint::Length(1),
            ])
            .split(outer[1]);
        self.draw_history(frame, main[0]);
        self.draw_input(frame, main[1]);
        self.draw_footer(frame, main[2]);
        if self.compl.is_some() {
            self.draw_completion(frame, main[1]);
        }
        if !matches!(self.active().modal, Modal::None) {
            self.draw_modal(frame);
        }
    }

    fn draw_sidebar(&self, frame: &mut Frame, area: Rect) {
        let theme = self.theme;
        let collapsed = self.sidebar_collapsed;
        let inner = area.width.saturating_sub(2) as usize;
        let mut rows = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let active = index == self.active;
            let color = tab_status_color(tab, theme);
            if collapsed {
                let dot_color = if active { theme.accent } else { color };
                rows.push(Line::from(Span::styled(
                    icons::DOT,
                    Style::default().fg(dot_color),
                )));
            } else {
                let pin = if tab.pinned {
                    format!("{} ", icons::PIN)
                } else {
                    String::new()
                };
                let title = short(&tab.title, inner.saturating_sub(4));
                let mark = if active { icons::ACTIVE } else { " " };
                let mut label_style =
                    Style::default().fg(if active { theme.text } else { theme.muted });
                if active {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                rows.push(Line::from(vec![
                    Span::styled(mark, Style::default().fg(theme.accent)),
                    Span::styled(format!(" {} ", icons::DOT), Style::default().fg(color)),
                    Span::styled(format!("{pin}{title}"), label_style),
                ]));
            }
        }
        let focused = self.focus == Focus::Sidebar;
        let title = if collapsed {
            ""
        } else if focused {
            " сессии ◂ j/k "
        } else {
            " сессии "
        };
        let border = if focused { theme.accent } else { theme.muted };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(Span::styled(
                title,
                Style::default().fg(if focused { theme.accent } else { theme.accent2 }),
            ));
        frame.render_widget(
            Paragraph::new(rows).block(block).style(Style::default().bg(theme.bar_bg)),
            area,
        );
    }

    fn draw_history(&mut self, frame: &mut Frame, area: Rect) {
        let theme = self.theme;
        let width = area.width.saturating_sub(2).max(1) as usize;
        let lines = self.history_lines(width);
        let viewport = area.height.saturating_sub(2);
        let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let max_scroll = total.saturating_sub(viewport);
        let scroll_back = self.active().scroll_back.min(max_scroll);
        self.active_mut().scroll_back = scroll_back;
        let offset = max_scroll.saturating_sub(scroll_back);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.muted))
            .title(Span::styled(
                format!(" {} ", self.active().title),
                Style::default().fg(theme.accent2),
            ));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .scroll((offset, 0)),
            area,
        );
    }

    fn history_lines(&self, width: usize) -> Vec<Line<'static>> {
        let theme = self.theme;
        let mut lines = Vec::new();
        for entry in &self.active().entries {
            if entry.role == Role::Assistant {
                self.push_markdown(&mut lines, &entry.text, width);
                lines.push(Line::from(""));
                continue;
            }
            if entry.role == Role::Diff {
                self.push_code_block(&mut lines, &entry.text, "diff", width);
                continue;
            }
            let (prefix, color, dim) = match entry.role {
                Role::User => ("▍ ", theme.accent, false),
                Role::Thinking => ("· ", theme.muted, true),
                Role::Tool => ("▹ ", theme.accent2, true),
                Role::ToolResult => ("", theme.success, true),
                Role::Error => ("✗ ", theme.error, false),
                Role::System => ("» ", theme.muted, true),
                Role::Assistant | Role::Diff => unreachable!(),
            };
            let mut style = Style::default().fg(color);
            if dim {
                style = style.add_modifier(Modifier::DIM);
            }
            let body = if prefix.is_empty() {
                entry.text.clone()
            } else {
                format!("{prefix}{}", entry.text)
            };
            for wrapped in wrap_text(&body, width) {
                lines.push(Line::from(Span::styled(wrapped, style)));
            }
            if entry.role == Role::User {
                lines.push(Line::from(""));
            }
        }
        lines
    }

    /// Рендер markdown-ответа ассистента в ratatui-строки: заголовки, **жирный**,
    /// *курсив*, `инлайн-код`, списки, цитаты, правила и переносы — через
    /// pulldown-cmark; блоки кода ```…``` — подсветка синтаксиса (syntect) моно-
    /// панелью. Абзацы переносятся по ширине.
    fn push_markdown(&self, lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
        let mut md = MdBuilder::new(self.theme, width);
        let mut in_code = false;
        let mut code_lang = String::new();
        let mut code_buf = String::new();
        // Текущая ссылка: URL и накопленный видимый текст (для сравнения, чтобы не
        // печатать `url (url)` для «голых» автоссылок).
        let mut link_url: Option<String> = None;
        let mut link_text = String::new();
        for event in Parser::new_ext(text, Options::all()) {
            match event {
                MdEvent::Start(Tag::CodeBlock(kind)) => {
                    md.flush(lines);
                    in_code = true;
                    code_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.trim().to_ascii_lowercase(),
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                MdEvent::End(TagEnd::CodeBlock) => {
                    in_code = false;
                    self.push_code_block(lines, code_buf.trim_end_matches('\n'), &code_lang, width);
                    lines.push(Line::from(""));
                }
                MdEvent::Text(t) if in_code => code_buf.push_str(&t),
                MdEvent::Text(t) => {
                    if link_url.is_some() {
                        link_text.push_str(&t);
                    }
                    md.text(lines, &t);
                }
                MdEvent::Code(c) => md.inline_code(lines, &c),
                MdEvent::Start(Tag::Strong) => md.strong += 1,
                MdEvent::End(TagEnd::Strong) => md.strong = md.strong.saturating_sub(1),
                MdEvent::Start(Tag::Emphasis) => md.emphasis += 1,
                MdEvent::End(TagEnd::Emphasis) => md.emphasis = md.emphasis.saturating_sub(1),
                MdEvent::Start(Tag::Link { dest_url, .. }) => {
                    md.link += 1;
                    link_url = Some(dest_url.to_string());
                    link_text.clear();
                }
                MdEvent::End(TagEnd::Link) => {
                    // ratatui не умеет OSC 8, поэтому печатаем сам URL рядом с
                    // текстом — его видно и можно открыть Cmd/Ctrl+кликом в
                    // терминалах с автолинковкой. md.link ещё > 0 → URL идёт
                    // тем же link-стилем.
                    if let Some(url) = link_url.take() {
                        if !url.is_empty() && link_text.trim() != url.trim() {
                            md.text(lines, &format!(" ({url})"));
                        }
                    }
                    md.link = md.link.saturating_sub(1);
                }
                MdEvent::Start(Tag::Heading { level, .. }) => {
                    md.flush(lines);
                    md.heading = heading_rank(level);
                }
                MdEvent::End(TagEnd::Heading(..)) => {
                    md.flush(lines);
                    md.heading = 0;
                    lines.push(Line::from(""));
                }
                MdEvent::End(TagEnd::Paragraph) => {
                    md.flush(lines);
                    lines.push(Line::from(""));
                }
                MdEvent::Start(Tag::List(first)) => md.list.push(first),
                MdEvent::End(TagEnd::List(..)) => {
                    md.list.pop();
                    md.flush(lines);
                }
                MdEvent::Start(Tag::Item) => {
                    md.flush(lines);
                    md.start_item();
                }
                MdEvent::End(TagEnd::Item) => md.flush(lines),
                MdEvent::Start(Tag::BlockQuote(..)) => md.quote += 1,
                MdEvent::End(TagEnd::BlockQuote(..)) => {
                    md.quote = md.quote.saturating_sub(1);
                    md.flush(lines);
                }
                MdEvent::SoftBreak => md.text(lines, " "),
                MdEvent::HardBreak => md.flush(lines),
                MdEvent::Rule => {
                    md.flush(lines);
                    lines.push(rule_line(self.theme, width));
                }
                _ => {}
            }
        }
        if in_code && !code_buf.is_empty() {
            self.push_code_block(lines, code_buf.trim_end_matches('\n'), &code_lang, width);
        }
        md.flush(lines);
    }

    /// Добавляет подсвеченный блок кода (через кэш) в моно-панель.
    fn push_code_block(&self, lines: &mut Vec<Line<'static>>, code: &str, lang: &str, width: usize) {
        let code_bg = Color::Rgb(30, 33, 44);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        lang.hash(&mut hasher);
        code.hash(&mut hasher);
        width.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(cached) = self.hl_cache.borrow().get(&key) {
            lines.extend(cached.iter().cloned());
            return;
        }
        let block = highlight::highlight_block(code, lang, code_bg, width);
        {
            let mut cache = self.hl_cache.borrow_mut();
            // Стриминг плодит временные ключи (растущий блок) — ограничиваем рост.
            if cache.len() > 1024 {
                cache.clear();
            }
            cache.insert(key, block.clone());
        }
        lines.extend(block);
    }

    fn draw_input(&mut self, frame: &mut Frame, area: Rect) {
        let theme = self.theme;
        let running = self.active().running;
        let border = if running { theme.muted } else { theme.accent };
        let typing_command = self
            .input
            .lines()
            .first()
            .is_some_and(|line| line.starts_with('/'));
        let title = if typing_command {
            " /commit · /review · /pr · /model · /plan · /compact · /rewind · /mcp · /skills · /help — все в /help "
        } else {
            " ввод "
        };
        self.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(Span::styled(title, Style::default().fg(theme.muted))),
        );
        frame.render_widget(&self.input, area);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let theme = self.theme;
        let tab = self.active();
        let spin = icons::SPINNER[self.frame_tick % icons::SPINNER.len()];
        let (icon, label, color) = match &tab.activity {
            Activity::Idle => ("●", "готов".to_string(), theme.success),
            Activity::Model => (spin, tab.elapsed_label("думает"), theme.working),
            Activity::Tool { label } => (spin, tab.elapsed_label(label), theme.accent2),
            Activity::Waiting { label } => ("?", format!("ждёт: {label}"), theme.error),
        };
        let branch = self
            .branch
            .as_deref()
            .map_or_else(String::new, |b| format!("  ⎇ {b}"));
        let left = format!(
            " {icon} {label}   ·   {}   ·   {}k↑ {}k↓{branch}",
            tab.model,
            tab.in_tok / 1000,
            tab.out_tok / 1000,
        );
        let mut spans = vec![Span::styled(left, Style::default().fg(color))];
        let (mode_label, mode_color) = match self.vim_mode {
            VimMode::Normal => (" NORMAL ", theme.accent2),
            VimMode::Insert => (" INSERT ", theme.success),
        };
        spans.push(Span::styled(
            format!("  {mode_label}"),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ));
        if tab.mode == PermissionMode::ReadOnly {
            spans.push(Span::styled(
                "   ⏸ PLAN",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            format!("   {}", short_path(&self.cwd)),
            Style::default().fg(theme.muted),
        ));
        let line = Line::from(spans);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.bar_bg)),
            area,
        );
    }

    fn draw_completion(&self, frame: &mut Frame, input_area: Rect) {
        let Some(compl) = &self.compl else {
            return;
        };
        let theme = self.theme;
        let height = u16::try_from(compl.items.len() + 2).unwrap_or(10).min(10);
        let longest = compl
            .items
            .iter()
            .map(|item| item.chars().count())
            .max()
            .unwrap_or(12);
        let width = u16::try_from(longest + 4)
            .unwrap_or(24)
            .clamp(16, input_area.width.saturating_sub(2).max(16));
        let area = Rect {
            x: input_area.x + 1,
            y: input_area.y.saturating_sub(height),
            width,
            height,
        };
        frame.render_widget(Clear, area);
        let visible = height.saturating_sub(2) as usize;
        let start = compl.selected.saturating_sub(visible.saturating_sub(1));
        let lines: Vec<Line> = compl
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, item)| {
                let style = if index == compl.selected {
                    Style::default().fg(theme.bar_bg).bg(theme.accent)
                } else {
                    Style::default().fg(theme.text)
                };
                Line::from(Span::styled(format!(" {item} "), style))
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(" ↹/↑↓ ", Style::default().fg(theme.muted)));
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .style(Style::default().bg(theme.bar_bg)),
            area,
        );
    }

    fn draw_modal(&self, frame: &mut Frame) {
        let theme = self.theme;
        let area = centered_rect(70, 45, frame.area());
        frame.render_widget(Clear, area);
        let (title, body, footer) = match &self.active().modal {
            Modal::Permission {
                tool,
                input,
                reason,
            } => (
                " запрос разрешения ",
                format!(
                    "Инструмент: {tool}\n\n{}{}",
                    first_line(input, 400),
                    reason
                        .as_deref()
                        .map_or_else(String::new, |r| format!("\n\nПричина: {r}")),
                ),
                "y — разрешить · n — отклонить",
            ),
            Modal::Question { question, options } => {
                let mut text = format!("{question}\n\n");
                for (index, option) in options.iter().enumerate() {
                    text.push_str(&format!("  {}. {option}\n", index + 1));
                }
                if !self.active().answer.is_empty() {
                    text.push_str(&format!("\n> {}", self.active().answer));
                }
                (" вопрос ", text, "1-9 — выбрать · текст+Enter · Esc — пропустить")
            }
            Modal::None => return,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(title, Style::default().fg(theme.accent)))
            .title_bottom(Span::styled(
                format!(" {footer} "),
                Style::default().fg(theme.muted),
            ));
        frame.render_widget(
            Paragraph::new(body)
                .block(block)
                .style(Style::default().fg(theme.text).bg(theme.bar_bg))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

impl Tab {
    fn elapsed_label(&self, verb: &str) -> String {
        self.turn_start.map_or_else(
            || verb.to_string(),
            |start| format!("{verb} · {}s", start.elapsed().as_secs()),
        )
    }
}

// --- утилиты ---------------------------------------------------------------

fn configure_input(input: &mut TextArea<'static>, theme: Theme) {
    input.set_style(Style::default().fg(theme.text));
    input.set_cursor_line_style(Style::default());
    input.set_placeholder_text("Спросите что-нибудь…  (Enter — отправить, Alt+Enter — строка)");
}

fn tab_status_color(tab: &Tab, theme: Theme) -> Color {
    if !matches!(tab.modal, Modal::None) {
        theme.error
    } else if tab.running {
        theme.working
    } else {
        theme.muted
    }
}

fn next_theme(current: Theme) -> Theme {
    let index = THEMES.iter().position(|t| t.name == current.name).unwrap_or(0);
    THEMES[(index + 1) % THEMES.len()]
}

fn theme_by_name(name: &str) -> Theme {
    THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(THEMES[0])
}

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

/// Загружает до 8 последних непустых сессий из `.claw/sessions` текущего
/// проекта (новые — первыми) как вкладки, восстанавливая историю.
fn load_recent_sessions(model: &str, mode: PermissionMode) -> Vec<Tab> {
    let Some(dir) = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".claw").join("sessions"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let mtime = std::fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
            Some((mtime, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .filter_map(|(_, path)| Session::load_from_path(&path).ok())
        .filter(|session| !session.messages.is_empty())
        .take(8)
        .enumerate()
        .map(|(index, session)| Tab::from_session(index + 1, model, mode, session, None))
        .collect()
}

/// Восстанавливает строки ленты из истории сессии.
fn rebuild_entries(session: &Session) -> Vec<Entry> {
    let mut out = Vec::new();
    for message in &session.messages {
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    match message.role {
                        MessageRole::User => out.push(Entry {
                            role: Role::User,
                            text: text.clone(),
                        }),
                        MessageRole::Assistant => out.push(Entry {
                            role: Role::Assistant,
                            text: text.clone(),
                        }),
                        MessageRole::System | MessageRole::Tool => {}
                    }
                }
                ContentBlock::ToolUse { name, input, .. } => out.push(Entry {
                    role: Role::Tool,
                    text: format!("{name}  {}", first_line(input, 200)),
                }),
                ContentBlock::ToolResult {
                    output, is_error, ..
                } => out.push(Entry {
                    role: if *is_error { Role::Error } else { Role::ToolResult },
                    text: format!("⎿ {}", first_line(output, 400)),
                }),
            }
        }
    }
    out
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

/// Строка с настроенными провайдерами из `providers.toml`.
fn providers_hint() -> String {
    let config = api::ProvidersConfig::load();
    let mut parts = Vec::new();
    if let Some(slot) = config.primary {
        parts.push(format!("primary={}", slot.model));
    }
    if let Some(slot) = config.auxiliary {
        parts.push(format!("auxiliary={}", slot.model));
    }
    if parts.is_empty() {
        "настроенных провайдеров нет".to_string()
    } else {
        format!("настроено: {}", parts.join("  "))
    }
}

fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read-only (план: правки заблокированы)",
        PermissionMode::WorkspaceWrite => "workspace-write (правки в проекте разрешены)",
        PermissionMode::DangerFullAccess => "danger-full-access (без ограничений)",
        PermissionMode::Prompt => "prompt (спрашивать по каждому действию)",
        PermissionMode::Allow => "allow (разрешать без спроса)",
    }
}

fn parse_permission_label(label: &str) -> Option<PermissionMode> {
    match label.trim().to_ascii_lowercase().as_str() {
        "read-only" | "readonly" | "plan" | "ro" => Some(PermissionMode::ReadOnly),
        "workspace-write" | "write" | "workspace" | "edit" => Some(PermissionMode::WorkspaceWrite),
        "danger-full-access" | "danger" | "full" | "yolo" => Some(PermissionMode::DangerFullAccess),
        "prompt" | "ask" => Some(PermissionMode::Prompt),
        "allow" => Some(PermissionMode::Allow),
        _ => None,
    }
}

/// Максимум строк на сторону в diff-панели правки (чтобы не заливать экран).
const DIFF_MAX_LINES: usize = 40;

/// Строит unified-diff-текст для правки Edit/Write из JSON-инпута тула. `None`,
/// если это не файловая правка или инпут не распарсился.
fn build_edit_diff(name: &str, input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    match name {
        "edit_file" => {
            let old = value.get("old_string")?.as_str()?;
            let new = value.get("new_string")?.as_str()?;
            let mut out = String::new();
            push_diff_side(&mut out, old, '-');
            push_diff_side(&mut out, new, '+');
            Some(out)
        }
        "write_file" => {
            let content = value.get("content")?.as_str()?;
            let mut out = String::new();
            push_diff_side(&mut out, content, '+');
            Some(out)
        }
        _ => None,
    }
}

/// Дописывает строки одной стороны diff с маркером (`+`/`-`), обрезая до лимита.
fn push_diff_side(out: &mut String, text: &str, marker: char) {
    let lines: Vec<&str> = text.lines().collect();
    for line in lines.iter().take(DIFF_MAX_LINES) {
        out.push(marker);
        out.push_str(line);
        out.push('\n');
    }
    if lines.len() > DIFF_MAX_LINES {
        out.push_str(&format!("… ещё {} строк\n", lines.len() - DIFF_MAX_LINES));
    }
}

/// Извлекает `path` из JSON-инпута файлового тула.
fn edit_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value.get("path")?.as_str().map(str::to_string)
}

/// Текст промпта для prompt-макроса. Ссылается на соответствующий прозрачный
/// скил (~/.claw/skills), поэтому процедура редактируется без пересборки.
fn macro_prompt(kind: &str, args: Option<&str>) -> String {
    let extra = args.map_or_else(String::new, |a| format!("\nКонтекст: {a}"));
    let body = match kind {
        "commit" => "Подготовь git-коммит для текущих изменений. Используй скил `commit` \
                     (вызови инструмент Skill со skill=commit) для точной процедуры; предложи \
                     сообщение и команду, но не коммить без явной просьбы.",
        "review" => "Проверь текущий diff на корректность и проблемы. Используй скил \
                     `code-review`. Дай находки как file:line + суть + фикс.",
        "pr" => "Составь заголовок и описание pull request для этой ветки по diff и коммитам. \
                 Используй скил `pr-description`.",
        "security-review" => "Проведи security-review текущего diff по чеклисту. Используй скил \
                              `security-review`. Находки — с severity и фиксом.",
        "init" => "Проанализируй этот проект и создай/обнови CLAUDE.md: команды сборки/тестов, \
                   структуру, конвенции и важные детали для будущих сессий.",
        _ => "",
    };
    format!("{body}{extra}")
}

/// Инкрементальный сборщик строк markdown: копит спаны текущей строки, переносит
/// по ширине, добавляет префиксы цитат/списков. Блоки кода рендерятся отдельно.
struct MdBuilder {
    theme: Theme,
    width: usize,
    cur: Vec<Span<'static>>,
    col: usize,
    prefix_w: usize,
    strong: u32,
    emphasis: u32,
    link: u32,
    heading: u8,
    quote: u32,
    list: Vec<Option<u64>>,
    item_prefix: Option<String>,
}

impl MdBuilder {
    fn new(theme: Theme, width: usize) -> Self {
        Self {
            theme,
            width: width.max(8),
            cur: Vec::new(),
            col: 0,
            prefix_w: 0,
            strong: 0,
            emphasis: 0,
            link: 0,
            heading: 0,
            quote: 0,
            list: Vec::new(),
            item_prefix: None,
        }
    }

    fn style(&self) -> Style {
        let mut style = Style::default().fg(self.theme.text);
        if self.heading > 0 {
            style = style.fg(self.theme.accent).add_modifier(Modifier::BOLD);
        }
        if self.strong > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.emphasis > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.quote > 0 {
            style = style.fg(self.theme.muted).add_modifier(Modifier::ITALIC);
        }
        if self.link > 0 {
            style = style.fg(self.theme.accent2).add_modifier(Modifier::UNDERLINED);
        }
        style
    }

    /// Добавляет префикс строки (цитаты `│`, маркер списка) при её начале.
    fn ensure_prefix(&mut self) {
        if self.col != 0 {
            return;
        }
        let mut width = 0;
        for _ in 0..self.quote {
            self.cur
                .push(Span::styled("│ ", Style::default().fg(self.theme.muted)));
            width += 2;
        }
        if let Some(prefix) = self.item_prefix.take() {
            width += prefix.chars().count();
            self.cur
                .push(Span::styled(prefix, Style::default().fg(self.theme.accent2)));
        }
        self.prefix_w = width;
        self.col = width;
    }

    fn push_word(&mut self, lines: &mut Vec<Line<'static>>, word: &str, style: Style) {
        self.ensure_prefix();
        let word_w = word.chars().count();
        let need_space = self.col > self.prefix_w;
        if self.col + word_w + usize::from(need_space) > self.width && self.col > self.prefix_w {
            self.flush(lines);
            self.ensure_prefix();
        } else if need_space {
            self.cur.push(Span::raw(" "));
            self.col += 1;
        }
        self.col += word_w;
        self.cur.push(Span::styled(word.to_string(), style));
    }

    fn text(&mut self, lines: &mut Vec<Line<'static>>, text: &str) {
        let style = self.style();
        for word in text.split_whitespace() {
            self.push_word(lines, word, style);
        }
    }

    fn inline_code(&mut self, lines: &mut Vec<Line<'static>>, code: &str) {
        let style = Style::default()
            .fg(Color::Rgb(224, 200, 140))
            .bg(Color::Rgb(40, 43, 54));
        self.push_word(lines, code, style);
    }

    fn start_item(&mut self) {
        let indent = "  ".repeat(self.list.len().saturating_sub(1));
        let marker = match self.list.last_mut() {
            Some(Some(number)) => {
                let current = *number;
                *number += 1;
                format!("{current}. ")
            }
            _ => "• ".to_string(),
        };
        self.item_prefix = Some(format!("{indent}{marker}"));
    }

    fn flush(&mut self, lines: &mut Vec<Line<'static>>) {
        if !self.cur.is_empty() {
            lines.push(Line::from(std::mem::take(&mut self.cur)));
        }
        self.col = 0;
        self.prefix_w = 0;
    }
}

fn heading_rank(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        _ => 3,
    }
}

fn rule_line(theme: Theme, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width.max(1)),
        Style::default().fg(theme.muted),
    ))
}

/// Полная карта клавиш (vim-режим). Показывается в новых сессиях и по `/keymap`.
fn keymap_text() -> String {
    [
        "⌨  Карта клавиш (модальный vim-ввод, INSERT по умолчанию):",
        "  INSERT (печать):  текст · Enter — отправить · Alt/Shift+Enter — перенос строки · Esc → NORMAL",
        "  NORMAL (Esc):     h j k l — курсор · w b e — по словам · 0 $ — края · gg G — верх/низ",
        "     правки:        i a A I o O — вставка · x — удалить символ · dd dw D — удалить · cc cw C — заменить",
        "                    u — отмена · Ctrl+r — повтор · p — вставить",
        "  Сессии:           Tab — фокус в меню сессий (там j/k — листать, Tab/Enter/Esc — назад)",
        "                    gt / gT — след/пред · Ctrl+N — новая · Ctrl+W — закрыть · Ctrl+P — закрепить",
        "  Экран:            Ctrl+E — свернуть сайдбар · Ctrl+T — тема · PgUp/PgDn — прокрутка",
        "  Команды:          /help — все команды · /keymap — эта справка",
    ]
    .join("\n")
}

fn app_help_text() -> String {
    [
        "Действия (запускают ход): /commit · /review · /pr · /security-review · /init",
        "",
        "Команды:",
        "  /model [id]        показать/сменить модель          /plan          тумблер режима планирования",
        "  /permissions <m>   режим доступа                    /compact       сжать контекст",
        "  /rewind [n]        откатить N ходов (1)              /rename <имя>  переименовать вкладку",
        "  /tasks             фоновые задачи                   /session       инфо о сессии",
        "  /mcp [list|show]   MCP-серверы                      /skills [list] скилы",
        "  /agents [list]     агенты                           /external      внешняя консультация",
        "  /hooks             хуки                             /sandbox       статус песочницы",
        "  /brain [on|off]    cozby-brain                      /config [секц] настройки",
        "  /memory            память                           /diff          изменения git",
        "  /version           версия                           /keymap        карта клавиш",
        "  /theme [name]      тема                             /clear         очистить вид",
        "  /help  /quit",
        "Вкладки: Ctrl+N новая · Ctrl+F/Ctrl+B переключить · Ctrl+W закрыть · Ctrl+P пин · Ctrl+E сайдбар",
        "Ввод: Enter — отправить · Alt+Enter — новая строка · @ — файлы · PgUp/PgDn — прокрутка",
    ]
    .join("\n")
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

fn short_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && path.starts_with(&home) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let truncated: String = line.chars().take(max).collect();
    format!("{truncated}…")
}

fn file_completions(partial: &str) -> Vec<String> {
    let (dir_part, name_part) = match partial.rfind('/') {
        Some(index) => (&partial[..=index], &partial[index + 1..]),
        None => ("", partial),
    };
    let base = std::env::current_dir().unwrap_or_default().join(dir_part);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut items: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !name_part.starts_with('.') {
                return None;
            }
            if !name.starts_with(name_part) {
                return None;
            }
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let slash = if is_dir { "/" } else { "" };
            Some(format!("@{dir_part}{name}{slash}"))
        })
        .collect();
    items.sort();
    items.truncate(20);
    items
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut len = 0usize;
        for word in raw.split(' ') {
            let word_len = word.chars().count();
            if len == 0 {
                current.push_str(word);
                len = word_len;
            } else if len + 1 + word_len <= width {
                current.push(' ');
                current.push_str(word);
                len += 1 + word_len;
            } else {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
                len = word_len;
            }
            while len > width {
                let head: String = current.chars().take(width).collect();
                let tail: String = current.chars().skip(width).collect();
                out.push(head);
                current = tail;
                len = current.chars().count();
            }
        }
        out.push(current);
    }
    out
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::{
        build_edit_diff, edit_path, macro_prompt, parse_permission_label, MdBuilder, THEMES,
    };
    use ratatui::style::Modifier;
    use ratatui::text::Line;
    use runtime::PermissionMode;

    #[test]
    fn markdown_wraps_long_text_and_keeps_bold() {
        let mut builder = MdBuilder::new(THEMES[0], 20);
        let mut lines: Vec<Line<'static>> = Vec::new();
        builder.strong = 1;
        builder.text(&mut lines, "Title");
        builder.strong = 0;
        builder.flush(&mut lines);
        builder.text(
            &mut lines,
            "one two three four five six seven eight nine ten eleven",
        );
        builder.flush(&mut lines);
        assert!(lines.len() >= 2, "long paragraph must wrap");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
            "strong text stays bold"
        );
    }

    #[test]
    fn markdown_list_item_gets_bullet_prefix() {
        let mut builder = MdBuilder::new(THEMES[0], 40);
        let mut lines: Vec<Line<'static>> = Vec::new();
        builder.list.push(None);
        builder.start_item();
        builder.text(&mut lines, "item");
        builder.flush(&mut lines);
        let rendered: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.starts_with("• "), "bullet prefix: {rendered:?}");
    }

    #[test]
    fn edit_diff_shows_removed_then_added_lines() {
        let input = r#"{"path":"src/x.rs","old_string":"let a = 1;","new_string":"let a = 2;"}"#;
        let diff = build_edit_diff("edit_file", input).expect("edit_file yields a diff");
        assert!(diff.contains("-let a = 1;"));
        assert!(diff.contains("+let a = 2;"));
        assert_eq!(edit_path(input).as_deref(), Some("src/x.rs"));
    }

    #[test]
    fn write_diff_is_all_additions() {
        let input = r#"{"path":"new.txt","content":"line1\nline2"}"#;
        let diff = build_edit_diff("write_file", input).expect("write_file yields a diff");
        assert!(diff.contains("+line1"));
        assert!(diff.contains("+line2"));
        assert!(!diff.contains("-line1"));
    }

    #[test]
    fn non_file_tool_has_no_diff() {
        assert!(build_edit_diff("grep_search", r#"{"pattern":"x"}"#).is_none());
    }

    #[test]
    fn permission_labels_parse_common_aliases() {
        assert_eq!(parse_permission_label("plan"), Some(PermissionMode::ReadOnly));
        assert_eq!(parse_permission_label("write"), Some(PermissionMode::WorkspaceWrite));
        assert_eq!(parse_permission_label("yolo"), Some(PermissionMode::DangerFullAccess));
        assert!(parse_permission_label("nonsense").is_none());
    }

    #[test]
    fn macro_prompt_references_skill_and_context() {
        let p = macro_prompt("commit", Some("wip auth"));
        assert!(p.contains("commit"));
        assert!(p.contains("wip auth"));
    }
}
