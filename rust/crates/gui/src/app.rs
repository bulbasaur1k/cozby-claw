//! egui-приложение: боковая панель (сессии + настройки), транскрипт чата со
//! стримингом, панель reasoning, карточки инструментов, апрув-модал, стоп.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use api::ProviderSlotKind;
use eframe::egui::{self, Color32, RichText};
use runtime::{ContentBlock, MessageRole, PermissionMode, Session};

use crate::agent::spawn_agent;
use crate::config::ModelConfig;
use crate::protocol::{Activity, AgentHandle, AgentToUi, SubAgentEvent, UiToAgent};
use crate::slash;

const MODES: [PermissionMode; 4] = [
    PermissionMode::ReadOnly,
    PermissionMode::WorkspaceWrite,
    PermissionMode::DangerFullAccess,
    PermissionMode::Prompt,
];

enum Entry {
    User(String),
    Assistant(String),
    Thinking(String),
    ToolCall { name: String, input: String },
    ToolResult { output: String, is_error: bool },
    Error(String),
    /// Под-агент (Agent-tool): задача + накопленные шаги + итог (`done`).
    SubAgent {
        id: u64,
        description: String,
        steps: Vec<String>,
        done: Option<bool>,
    },
}

struct PendingPermission {
    tool_name: String,
    input: String,
    reason: Option<String>,
}

/// Вопрос модели (`AskUserQuestion`), ожидающий ответа пользователя.
struct PendingQuestion {
    question: String,
    options: Vec<String>,
    answer_draft: String,
}

/// Запись сессии в боковой панели: путь к файлу + человекочитаемый заголовок
/// (первое сообщение пользователя), чтобы список не показывал «session-<ts>».
struct SessionEntry {
    path: PathBuf,
    title: String,
}

/// Редактируемая копия настроек (строки для текстовых полей).
struct SettingsDraft {
    kind: ProviderSlotKind,
    model: String,
    base_url: String,
    api_key: String,
    max_tokens: String,
    permission_mode: PermissionMode,
}

impl SettingsDraft {
    fn from_config(config: &ModelConfig) -> Self {
        Self {
            kind: config.kind,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            max_tokens: config.max_tokens.to_string(),
            permission_mode: config.permission_mode,
        }
    }
}

const PROVIDER_KINDS: [ProviderSlotKind; 2] =
    [ProviderSlotKind::Openai, ProviderSlotKind::Anthropic];

pub struct AgentApp {
    handle: AgentHandle,
    config: ModelConfig,
    settings_draft: SettingsDraft,
    sessions_dir: PathBuf,
    current_path: PathBuf,
    sessions: Vec<SessionEntry>,
    transcript: Vec<Entry>,
    live_text: String,
    live_thinking: String,
    show_thinking: bool,
    input: String,
    running: bool,
    pending: Option<PendingPermission>,
    pending_question: Option<PendingQuestion>,
    usage_in: u32,
    usage_out: u32,
    status: String,
    /// Чем воркер занят прямо сейчас (для живого индикатора активности).
    activity: Activity,
    /// Применён ли «fluid»-стиль (делаем один раз при первом кадре).
    styled: bool,
}

