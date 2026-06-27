//! Сообщения между UI-потоком (Slint) и фоновым agent-воркером.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

/// Чем занят воркер прямо сейчас — для индикатора состояния агента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Model,
    Tool { label: String },
    Waiting { label: String },
}

/// События от воркера к UI (применяются по мере поступления).
#[derive(Debug, Clone)]
pub enum AgentToUi {
    /// Инкрементальный кусок ответа ассистента.
    Text(String),
    /// Инкрементальный кусок «размышлений» модели.
    Thinking(String),
    /// Модель запросила вызов инструмента.
    ToolCall { name: String, input: String },
    /// Результат выполнения инструмента.
    ToolResult { output: String, is_error: bool },
    /// Требуется подтверждение перед выполнением инструмента.
    PermissionAsk {
        tool_name: String,
        input: String,
        reason: Option<String>,
    },
    /// Модель задала вопрос и ждёт ответа из UI.
    AskUser {
        question: String,
        options: Vec<String>,
    },
    /// Сводка по токенам.
    Usage { input_tokens: u32, output_tokens: u32 },
    /// Текущая активность воркера.
    Activity(Activity),
    /// Ход завершён.
    TurnDone,
    /// Ошибка хода.
    Error(String),
}

/// Команды от UI к воркеру.
#[derive(Debug, Clone)]
pub enum UiToAgent {
    Prompt(String),
}

/// Ручка для UI: каналы к воркеру и обратно + ответы на запросы + флаг отмены.
pub struct AgentHandle {
    pub to_agent: Sender<UiToAgent>,
    pub from_agent: Receiver<AgentToUi>,
    pub permission_reply: Sender<bool>,
    pub question_reply: Sender<String>,
    pub cancel: Arc<AtomicBool>,
}
