//! Клиент мультиплексера: подключается к сокету, при необходимости поднимает
//! фоновый сервер (детачится от клиента) и шлёт один запрос за соединение.

use std::io::{BufRead, BufReader, Write};
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