impl AgentApp {
    #[must_use]
    pub fn new() -> Self {
        let config = ModelConfig::load();
        let sessions_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".claw")
            .join("sessions");
        let session = Session::new();
        let current_path = session_file_path(&sessions_dir, &session.session_id);
        // Сохраняем пустую сессию сразу, чтобы она появилась в списке и не «терялась».
        let _ = session.save_to_path(&current_path);
        let handle = spawn_agent(config.clone(), session, Some(current_path.clone()));
        let mut app = Self {
            settings_draft: SettingsDraft::from_config(&config),
            handle,
            config,
            sessions_dir,
            current_path,
            sessions: Vec::new(),
            transcript: Vec::new(),
            live_text: String::new(),
            live_thinking: String::new(),
            show_thinking: true,
            input: String::new(),
            running: false,
            pending: None,
            pending_question: None,
            usage_in: 0,
            usage_out: 0,
            status: "ready".to_string(),
            activity: Activity::Idle,
            styled: false,
        };
        app.refresh_sessions();
        app
    }

    fn refresh_sessions(&mut self) {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(UNIX_EPOCH);
                let title = session_title_for(&path);
                found.push((modified, SessionEntry { path, title }));
            }
        }
        // Самые свежие сверху.
        found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        self.sessions = found.into_iter().map(|(_, entry)| entry).collect();
    }

    fn respawn(&mut self, session: Session, path: PathBuf) {
        self.handle = spawn_agent(self.config.clone(), session, Some(path.clone()));
        self.current_path = path;
        self.live_text.clear();
        self.live_thinking.clear();
        self.pending = None;
        self.pending_question = None;
        self.running = false;
        self.usage_in = 0;
        self.usage_out = 0;
        self.status = "ready".to_string();
        self.activity = Activity::Idle;
    }

    fn new_session(&mut self) {
        let session = Session::new();
        let path = session_file_path(&self.sessions_dir, &session.session_id);
        // Сразу персистим, чтобы новая сессия появилась в списке.
        let _ = session.save_to_path(&path);
        self.transcript.clear();
        self.respawn(session, path);
        self.refresh_sessions();
    }

    /// Удаляет файл сессии. Если удаляем текущую — открываем новую.
    fn delete_session(&mut self, path: &Path) {
        let _ = std::fs::remove_file(path);
        if *path == self.current_path {
            self.new_session();
        } else {
            self.refresh_sessions();
        }
    }

    fn load_session(&mut self, path: &PathBuf) {
        match Session::load_from_path(path) {
            Ok(session) => {
                self.transcript = transcript_from_session(&session);
                self.respawn(session, path.clone());
            }
            Err(error) => {
                self.transcript.push(Entry::Error(format!("load failed: {error}")));
            }
        }
    }

    fn apply_settings(&mut self) {
        self.config.kind = self.settings_draft.kind;
        self.config.model = self.settings_draft.model.trim().to_string();
        self.config.base_url = self.settings_draft.base_url.trim().to_string();
        self.config.api_key = self.settings_draft.api_key.trim().to_string();
        if let Ok(max) = self.settings_draft.max_tokens.trim().parse::<u32>() {
            self.config.max_tokens = max.max(256);
        }
        self.config.permission_mode = self.settings_draft.permission_mode;
        let saved = self.config.save();
        // Перезапускаем воркер с новым клиентом, продолжая текущую сессию с диска.
        let path = self.current_path.clone();
        let session = Session::load_from_path(&path).unwrap_or_else(|_| Session::new());
        self.respawn(session, path);
        self.status = match saved {
            Ok(()) => "settings saved".to_string(),
            Err(error) => format!("settings applied (save failed: {error})"),
        };
    }

    fn flush_live(&mut self) {
        if !self.live_thinking.is_empty() {
            self.transcript
                .push(Entry::Thinking(std::mem::take(&mut self.live_thinking)));
        }
        if !self.live_text.is_empty() {
            self.transcript
                .push(Entry::Assistant(std::mem::take(&mut self.live_text)));
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.handle.from_agent.try_recv() {
            match event {
                AgentToUi::Text(text) => self.live_text.push_str(&text),
                AgentToUi::Thinking(text) => self.live_thinking.push_str(&text),
                AgentToUi::ToolCall { name, input } => {
                    self.flush_live();
                    self.transcript.push(Entry::ToolCall { name, input });
                }
                AgentToUi::ToolResult { output, is_error } => {
                    self.transcript.push(Entry::ToolResult { output, is_error });
                }
                AgentToUi::PermissionAsk {
                    tool_name,
                    input,
                    reason,
                } => {
                    self.pending = Some(PendingPermission {
                        tool_name,
                        input,
                        reason,
                    });
                }
                AgentToUi::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    self.usage_in = self.usage_in.saturating_add(input_tokens);
                    self.usage_out = self.usage_out.saturating_add(output_tokens);
                }
                AgentToUi::TurnDone => {
                    self.flush_live();
                    self.running = false;
                    self.status = "ready".to_string();
                    self.activity = Activity::Idle;
                    self.refresh_sessions();
                }
                AgentToUi::Error(message) => {
                    self.flush_live();
                    self.transcript.push(Entry::Error(message));
                    self.running = false;
                    self.status = "error".to_string();
                    self.activity = Activity::Idle;
                }
                AgentToUi::Activity(activity) => self.activity = activity,
                AgentToUi::SubAgent { id, event } => self.apply_subagent_event(id, event),
                AgentToUi::AskUser { question, options } => {
                    self.pending_question = Some(PendingQuestion {
                        question,
                        options,
                        answer_draft: String::new(),
                    });
                }
            }
        }
    }

    /// Применяет событие под-агента: создаёт/обновляет соответствующую запись
    /// транскрипта по `id` (стартовое сообщение фиксирует текст до live-блока).
    fn apply_subagent_event(&mut self, id: u64, event: SubAgentEvent) {
        match event {
            SubAgentEvent::Started { description } => {
                self.flush_live();
                self.transcript.push(Entry::SubAgent {
                    id,
                    description,
                    steps: Vec::new(),
                    done: None,
                });
            }
            SubAgentEvent::Step(line) => {
                if let Some(Entry::SubAgent { steps, .. }) = self.find_subagent(id) {
                    steps.push(line);
                }
            }
            SubAgentEvent::Finished { ok } => {
                if let Some(Entry::SubAgent { done, .. }) = self.find_subagent(id) {
                    *done = Some(ok);
                }
            }
        }
    }

    fn find_subagent(&mut self, id: u64) -> Option<&mut Entry> {
        self.transcript.iter_mut().rev().find(
            |entry| matches!(entry, Entry::SubAgent { id: entry_id, .. } if *entry_id == id),
        )
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.running {
            return;
        }
        // Слэш-команды исполняются локально, не уходят в модель.
        if let Some(command) = slash::parse(&text) {
            self.input.clear();
            self.handle_command(command);
            return;
        }
        self.transcript.push(Entry::User(text.clone()));
        if self.handle.to_agent.send(UiToAgent::Prompt(text)).is_ok() {
            self.running = true;
            self.status = "thinking…".to_string();
        } else {
            self.status = "agent thread is gone".to_string();
        }
        self.input.clear();
    }

    fn handle_command(&mut self, command: slash::GuiCommand) {
        use slash::GuiCommand;
        match command {
            GuiCommand::Help => self.transcript.push(Entry::Assistant(slash::help_text())),
            GuiCommand::Clear => {
                self.transcript.clear();
                self.live_text.clear();
                self.live_thinking.clear();
                self.status = "cleared".to_string();
            }
            GuiCommand::New => self.new_session(),
            GuiCommand::Cost => self.transcript.push(Entry::Assistant(format!(
                "Tokens — input {}, output {} (this session).",
                self.usage_in, self.usage_out
            ))),
            GuiCommand::Status => {
                let text = self.status_report();
                self.transcript.push(Entry::Assistant(text));
            }
            GuiCommand::Diff => {
                let text = run_git(&["diff", "--stat"]);
                self.transcript.push(Entry::Assistant(text));
            }
            GuiCommand::Model(model) => self.switch_model(&model),
            GuiCommand::Export(path) => {
                let text = self.export_transcript(path.as_deref());
                self.transcript.push(Entry::Assistant(text));
            }
            GuiCommand::Unknown(name) => self.transcript.push(Entry::Error(format!(
                "Unknown command /{name}. Type /help for the list."
            ))),
        }
    }

    /// Сводка состояния для `/status`.
    fn status_report(&self) -> String {
        let cwd = std::env::current_dir().map_or_else(
            |_| "<unknown>".to_string(),
            |path| path.display().to_string(),
        );
        let session = self
            .current_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("session");
        format!(
            "Status\n  model        {}\n  provider     {}\n  directory    {cwd}\n  session      {session}\n  permissions  {}\n  tokens       in {} / out {}",
            self.config.model,
            self.config.kind.as_str(),
            self.config.permission_mode.as_str(),
            self.usage_in,
            self.usage_out,
        )
    }

    /// Смена основной модели: обновляет конфиг, сохраняет, перезапускает воркер
    /// на текущей сессии (с диска).
    fn switch_model(&mut self, model: &str) {
        if model.trim().is_empty() {
            self.transcript
                .push(Entry::Error("Usage: /model <model-id>".to_string()));
            return;
        }
        self.config.model = model.trim().to_string();
        self.settings_draft.model = self.config.model.clone();
        let _ = self.config.save();
        let path = self.current_path.clone();
        let session = Session::load_from_path(&path).unwrap_or_else(|_| Session::new());
        self.respawn(session, path);
        self.transcript
            .push(Entry::Assistant(format!("Model switched to {model}.")));
    }

    /// Экспорт транскрипта в markdown-файл.
    fn export_transcript(&self, path: Option<&str>) -> String {
        let target = path.map_or_else(
            || {
                self.sessions_dir
                    .join(format!("export-{}.md", self.current_path.file_stem().and_then(|s| s.to_str()).unwrap_or("session")))
            },
            PathBuf::from,
        );
        let body = transcript_to_markdown(&self.transcript);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&target, body) {
            Ok(()) => format!("Exported transcript to {}", target.display()),
            Err(error) => format!("Export failed: {error}"),
        }
    }

    fn stop(&mut self) {
        self.handle.cancel.store(true, Ordering::SeqCst);
        // Если ждём ответа на вопрос — разблокируем воркер пустым ответом.
        if self.pending_question.take().is_some() {
            let _ = self.handle.question_reply.send(String::new());
        }
        self.status = "stopping…".to_string();
    }

    /// Отправляет ответ на вопрос модели и закрывает модал.
    fn answer_question(&mut self, answer: String) {
        let _ = self.handle.question_reply.send(answer);
        self.pending_question = None;
    }
}

