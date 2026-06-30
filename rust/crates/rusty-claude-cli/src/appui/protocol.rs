//! Сообщения между UI-потоком (ratatui) и фоновым agent-воркером приложения.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

/// Чем занят воркер прямо сейчас — для индикатора состояния в футере.
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
    Text(String),
    Thinking(String),
    ToolCall { name: String, input: String },
    ToolResult { output: String, is_error: bool },
    PermissionAsk {
        tool_name: String,
        input: String,
        reason: Option<String>,
    },
    AskUser {
        question: String,
        options: Vec<String>,
    },
    Usage { input_tokens: u32, output_tokens: u32 },
    Activity(Activity),
    TurnDone,
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
