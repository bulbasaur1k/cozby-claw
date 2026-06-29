//! Фоновый сервер мультиплексера: держит реестр сессий-агентов (по разным
//! проектам) и отвечает клиентам по unix-сокету. Каждое соединение —
//! один запрос/ответ; состояние общее под `Mutex`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use super::protocol::{Request, Response, SessionInfo};

/// Одна сессия-агент в реестре (Этап 1 — метаданные; исполнение агента далее).
struct Session {
    id: String,
    title: String,
    cwd: String,
    status: String,
    msgs: usize,
}

#[derive(Default)]
struct Mux {
    sessions: Vec<Session>,
    counter: usize,
}

impl Mux {
    fn new_session(&mut self, cwd: String, title: Option<String>) -> String {
        self.counter += 1;
        let id = format!("s{}", self.counter);
        let title = title.unwrap_or_else(|| default_title(&cwd));
        self.sessions.push(Session {
            id: id.clone(),
            title,
            cwd,
            status: "idle".to_string(),
            msgs: 0,
        });
        id
    }

    fn snapshot(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|session| SessionInfo {
                id: session.id.clone(),
                title: session.title.clone(),
                cwd: session.cwd.clone(),
                status: session.status.clone(),
                msgs: session.msgs,
            })
            .collect()
    }
}

/// Заголовок по умолчанию — имя последнего компонента пути.
fn default_title(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(cwd)
        .to_string()
}

/// Запускает сервер на `socket_path` (блокирующе). Удаляет устаревший сокет.
///
/// # Errors
/// Ошибки привязки сокета / приёма соединений.
pub fn serve(socket_path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    let state = Arc::new(Mutex::new(Mux::default()));
    let shutdown = Arc::new(AtomicBool::new(false));

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let conn_state = Arc::clone(&state);
        let conn_shutdown = Arc::clone(&shutdown);
        thread::spawn(move || handle_conn(&stream, &conn_state, &conn_shutdown));
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

fn handle_conn(stream: &UnixStream, state: &Mutex<Mux>, shutdown: &AtomicBool) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(request) => process(request, state, shutdown),
        Err(error) => Response::Error {
            message: format!("bad request: {error}"),
        },
    };
    let mut writer = stream;
    let payload = serde_json::to_string(&response).unwrap_or_else(|_| {
        "{\"resp\":\"error\",\"message\":\"serialize failed\"}".to_string()
    });
    let _ = writeln!(writer, "{payload}");
    let _ = writer.flush();
}

fn process(request: Request, state: &Mutex<Mux>, shutdown: &AtomicBool) -> Response {
    match request {
        Request::Ping => Response::Ok,
        Request::New { cwd, title } => {
            let id = state.lock().expect("mux lock").new_session(cwd, title);
            Response::Created { id }
        }
        Request::List => Response::Sessions {
            sessions: state.lock().expect("mux lock").snapshot(),
        },
        Request::Close { id } => {
            let mut mux = state.lock().expect("mux lock");
            let before = mux.sessions.len();
            mux.sessions.retain(|session| session.id != id);
            if mux.sessions.len() == before {
                Response::Error {
                    message: format!("no session {id}"),
                }
            } else {
                Response::Ok
            }
        }
        Request::Shutdown => {
            shutdown.store(true, Ordering::SeqCst);
            // Разбудить accept-цикл фиктивным соединением сделает вызывающий клиент.
            Response::Ok
        }
    }
}