impl Default for AgentApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for AgentApp {
    #[allow(clippy::too_many_lines)]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.styled {
            apply_fluid_style(ctx);
            self.styled = true;
        }
        self.drain_events();

        self.render_sidebar(ctx);
        self.render_header(ctx);
        self.render_composer(ctx);
        self.render_transcript(ctx);
        self.render_permission_modal(ctx);
        self.render_question_modal(ctx);

        if self.running {
            ctx.request_repaint();
        }
    }
}

impl AgentApp {
    #[allow(clippy::too_many_lines)]
    fn render_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(240.0)
            .show(ctx, |ui| {
                if ui
                    .add_enabled(!self.running, egui::Button::new("➕  New session"))
                    .clicked()
                {
                    self.new_session();
                }
                ui.separator();
                ui.label(RichText::new("Sessions").strong());
                let current = self.current_path.clone();
                let running = self.running;
                let mut to_load: Option<PathBuf> = None;
                let mut to_delete: Option<PathBuf> = None;
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for entry in &self.sessions {
                            let selected = entry.path == current;
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        !running,
                                        egui::SelectableLabel::new(selected, &entry.title),
                                    )
                                    .clicked()
                                {
                                    to_load = Some(entry.path.clone());
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !running,
                                                egui::Button::new("🗑").frame(false),
                                            )
                                            .on_hover_text("Delete session")
                                            .clicked()
                                        {
                                            to_delete = Some(entry.path.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
                if let Some(path) = to_delete {
                    self.delete_session(&path);
                } else if let Some(path) = to_load {
                    self.load_session(&path);
                }

                ui.separator();
                egui::CollapsingHeader::new("⚙  Settings")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label("Provider");
                        egui::ComboBox::from_id_salt("provider_kind")
                            .selected_text(self.settings_draft.kind.as_str())
                            .show_ui(ui, |ui| {
                                for kind in PROVIDER_KINDS {
                                    ui.selectable_value(
                                        &mut self.settings_draft.kind,
                                        kind,
                                        kind.as_str(),
                                    );
                                }
                            });
                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.settings_draft.model);
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.settings_draft.base_url);
                        ui.label("API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_draft.api_key)
                                .password(true),
                        );
                        ui.label("Max tokens");
                        ui.text_edit_singleline(&mut self.settings_draft.max_tokens);
                        ui.label("Permissions");
                        egui::ComboBox::from_id_salt("perm")
                            .selected_text(self.settings_draft.permission_mode.as_str())
                            .show_ui(ui, |ui| {
                                for mode in MODES {
                                    ui.selectable_value(
                                        &mut self.settings_draft.permission_mode,
                                        mode,
                                        mode.as_str(),
                                    );
                                }
                            });
                        ui.add_space(6.0);
                        if ui
                            .add_enabled(!self.running, egui::Button::new("Save & reconnect"))
                            .clicked()
                        {
                            self.apply_settings();
                        }
                        ui.weak(
                            RichText::new(format!(
                                "stored at {}",
                                ModelConfig::config_path().display()
                            ))
                            .size(10.0),
                        );
                    });
            });
    }

    fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("claw");
                ui.separator();
                ui.label(format!("model: {}", self.config.model));
                ui.separator();
                ui.label(format!("status: {}", self.status));
                ui.separator();
                ui.label(format!("tokens in {} / out {}", self.usage_in, self.usage_out));
                ui.separator();
                ui.checkbox(&mut self.show_thinking, "reasoning");
            });
            // Живой индикатор «что приложение делает сейчас» (как в claude code).
            if self.running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let (label, color) = activity_badge(&self.activity);
                    ui.colored_label(color, label);
                });
            }
        });
    }

    fn render_composer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.add_enabled(
                !self.running,
                egui::TextEdit::multiline(&mut self.input)
                    .desired_rows(2)
                    .hint_text("Ask claw…  (Ctrl+Enter to send · /help for commands)")
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if self.running {
                    if ui.button("⏹  Stop").clicked() {
                        self.stop();
                    }
                } else if ui.button("Send").clicked() {
                    self.submit();
                }
                ui.weak("Ctrl+Enter to send");
            });
            ui.add_space(4.0);
        });

        if !self.running && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            self.submit();
        }
    }

    fn render_transcript(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for entry in &self.transcript {
                        render_entry(ui, entry, self.show_thinking);
                    }
                    if self.show_thinking && !self.live_thinking.is_empty() {
                        render_thinking(ui, &self.live_thinking);
                    }
                    if !self.live_text.is_empty() {
                        render_assistant(ui, &self.live_text);
                    }
                });
        });
    }

    fn render_permission_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending else {
            return;
        };
        let tool_name = pending.tool_name.clone();
        let input = pending.input.clone();
        let reason = pending.reason.clone();
        let mut decision: Option<bool> = None;
        egui::Window::new("Approve tool call")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Tool: {tool_name}"));
                if let Some(reason) = &reason {
                    ui.label(format!("Reason: {reason}"));
                }
                ui.separator();
                ui.label("Input:");
                ui.monospace(truncate(&input, 2000));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Allow").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Deny").clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(allow) = decision {
            let _ = self.handle.permission_reply.send(allow);
            self.pending = None;
        }
    }

    fn render_question_modal(&mut self, ctx: &egui::Context) {
        let Some(question) = &self.pending_question else {
            return;
        };
        let prompt = question.question.clone();
        let options = question.options.clone();
        // Решение пользователя: Some(answer) → отправить, пустая строка = пропуск.
        let mut answer: Option<String> = None;
        egui::Window::new("claw asks")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(RichText::new(prompt).strong());
                ui.separator();
                for option in &options {
                    if ui.button(option).clicked() {
                        answer = Some(option.clone());
                    }
                }
                if !options.is_empty() {
                    ui.separator();
                    ui.label("…or type your own answer:");
                }
                if let Some(pending) = self.pending_question.as_mut() {
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut pending.answer_draft)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("Type an answer, then Send"),
                    );
                    let submit = response.lost_focus()
                        && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("Send").clicked() || submit {
                            answer = Some(pending.answer_draft.trim().to_string());
                        }
                        if ui.button("Skip").clicked() {
                            answer = Some(String::new());
                        }
                    });
                }
            });
        if let Some(answer) = answer {
            self.answer_question(answer);
        }
    }
}

