//! Фоновый сервер мультиплексера. Каждая сессия — отдельный дочерний процесс
//! агента (`cozby-claw-cli` REPL в своей рабочей директории, без TTY: читает
//! промпты из stdin, печатает в stdout). Так у каждого агента свой реальный `cwd`
//! (изоляция проектов), и переиспользуется весь существующий агент CLI.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use super::protocol::{Request, Response, SessionInfo};

/// Максимум хранимого транскрипта на сессию (хвост), байт.
const MAX_BUFFER: usize = 200_000;

/// Одна сессия-агент: дочерний процесс + stdin + буфер вывода + статус +
/// подписчики (attach-соединения), которым транслируется вывод агента.
struct Session {
    id: String,
    title: String,
    cwd: String,
    child: Child,
    stdin: Option<ChildStdin>,
    buffer: Arc<Mutex<String>>,
    status: Arc<Mutex<String>>,
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
    pinned: bool,
}

impl Session {
    /// Спавнит дочерний агент в `cwd` и поднимает читатели stdout/stderr.
    fn spawn(id: String, cwd: String, title: String) -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let mut child = Command::new(&exe)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let buffer = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new("idle".to_string()));
        let subscribers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        if let Some(out) = child.stdout.take() {
            spawn_reader(
                out,
                Arc::clone(&buffer),
                Arc::clone(&subscribers),
                Some(Arc::clone(&status)),
            );
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, Arc::clone(&buffer), Arc::clone(&subscribers), None);
        }
        Ok(Self {
            id,
            title,
            cwd,
            child,
            stdin,
            buffer,
            status,
            subscribers,
            pinned: false,
        })
    }

    /// Жив ли дочерний процесс (обновляет статус на `exited`, если умер).
    fn alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_)) => {
                *self.status.lock().expect("status lock") = "exited".to_string();
                false
            }
            _ => true,
        }
    }

    fn send_prompt(&mut self, text: &str) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "agent stdin closed",
            ));
        };
        *self.status.lock().expect("status lock") = "working".to_string();
        writeln!(stdin, "{text}")?;
        stdin.flush()
    }

    fn info(&mut self) -> SessionInfo {
        let alive = self.alive();
        let status = if alive {
            self.status.lock().expect("status lock").clone()
        } else {
            "exited".to_string()
        };
        SessionInfo {
            id: self.id.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            status,
            msgs: 0,
            pinned: self.pinned,
        }
    }
}

/// Читает поток дочернего процесса: пишет в буфер, транслирует подписчикам
/// (attach) и обновляет статус по приглашению REPL.
fn spawn_reader(
    mut stream: impl Read + Send + 'static,
    buffer: Arc<Mutex<String>>,
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
    idle_status: Option<Arc<Mutex<String>>>,
) {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let bytes = &chunk[..read];
                    let text = String::from_utf8_lossy(bytes);
                    if let Ok(mut buffer) = buffer.lock() {
                        buffer.push_str(&text);
                        if buffer.len() > MAX_BUFFER {
                            let cut = buffer.len() - MAX_BUFFER;
                            buffer.drain(..cut);
                        }
                    }
                    // Транслируем подписчикам; отвалившихся (ошибка записи) убираем.
                    if let Ok(mut subs) = subscribers.lock() {
                        subs.retain_mut(|sub| sub.write_all(bytes).and_then(|()| sub.flush()).is_ok());
                    }
                    // Приглашение REPL (символ `›`) = агент снова ждёт ввода.
                    if let Some(status) = &idle_status {
                        if text.contains('\u{203A}') {
                            *status.lock().expect("status lock") = "idle".to_string();
                        }
                    }
                }
            }
        }
    });
}

#[derive(Default)]
struct Mux {
    sessions: Vec<Session>,
    counter: usize,
}

impl Mux {
    fn new_session(
        &mut self,
        cwd: String,
        title: Option<String>,
        prompt: Option<String>,
    ) -> std::io::Result<String> {
        self.counter += 1;
        let id = format!("s{}", self.counter);
        let title = title.unwrap_or_else(|| default_title(&cwd));
        let mut session = Session::spawn(id.clone(), cwd, title)?;
        if let Some(text) = prompt {
            let _ = session.send_prompt(&text);
        }
        self.sessions.push(session);
        Ok(id)
    }

