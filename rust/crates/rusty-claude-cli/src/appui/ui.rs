//! Полноэкранное интерактивное приложение (ratatui) — основной интерфейс
//! `cozby-claw-cli`: вкладки-сессии (параллельные агенты в процессе), история со
//! стримингом, многострочный ввод с автодополнением, футер, подтверждения, темы.
//! Мышь НЕ захватывается — нативное выделение/копирование терминала работает.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use runtime::{PermissionMode, Session};
use tui_textarea::{CursorMove, TextArea};

use super::icons;
use super::protocol::{Activity, AgentHandle, AgentToUi, UiToAgent};
use super::worker;

const COMMANDS: &[&str] = &[
    "/memory", "/diff", "/config", "/theme", "/clear", "/help", "/quit",
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
}

impl Tab {
    fn new(id: usize, model: &str, mode: PermissionMode) -> Self {
        let session = Session::new();
        let save_path = sessions_path(&session.session_id);
        let handle = worker::spawn_agent(model.to_string(), mode, session, save_path);
        Self {
            id,
            title: format!("Сессия {id}"),
            handle,
            entries: Vec::new(),
            activity: Activity::Idle,
            running: false,
            modal: Modal::None,
            answer: String::new(),
            in_tok: 0,
            out_tok: 0,
            turn_start: None,
            scroll_back: 0,
            pinned: false,
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
                self.push(Role::Tool, format!("{name}  {}", first_line(&input, 200)));
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
}

/// Запускает приложение.
///
/// # Errors
/// Ошибки терминала/ввода-вывода.
pub fn run(model: String, mode: PermissionMode) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let mut app = App::new(model, mode);
    let result = app.event_loop(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
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
        let first = Tab::new(1, &model, mode);
        let mut app = Self {
            theme,
            model,
            mode,
            cwd,
            branch: git_branch(),
            tabs: vec![first],
            active: 0,
            next_id: 2,
            input,
            compl: None,
            frame_tick: 0,
            sidebar_collapsed: false,
            should_quit: false,
        };
        app.active_mut().push(
            Role::System,
            "cozby-claw. /help — команды, Ctrl+N — вкладка, Ctrl+F/Ctrl+B — переключить, \
             Ctrl+E — свернуть сайдбар, Ctrl+T — тема."
                .to_string(),
        );
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

    // --- клавиши ------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        if !matches!(self.active().modal, Modal::None) {
            self.on_modal_key(key);
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
                if self.active().running {
                    self.cancel_turn();
                } else {
                    self.reset_input();
                }
            }
            KeyCode::PageUp => {
                let s = &mut self.active_mut().scroll_back;
                *s = s.saturating_add(8);
            }
            KeyCode::PageDown => {
                let s = &mut self.active_mut().scroll_back;
                *s = s.saturating_sub(8);
            }
            KeyCode::Enter if alt || shift => self.input.insert_newline(),
            KeyCode::Enter => self.submit(),
            _ => {
                self.input.input(key);
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
        self.tabs.push(Tab::new(id, &self.model, self.mode));
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(self.active);
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
        let mut parts = command.split_whitespace();
        let name = parts.next().unwrap_or_default();
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
                self.theme = parts
                    .next()
                    .map_or_else(|| next_theme(self.theme), theme_by_name);
                configure_input(&mut self.input, self.theme);
                let name = self.theme.name.to_string();
                self.active_mut().push(Role::System, format!("тема: {name}"));
                true
            }
            "memory" => {
                self.run_report(crate::render_memory_report());
                true
            }
            "diff" => {
                self.run_report(crate::render_diff_report());
                true
            }
            "config" => {
                self.run_report(crate::render_config_report(parts.next()));
                true
            }
            "help" => {
                self.active_mut().push(
                    Role::System,
                    "Команды: /memory /diff /config [секция] /clear /theme [name] /quit.  \
                     Вкладки: Ctrl+N новая · Ctrl+F/Ctrl+B переключить · Ctrl+W закрыть · \
                     Ctrl+P пин · Ctrl+E свернуть сайдбар.  Enter — отправить, Alt+Enter — строка."
                        .to_string(),
                );
                true
            }
            _ => false,
        }
    }

    fn run_report(&mut self, result: Result<String, Box<dyn std::error::Error>>) {
        match result {
            Ok(text) => self.active_mut().push(Role::System, text),
            Err(error) => self.active_mut().push(Role::Error, error.to_string()),
        }
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
                let pin = if tab.pinned { icons::PIN } else { "" };
                let title = short(&tab.title, inner.saturating_sub(6));
                let mark = if active { icons::ACTIVE } else { " " };
                let mut label_style = Style::default().fg(if active { theme.text } else { theme.muted });
                if active {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                rows.push(Line::from(vec![
                    Span::styled(format!("{mark}"), Style::default().fg(theme.accent)),
                    Span::styled(format!(" {} ", icons::DOT), Style::default().fg(color)),
                    Span::styled(format!("{}{pin} {title}", index + 1), label_style),
                ]));
            }
        }
        let title = if collapsed { "" } else { " сессии " };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.muted))
            .title(Span::styled(title, Style::default().fg(theme.accent2)));
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
            let (prefix, color, dim) = match entry.role {
                Role::User => ("▍ ", theme.accent, false),
                Role::Assistant => ("", theme.text, false),
                Role::Thinking => ("· ", theme.muted, true),
                Role::Tool => ("▹ ", theme.accent2, true),
                Role::ToolResult => ("", theme.success, true),
                Role::Error => ("✗ ", theme.error, false),
                Role::System => ("» ", theme.muted, true),
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
            if matches!(entry.role, Role::User | Role::Assistant) {
                lines.push(Line::from(""));
            }
        }
        lines
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
            " /memory · /diff · /config · /theme · /clear · /help · /quit "
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
            self.model,
            tab.in_tok / 1000,
            tab.out_tok / 1000,
        );
        let line = Line::from(vec![
            Span::styled(left, Style::default().fg(color)),
            Span::styled(
                format!("   {}", short_path(&self.cwd)),
                Style::default().fg(theme.muted),
            ),
        ]);
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