/// Применяет «fluid»-стиль: мягкая тёмная палитра, скруглённые углы, очень
/// лёгкие тени и больше воздуха в отступах. Цвета текстовых меток («you»/«claw»/
/// инструменты) не переопределяются — они задаются отдельно в `colored_label`.
fn apply_fluid_style(ctx: &egui::Context) {
    use egui::epaint::Shadow;
    use egui::{Rounding, Stroke, Vec2};

    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    v.dark_mode = true;

    // Мягкая тёмная палитра с лёгкой глубиной между панелью и окнами.
    v.panel_fill = Color32::from_rgb(21, 23, 28);
    v.window_fill = Color32::from_rgb(27, 29, 35);
    v.extreme_bg_color = Color32::from_rgb(16, 17, 21);
    v.faint_bg_color = Color32::from_rgb(31, 33, 39);

    // Скругления окон/меню и виджетов.
    v.window_rounding = Rounding::same(12.0);
    v.menu_rounding = Rounding::same(10.0);
    let widget_rounding = Rounding::same(8.0);
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.rounding = widget_rounding;
    }

    // Очень лёгкие тени — едва заметная глубина, без «тяжёлого» затемнения.
    v.window_shadow = Shadow {
        offset: Vec2::new(0.0, 6.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(40),
    };
    v.popup_shadow = Shadow {
        offset: Vec2::new(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(30),
    };

    // Акцент и выделение.
    let accent = Color32::from_rgb(120, 170, 255);
    v.selection.bg_fill = accent.linear_multiply(0.32);
    v.selection.stroke = Stroke::new(1.0, accent);
    v.hyperlink_color = accent;

    // Заливки/границы виджетов и мягкий цвет текста по умолчанию.
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(44, 47, 55));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(220, 223, 230));
    v.widgets.inactive.bg_fill = Color32::from_rgb(38, 41, 49);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(33, 36, 43);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(208, 212, 220));
    v.widgets.hovered.bg_fill = Color32::from_rgb(50, 54, 63);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent.linear_multiply(0.6));
    v.widgets.active.bg_fill = accent.linear_multiply(0.55);

    // Больше воздуха.
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.interact_size.y = 28.0;

    ctx.set_style(style);
}

