//! Клиент мультиплексера: подключается к сокету, при необходимости поднимает
//! фоновый сервер (детачится от клиента) и шлёт один запрос за соединение.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::protocol::{Request, Response};

/// Гарантирует, что сервер запущен: пробует подключиться, иначе спавнит демона
/// (`<exe> mux --serve`) с логом и ждёт появления сокета.
fn ensure_running(socket_path: &Path) -> std::io::Result<()> {
    if UnixStream::connect(socket_path).is_ok() {
        return Ok(());
    }
    // Устаревший сокет от умершего сервера — убираем.
    let _ = std::fs::remove_file(socket_path);

    let exe = std::env::current_exe()?;
    let log = socket_path.with_file_name("mux.log");
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .ok();
    let (stdout, stderr) = match out {
        Some(file) => (
            Stdio::from(file.try_clone()?),
            Stdio::from(file),
        ),
        None => (Stdio::null(), Stdio::null()),
    };
    Command::new(exe)
        .arg("mux")
        .arg("--serve")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()?;

    for _ in 0..100 {
        if UnixStream::connect(socket_path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "mux server did not start",
    ))
}

/// Отправляет один запрос и возвращает ответ, подняв сервер при необходимости.
///
/// # Errors
/// Ошибки запуска сервера, соединения, сериализации или чтения ответа.
pub fn request(socket_path: &Path, req: &Request) -> std::io::Result<Response> {
    ensure_running(socket_path)?;
    let stream = UnixStream::connect(socket_path)?;
    let mut writer = stream.try_clone()?;
    let payload = serde_json::to_string(req)?;
    writeln!(writer, "{payload}")?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(line.trim())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Подключается к сессии: вывод агента → stdout, строки stdin → агенту.
/// Завершается по Ctrl-D (EOF на stdin) — сессия продолжает работать на сервере.
///
/// # Errors
/// Ошибки запуска сервера, соединения или сериализации запроса.
pub fn attach(socket_path: &Path, id: &str) -> std::io::Result<()> {
    ensure_running(socket_path)?;
    let stream = UnixStream::connect(socket_path)?;
    let mut writer = stream.try_clone()?;
    let payload = serde_json::to_string(&Request::Attach { id: id.to_string() })?;
    writeln!(writer, "{payload}")?;
    writer.flush()?;

    // Поток сервера → stdout.
    let mut read_stream = stream.try_clone()?;
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut stdout = std::io::stdout();
        loop {
            match read_stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if stdout.write_all(&chunk[..read]).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                }
            }
        }
    });

    // stdin клиента (строки) → серверу как промпты, пока не Ctrl-D.
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if writer.write_all(line.as_bytes()).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader.join();
    Ok(())
}

/// Активное подключение к сфокусированной сессии: сокет + поток-читатель вывода.
struct Focus {
    id: String,
    stream: UnixStream,
    reader: std::thread::JoinHandle<()>,
}

fn open_focus(socket_path: &Path, id: &str) -> std::io::Result<Focus> {
    let stream = UnixStream::connect(socket_path)?;
    let mut writer = stream.try_clone()?;
    let payload = serde_json::to_string(&Request::Attach { id: id.to_string() })?;
    writeln!(writer, "{payload}")?;
    writer.flush()?;
    let mut read_stream = stream.try_clone()?;
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut stdout = std::io::stdout();
        loop {
            match read_stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if stdout.write_all(&chunk[..read]).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                }
            }
        }
    });
    Ok(Focus {
        id: id.to_string(),
        stream,
        reader,
    })
}

fn close_focus(focus: Focus) {
    let _ = focus.stream.shutdown(Shutdown::Both);
    let _ = focus.reader.join();
}

/// id сессии по 1-based номеру из списка.
fn id_by_index(socket_path: &Path, index: usize) -> Option<String> {
    match request(socket_path, &Request::List).ok()? {
        Response::Sessions { sessions } => {
            index.checked_sub(1).and_then(|i| sessions.get(i)).map(|s| s.id.clone())
        }
        _ => None,
    }
}