    fn snapshot(&mut self) -> Vec<SessionInfo> {
        let mut infos: Vec<SessionInfo> = self.sessions.iter_mut().map(Session::info).collect();
        // Закреплённые — первыми (стабильно, остальной порядок сохраняется).
        infos.sort_by_key(|info| !info.pinned);
        infos
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    fn close(&mut self, id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain_mut(|session| {
            if session.id == id {
                let _ = session.child.kill();
                false
            } else {
                true
            }
        });
        self.sessions.len() != before
    }
}

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
/// Ошибки привязки/приёма соединений сокета.
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
        thread::spawn(move || handle_conn(stream, conn_state, conn_shutdown));
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

fn handle_conn(stream: UnixStream, state: Arc<Mutex<Mux>>, shutdown: Arc<AtomicBool>) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let request = match serde_json::from_str::<Request>(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            let mut writer = &stream;
            let _ = writeln!(writer, "bad request: {error}");
            return;
        }
    };
    // Attach переводит соединение в потоковый режим — обычного ответа нет.
    if let Request::Attach { id } = &request {
        handle_attach(&stream, id, &state);
        return;
    }
    let response = process(request, &state, &shutdown);
    let mut writer = &stream;
    let payload = serde_json::to_string(&response).unwrap_or_else(|_| {
        "{\"resp\":\"error\",\"message\":\"serialize failed\"}".to_string()
    });
    let _ = writeln!(writer, "{payload}");
    let _ = writer.flush();
}

/// Потоковый режим: транслируем вывод сессии клиенту и шлём его строки агенту.
/// Завершается, когда клиент отсоединился (EOF) — сессия продолжает работать.
fn handle_attach(stream: &UnixStream, id: &str, state: &Arc<Mutex<Mux>>) {
    {
        let mut mux = state.lock().expect("mux lock");
        match mux.session_mut(id) {
            Some(session) => {
                // Реплей текущего транскрипта + подписка на будущий вывод.
                let replay = session.buffer.lock().expect("buffer lock").clone();
                if let Ok(mut writer) = stream.try_clone() {
                    let _ = writer.write_all(replay.as_bytes());
                    let _ = writer.flush();
                }
                if let Ok(sub) = stream.try_clone() {
                    session.subscribers.lock().expect("subs lock").push(sub);
                }
            }
            None => {
                let mut writer = stream;
                let _ = writeln!(writer, "no session {id}");
                return;
            }
        }
    }
    // Ввод клиента (строки) → промпты агенту, пока клиент не отсоединится.
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(reader_stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(mut mux) = state.lock() {
            if let Some(session) = mux.session_mut(id) {
                let _ = session.send_prompt(&line);
            }
        }
    }
}

fn process(request: Request, state: &Mutex<Mux>, shutdown: &AtomicBool) -> Response {
    match request {
        Request::Ping => Response::Ok,
        Request::New { cwd, title, prompt } => {
            match state.lock().expect("mux lock").new_session(cwd, title, prompt) {
                Ok(id) => Response::Created { id },
                Err(error) => Response::Error {
                    message: format!("spawn failed: {error}"),
                },
            }
        }
        Request::List => Response::Sessions {
            sessions: state.lock().expect("mux lock").snapshot(),
        },
        Request::Prompt { id, text } => {
            let mut mux = state.lock().expect("mux lock");
            match mux.session_mut(&id) {
                Some(session) => match session.send_prompt(&text) {
                    Ok(()) => Response::Ok,
                    Err(error) => Response::Error {
                        message: format!("send failed: {error}"),
                    },
                },
                None => Response::Error {
                    message: format!("no session {id}"),
                },
            }
        }
        Request::Logs { id } => {
            let mut mux = state.lock().expect("mux lock");
            match mux.session_mut(&id) {
                Some(session) => Response::Logs {
                    text: session.buffer.lock().expect("buffer lock").clone(),
                },
                None => Response::Error {
                    message: format!("no session {id}"),
                },
            }
        }
        Request::Pin { id, pinned } => {
            let mut mux = state.lock().expect("mux lock");
            match mux.session_mut(&id) {
                Some(session) => {
                    session.pinned = pinned;
                    Response::Ok
                }
                None => Response::Error {
                    message: format!("no session {id}"),
                },
            }
        }
        Request::Close { id } => {
            if state.lock().expect("mux lock").close(&id) {
                Response::Ok
            } else {
                Response::Error {
                    message: format!("no session {id}"),
                }
            }
        }
        Request::Shutdown => {
            shutdown.store(true, Ordering::SeqCst);
            Response::Ok
        }
        // Attach обрабатывается в handle_conn (потоковый режим) и сюда не доходит.
        Request::Attach { .. } => Response::Error {
            message: "attach is a streaming request".to_string(),
        },
    }
}