/// Путь к файлу сессии по её id: `<dir>/<session_id>.jsonl`.
fn session_file_path(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(format!("{session_id}.jsonl"))
}

/// Заголовок сессии для списка: первое сообщение пользователя (обрезанное),
/// иначе «New chat» (для пустой/нечитаемой сессии).
fn session_title_for(path: &Path) -> String {
    Session::load_from_path(path)
        .ok()
        .and_then(|session| first_user_text(&session))
        .map_or_else(|| "New chat".to_string(), |text| compact_title(&text, 42))
}

/// Текст первого пользовательского сообщения сессии, если есть.
fn first_user_text(session: &Session) -> Option<String> {
    session.messages.iter().find_map(|message| {
        if !matches!(message.role, MessageRole::User) {
            return None;
        }
        message.blocks.iter().find_map(|block| match block {
            ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    })
}

/// Однострочный заголовок, обрезанный до `max` символов.
fn compact_title(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(max).collect::<String>())
    }
}

fn transcript_from_session(session: &Session) -> Vec<Entry> {
    let mut out = Vec::new();
    for message in &session.messages {
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => match message.role {
                    MessageRole::User => out.push(Entry::User(text.clone())),
                    MessageRole::Assistant => out.push(Entry::Assistant(text.clone())),
                    MessageRole::System | MessageRole::Tool => {}
                },
                ContentBlock::ToolUse { name, input, .. } => out.push(Entry::ToolCall {
                    name: name.clone(),
                    input: input.clone(),
                }),
                ContentBlock::ToolResult {
                    output, is_error, ..
                } => out.push(Entry::ToolResult {
                    output: output.clone(),
                    is_error: *is_error,
                }),
            }
        }
    }
    out
}

