//! Полноэкранный TUI-кокпит мультиплексера поверх демона: вкладки агентов
//! (по всем проектам), поток сфокусированного агента, ввод, пины. Мышь НЕ
//! захватывается — нативное выделение/копирование терминала продолжает работать;
//! навигация — клавишами.

use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::client::request;
use super::protocol::{Request, Response, SessionInfo};

struct App<'a> {
    socket: &'a Path,
    tabs: Vec<SessionInfo>,
    focused: usize,
    input: String,
    transcript: String,
    status_line: String,
}

impl<'a> App<'a> {
    fn new(socket: &'a Path) -> Self {
        Self {
            socket,
            tabs: Vec::new(),
            focused: 0,
            input: String::new(),
            transcript: String::new(),
            status_line: "Ctrl+N — новый агент".to_string(),
        }
    }

    fn focused_id(&self) -> Option<String> {
        self.tabs.get(self.focused).map(|tab| tab.id.clone())
    }

    fn refresh_tabs(&mut self) {
        if let Ok(Response::Sessions { sessions }) = request(self.socket, &Request::List) {
            self.tabs = sessions;
            if self.focused >= self.tabs.len() {
                self.focused = self.tabs.len().saturating_sub(1);
            }
        }
    }

    fn refresh_transcript(&mut self) {
        if let Some(id) = self.focused_id() {
            if let Ok(Response::Logs { text }) = request(self.socket, &Request::Logs { id }) {
                self.transcript = text;
            }
        } else {
            self.transcript.clear();
        }
    }

    fn refresh(&mut self) {
        self.refresh_tabs();
        self.refresh_transcript();
    }

    /// Возвращает `true`, если надо выйти.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') if ctrl => return true,
            KeyCode::Esc if self.input.is_empty() => return true,
            KeyCode::Char('n') if ctrl => self.new_agent(),
            KeyCode::Char('w') if ctrl => self.close_focused(),
            KeyCode::Char('p') if ctrl => self.toggle_pin(),
            KeyCode::Tab => {
                if !self.tabs.is_empty() {
                    self.focused = (self.focused + 1) % self.tabs.len();
                    self.refresh_transcript();
                }
            }
            KeyCode::BackTab => {
                if !self.tabs.is_empty() {
                    self.focused = (self.focused + self.tabs.len() - 1) % self.tabs.len();
                    self.refresh_transcript();
                }
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
        false
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        if let Some(id) = self.focused_id() {
            let _ = request(self.socket, &Request::Prompt { id, text });
            self.status_line = "отправлено".to_string();
        } else {
            self.status_line = "нет агента — Ctrl+N".to_string();
        }
    }

    fn new_agent(&mut self) {
        let cwd = std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |path| path.display().to_string());
        if let Ok(Response::Created { .. }) = request(
            self.socket,
            &Request::New {
                cwd,
                title: None,
                prompt: None,
            },
        ) {
            self.refresh_tabs();
            self.focused = self.tabs.len().saturating_sub(1);
            self.refresh_transcript();
            self.status_line = "создан агент".to_string();
        }
    }

    fn close_focused(&mut self) {
        if let Some(id) = self.focused_id() {
            let _ = request(self.socket, &Request::Close { id });
            self.refresh();
            self.status_line = "агент закрыт".to_string();
        }
    }

    fn toggle_pin(&mut self) {
        if let Some(tab) = self.tabs.get(self.focused) {
            let id = tab.id.clone();
            let pinned = !tab.pinned;
            let _ = request(self.socket, &Request::Pin { id, pinned });
            self.refresh_tabs();
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());

        self.draw_tabs(frame, layout[0]);
        self.draw_transcript(frame, layout[1]);
        self.draw_input(frame, layout[2]);
        self.draw_status(frame, layout[3]);
    }

    fn draw_tabs(&self, frame: &mut Frame, area: Rect) {
        let mut spans = Vec::new();
        if self.tabs.is_empty() {
            spans.push(Span::styled(
                " нет агентов — Ctrl+N ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        for (index, tab) in self.tabs.iter().enumerate() {
            let active = index == self.focused;
            let pin = if tab.pinned { "★" } else { "" };
            let label = format!(" {pin}{} {} ", index + 1, short(&tab.title, 16));
            let mut style = Style::default().fg(status_color(&tab.status));
            if active {
                style = style.bg(Color::Rgb(40, 44, 60)).add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_transcript(&self, frame: &mut Frame, area: Rect) {
        let title = self
            .tabs
            .get(self.focused)
            .map_or_else(|| "—".to_string(), |tab| format!(" {} · {} ", tab.title, tab.cwd));
        let viewport = area.height.saturating_sub(2) as usize;
        let lines: Vec<&str> = self.transcript.lines().collect();
        let start = lines.len().saturating_sub(viewport.max(1));
        let body = lines[start..].join("\n");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(60, 66, 84)))
            .title(Span::styled(title, Style::default().fg(Color::Rgb(140, 150, 175))));
        frame.render_widget(
            Paragraph::new(body)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(70, 80, 110)))
            .title(Span::styled(" ввод ", Style::default().fg(Color::DarkGray)));
        frame.render_widget(
            Paragraph::new(format!("> {}", self.input))
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let help = "Enter отпр · Tab вкладка · ^N новый · ^W закрыть · ^P пин · ^Q выход";
        let line = Line::from(vec![
            Span::styled(format!(" {} ", self.status_line), Style::default().fg(Color::Rgb(180, 150, 100))),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "working" => Color::Rgb(200, 165, 110),
        "waiting" => Color::Rgb(205, 145, 145),
        "exited" | "error" => Color::Rgb(120, 128, 145),
        _ => Color::Rgb(140, 175, 145),
    }
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// Запускает TUI-кокпит (поднимает демон при необходимости).
///
/// # Errors
/// Ошибки терминала/демона.
pub fn run(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(socket);
    app.refresh();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut app, &mut terminal);
    ratatui::restore();
    result
}

fn event_loop(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last_refresh = Instant::now();
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        if last_refresh.elapsed() >= Duration::from_millis(300) {
            app.refresh();
            last_refresh = Instant::now();
            dirty = true;
        }
        if event::poll(Duration::from_millis(120))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if app.on_key(key) {
                        break;
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}
