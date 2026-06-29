//! Протокол мультиплексера: построчный JSON по unix-сокету между клиентом (CLI)
//! и фоновым сервером, который держит агентов по разным проектам.

use serde::{Deserialize, Serialize};

/// Запрос клиента к серверу (одна строка JSON на соединение).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Проверка живости сервера.
    Ping,
    /// Завести сессию-агента в рабочей директории `cwd`.
    New {
        cwd: String,
        title: Option<String>,
    },
    /// Список всех сессий со статусами.
    List,
    /// Закрыть сессию по id.
    Close { id: String },
    /// Остановить сервер (когда сессий не осталось/по запросу).
    Shutdown,
}

/// Ответ сервера (одна строка JSON).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Created { id: String },
    Sessions { sessions: Vec<SessionInfo> },
    Error { message: String },
}

/// Снимок одной сессии для `ls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// idle | working | waiting | done | error
    pub status: String,
    pub msgs: usize,
}