fn render_entry(ui: &mut egui::Ui, entry: &Entry, show_thinking: bool) {
    match entry {
        Entry::User(text) => {
            ui.colored_label(Color32::from_rgb(120, 170, 255), "you");
            render_markdown(ui, text);
            ui.add_space(6.0);
        }
        Entry::Assistant(text) => render_assistant(ui, text),
        Entry::Thinking(text) => {
            if show_thinking {
                render_thinking(ui, text);
            }
        }
        Entry::ToolCall { name, input } => {
            // Компактная строка вместо сырого JSON: имя + ключевой аргумент.
            let summary = summarize_tool_input(input);
            let label = if summary.is_empty() {
                format!("⚙ {name}")
            } else {
                format!("⚙ {name} · {summary}")
            };
            ui.colored_label(Color32::from_rgb(200, 160, 90), label);
            ui.add_space(4.0);
        }
        Entry::ToolResult { output, is_error } => {
            let color = if *is_error {
                Color32::from_rgb(230, 110, 110)
            } else {
                Color32::from_rgb(120, 190, 120)
            };
            ui.colored_label(color, if *is_error { "tool error" } else { "tool result" });
            ui.monospace(truncate(output, 4000));
            ui.add_space(6.0);
        }
        Entry::Error(message) => {
            ui.colored_label(Color32::from_rgb(230, 110, 110), format!("error: {message}"));
            ui.add_space(6.0);
        }
        Entry::SubAgent {
            description,
            steps,
            done,
            ..
        } => render_subagent(ui, description, steps, *done),
    }
}