fn print_sessions(socket_path: &Path, focused: Option<&str>) {
    match request(socket_path, &Request::List) {
        Ok(Response::Sessions { sessions }) if !sessions.is_empty() => {
            for (index, session) in sessions.iter().enumerate() {
                let mark = if Some(session.id.as_str()) == focused {
                    "▸"
                } else {
                    " "
                };
                let title: String = session.title.chars().take(28).collect();
                println!(
                    "  {mark} {:<2} [{:<7}] {:<30} {}",
                    index + 1,
                    session.status,
                    title,
                    session.cwd
                );
            }
        }
        Ok(Response::Sessions { .. }) => println!("  (сессий нет — :new [dir] [title])"),
        Ok(Response::Error { message }) => eprintln!("  ошибка: {message}"),
        _ => {}
    }
}

fn console_help() {
    println!(
        "Команды кокпита:\n  \
         :ls                список агентов\n  \
         :new [dir] [title] новый агент (dir — рабочая директория)\n  \
         :switch N | :N     переключить фокус на агента N\n  \
         :close N           закрыть агента N\n  \
         :help :quit        справка / выход (агенты продолжат в фоне)\n  \
         <текст>            отправить промпт сфокусированному агенту"
    );
}

/// Единый интерактивный кокпит мультиплексера: из одного интерфейса создаёшь,
/// видишь, переключаешь и общаешься со всеми агентами (демон держит их в фоне).
///
/// # Errors
/// Ошибки запуска сервера или ввода-вывода.
pub fn console(socket_path: &Path) -> std::io::Result<()> {
    ensure_running(socket_path)?;
    println!("cozby-claw — мультиплексер агентов. :help — команды, :quit — выход.");
    print_sessions(socket_path, None);

    let mut focus: Option<Focus> = None;
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let text = line.trim_end_matches(['\n', '\r']);
        if let Some(meta) = text.strip_prefix(':') {
            let mut parts = meta.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            match cmd {
                "help" | "h" => console_help(),
                "ls" | "list" => {
                    print_sessions(socket_path, focus.as_ref().map(|f| f.id.as_str()));
                }
                "quit" | "q" | "exit" => break,
                "new" => {
                    let cwd = parts.next().map_or_else(
                        || {
                            std::env::current_dir()
                                .map_or_else(|_| ".".to_string(), |p| p.display().to_string())
                        },
                        |dir| {
                            std::fs::canonicalize(dir)
                                .map_or_else(|_| dir.to_string(), |p| p.display().to_string())
                        },
                    );
                    let title_rest: Vec<&str> = parts.collect();
                    let title = (!title_rest.is_empty()).then(|| title_rest.join(" "));
                    match request(socket_path, &Request::New { cwd, title, prompt: None }) {
                        Ok(Response::Created { id }) => {
                            switch_focus(socket_path, &mut focus, &id);
                        }
                        Ok(Response::Error { message }) => eprintln!("ошибка: {message}"),
                        _ => {}
                    }
                }
                "switch" | "s" => {
                    if let Some(id) = parts
                        .next()
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|n| id_by_index(socket_path, n))
                    {
                        switch_focus(socket_path, &mut focus, &id);
                    } else {
                        eprintln!("использование: :switch N");
                    }
                }
                "close" => {
                    if let Some(id) = parts
                        .next()
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|n| id_by_index(socket_path, n))
                    {
                        let _ = request(socket_path, &Request::Close { id: id.clone() });
                        if focus.as_ref().is_some_and(|f| f.id == id) {
                            if let Some(old) = focus.take() {
                                close_focus(old);
                            }
                        }
                        print_sessions(socket_path, focus.as_ref().map(|f| f.id.as_str()));
                    } else {
                        eprintln!("использование: :close N");
                    }
                }
                numeric if numeric.parse::<usize>().is_ok() => {
                    if let Some(id) = numeric
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| id_by_index(socket_path, n))
                    {
                        switch_focus(socket_path, &mut focus, &id);
                    }
                }
                other => eprintln!("неизвестная команда :{other} (см. :help)"),
            }
        } else if let Some(active) = focus.as_mut() {
            let _ = writeln!(active.stream, "{text}");
            let _ = active.stream.flush();
        } else {
            println!("нет активного агента — :new [dir] или :switch N");
        }
    }
    if let Some(active) = focus.take() {
        close_focus(active);
    }
    Ok(())
}

fn switch_focus(socket_path: &Path, focus: &mut Option<Focus>, id: &str) {
    if let Some(old) = focus.take() {
        close_focus(old);
    }
    match open_focus(socket_path, id) {
        Ok(new_focus) => {
            println!("▸ фокус: {id}");
            *focus = Some(new_focus);
        }
        Err(error) => eprintln!("не удалось подключиться к {id}: {error}"),
    }
}
