//! Сообщения между UI-потоком (egui) и фоновым agent-воркером.
//!
//! Два независимых канала + отдельный канал ответа на запрос разрешения
//! (воркер блокируется в `run_turn`, поэтому UI отвечает прямо в prompter).

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

/// События от воркера к UI (рисуются по мере поступления).
#[derive(Debug, Clone)]
pub enum AgentToUi {
    /// Инкрементальный кусок ответа ассистента.
    Text(String),
    /// Инкрементальный кусок «размышлений» (reasoning) модели.
    Thinking(String),
    /// Модель запросила вызов инструмента (вход уже собран целиком).
    ToolCall { name: String, input: String },
    /// Результат выполнения инструмента.
    ToolResult { output: String, is_error: bool },
    /// Требуется подтверждение пользователя перед выполнением инструмента.
    PermissionAsk {
        tool_name: String,
        input: String,
        reason: Option<String>,
    },
    /// Сводка по токенам после ответа модели.
    Usage { input_tokens: u32, output_tokens: u32 },
    /// Ход завершён (можно слать следующий запрос).
    TurnDone,
    /// Ошибка хода.
    Error(String),
    /// Текущая активность воркера — для индикатора «что приложение делает сейчас».
    Activity(Activity),
    /// Событие под-агента (сабагента), запущенного инструментом `Agent`.
    SubAgent { id: u64, event: SubAgentEvent },
    /// Модель задала вопрос (`AskUserQuestion`) и ждёт ответа из UI.
    AskUser {
        question: String,
        options: Vec<String>,
    },
}

/// Чем занят воркер прямо сейчас (рисуется строкой статуса со спиннером).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Ничего не выполняется (ход завершён/не начат).
    Idle,
    /// Идёт запрос к модели либо стриминг её ответа.
    Model,
    /// Выполняется инструмент; `label` — человекочитаемая категория.
    Tool { label: String },
    /// Работает под-агент (сабагент).
    SubAgent { label: String },
}

/// Жизненный цикл под-агента, отображаемый вложенным блоком в транскрипте.
#[derive(Debug, Clone)]
pub enum SubAgentEvent {
    /// Под-агент запущен с задачей `description`.
    Started { description: String },
    /// Готовый к показу шаг под-агента (вызов инструмента или фрагмент текста).
    Step(String),
    /// Под-агент завершился; `ok=false` — с ошибкой/отменён.
    Finished { ok: bool },
}

/// Команды от UI к воркеру.
#[derive(Debug, Clone)]
pub enum UiToAgent {
    /// Отправить запрос пользователя в агент.
    Prompt(String),
}

/// Ручка для UI: каналы к воркеру и обратно + ответ на запрос разрешения.
pub struct AgentHandle {
    pub to_agent: Sender<UiToAgent>,
    pub from_agent: Receiver<AgentToUi>,
    /// `true` — разрешить вызов инструмента, `false` — отклонить.
    pub permission_reply: Sender<bool>,
    /// Ответ пользователя на `AskUser` (пустая строка — пропустить вопрос).
    pub question_reply: Sender<String>,
    /// Кооперативная отмена текущего хода: UI ставит `true`, клиент/исполнитель
    /// инструментов проверяют флаг и сворачивают ход.
    pub cancel: Arc<AtomicBool>,
}