/// Вложенный блок под-агента: задача, шаги и итоговый статус.
fn render_subagent(ui: &mut egui::Ui, description: &str, steps: &[String], done: Option<bool>) {
    let header = match done {
        None => "⤷ sub-agent (running…)".to_string(),
        Some(true) => "⤷ sub-agent ✓".to_string(),
        Some(false) => "⤷ sub-agent ✘".to_string(),
    };
    let color = match done {
        None => Color32::from_rgb(160, 160, 220),
        Some(true) => Color32::from_rgb(120, 190, 120),
        Some(false) => Color32::from_rgb(230, 110, 110),
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.colored_label(color, header);
        ui.weak(truncate(description, 400));
        for step in steps {
            ui.monospace(truncate(step, 200));
        }
    });
    ui.add_space(6.0);
}

/// Запускает `git <args>` в текущем каталоге и возвращает вывод для транскрипта.
fn run_git(args: &[&str]) -> String {
    match std::process::Command::new("git").args(args).output() {
        Ok(output) => {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    "(no changes)".to_string()
                } else {
                    trimmed.to_string()
                }
            } else {
                format!("git error: {}", String::from_utf8_lossy(&output.stderr).trim())
            }
        }
        Err(error) => format!("failed to run git: {error}"),
    }
}

/// Рендерит транскрипт в markdown (для `/export`). Чистая функция — под тесты.
fn transcript_to_markdown(transcript: &[Entry]) -> String {
    use std::fmt::Write;
    let mut out = String::from("# claw transcript\n\n");
    for entry in transcript {
        // `write!` в String не падает, поэтому результат игнорируем.
        let _ = match entry {
            Entry::User(text) => write!(out, "## You\n\n{text}\n\n"),
            Entry::Assistant(text) => write!(out, "## claw\n\n{text}\n\n"),
            Entry::Thinking(text) => write!(out, "> reasoning: {text}\n\n"),
            Entry::ToolCall { name, input } => {
                write!(out, "**tool** `{name}` — {}\n\n", summarize_tool_input(input))
            }
            Entry::ToolResult { output, is_error } => {
                let tag = if *is_error { "tool error" } else { "tool result" };
                write!(out, "```\n{tag}: {}\n```\n\n", truncate(output, 4000))
            }
            Entry::Error(message) => write!(out, "**error:** {message}\n\n"),
            Entry::SubAgent {
                description,
                steps,
                done,
                ..
            } => {
                let _ = write!(out, "### sub-agent: {description}\n\n");
                for step in steps {
                    let _ = writeln!(out, "- {step}");
                }
                if let Some(ok) = done {
                    let _ = write!(out, "\n_{}_\n\n", if *ok { "done" } else { "failed" });
                }
                Ok(())
            }
        };
    }
    out
}

/// Компактная сводка входа инструмента: вытаскивает ключевой аргумент
/// (path/command/query/…) и обрезает до одной строки — вместо сырого JSON.
fn summarize_tool_input(input: &str) -> String {
    let picked = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| {
            [
                "path",
                "file_path",
                "command",
                "pattern",
                "query",
                "url",
                "description",
                "prompt",
            ]
            .iter()
            .find_map(|key| {
                value
                    .get(*key)
                    .and_then(|field| field.as_str())
                    .map(str::to_string)
            })
        });
    let text = picked.unwrap_or_else(|| input.to_string());
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= 160 {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(160).collect::<String>())
    }
}

/// Подпись и цвет для индикатора текущей активности.
fn activity_badge(activity: &Activity) -> (String, Color32) {
    match activity {
        Activity::Idle => ("idle".to_string(), Color32::from_gray(150)),
        Activity::Model => ("LLM request…".to_string(), Color32::from_rgb(150, 220, 150)),
        Activity::Tool { label } => (
            format!("running: {label}"),
            Color32::from_rgb(200, 160, 90),
        ),
        Activity::SubAgent { label } => (
            format!("sub-agent: {label}"),
            Color32::from_rgb(160, 160, 220),
        ),
    }
}

fn render_assistant(ui: &mut egui::Ui, text: &str) {
    ui.colored_label(Color32::from_rgb(150, 220, 150), "claw");
    render_markdown(ui, text);
    ui.add_space(6.0);
}

fn render_thinking(ui: &mut egui::Ui, text: &str) {
    ui.colored_label(Color32::from_gray(150), "reasoning");
    ui.weak(text);
    ui.add_space(4.0);
}

/// Лёгкий markdown: разделяем по fenced-блокам, код — в моноширинном фрейме.
fn render_markdown(ui: &mut egui::Ui, text: &str) {
    for (index, segment) in text.split("```").enumerate() {
        if index % 2 == 0 {
            let trimmed = segment.trim_matches('\n');
            if !trimmed.is_empty() {
                ui.label(trimmed);
            }
        } else {
            // Внутри блока: возможная первая строка — язык, отбрасываем её.
            let body = match segment.split_once('\n') {
                Some((lang, rest)) if !lang.trim().is_empty() && !lang.contains(char::is_whitespace) => {
                    rest
                }
                _ => segment,
            };
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(body.trim_end_matches('\n')).monospace())
                        .wrap(),
                );
            });
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… [{} more bytes]", &text[..cut], text.len() - cut)
}

#[cfg(test)]
mod tests {
    use super::{
        activity_badge, compact_title, summarize_tool_input, transcript_to_markdown, Entry,
    };
    use crate::protocol::Activity;

    #[test]
    fn transcript_export_renders_each_role() {
        let transcript = vec![
            Entry::User("hi".to_string()),
            Entry::Assistant("hello".to_string()),
            Entry::ToolCall {
                name: "read_file".to_string(),
                input: r#"{"path":"a.rs"}"#.to_string(),
            },
            Entry::ToolResult {
                output: "ok".to_string(),
                is_error: false,
            },
            Entry::Error("boom".to_string()),
            Entry::SubAgent {
                id: 1,
                description: "count files".to_string(),
                steps: vec!["⚙ bash".to_string()],
                done: Some(true),
            },
        ];
        let md = transcript_to_markdown(&transcript);
        assert!(md.contains("## You\n\nhi"));
        assert!(md.contains("## claw\n\nhello"));
        assert!(md.contains("**tool** `read_file` — a.rs"));
        assert!(md.contains("tool result: ok"));
        assert!(md.contains("**error:** boom"));
        assert!(md.contains("### sub-agent: count files"));
        assert!(md.contains("- ⚙ bash"));
        assert!(md.contains("_done_"));
    }

    #[test]
    fn summarize_picks_key_field_or_first_line() {
        assert_eq!(summarize_tool_input(r#"{"path":"src/main.rs"}"#), "src/main.rs");
        assert_eq!(summarize_tool_input(r#"{"command":"ls -la"}"#), "ls -la");
        assert_eq!(summarize_tool_input("raw text\nsecond line"), "raw text");
    }

    #[test]
    fn compact_title_trims_and_truncates() {
        assert_eq!(compact_title("  short  ", 40), "short");
        let long = "a".repeat(60);
        let title = compact_title(&long, 10);
        assert_eq!(title.chars().count(), 11, "10 chars + ellipsis");
        assert!(title.ends_with('…'));
    }

    #[test]
    fn activity_badges_have_human_labels() {
        assert_eq!(activity_badge(&Activity::Idle).0, "idle");
        assert_eq!(activity_badge(&Activity::Model).0, "LLM request…");
        assert!(activity_badge(&Activity::Tool {
            label: "shell".to_string()
        })
        .0
        .contains("shell"));
        assert!(activity_badge(&Activity::SubAgent {
            label: "task".to_string()
        })
        .0
        .contains("sub-agent"));
    }
}
