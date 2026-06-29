//! Мультиплексер агентов: фоновый сервер держит сессии по разным проектам
//! (конкурентно, переживает клиента), CLI — тонкий клиент (`ls`/`new`/`close`,
//! далее `attach`). Этап 1: реестр сессий + жизненный цикл демона.

pub mod protocol;
mod client;
mod server;

use std::path::PathBuf;

use protocol::{Request, Response};

/// Каталог конфигурации (`$CLAW_CONFIG_HOME` или `~/.claw`).
fn config_home() -> PathBuf {
    if let Some(explicit) = std::env::var_os("CLAW_CONFIG_HOME") {
        return PathBuf::from(explicit);
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".claw"),
        |home| PathBuf::from(home).join(".claw"),
    )
}

fn socket_path() -> PathBuf {
    config_home().join("mux.sock")
}

/// Точка входа подкоманды `mux`.
///
/// # Errors
/// Ошибки сервера/клиента (сокет, IO, протокол).
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let socket = socket_path();
    match args.first().map(String::as_str) {
        Some("--serve") => {
            server::serve(&socket)?;
            Ok(())
        }
        None | Some("ls" | "list") => print_list(&socket),
        Some("new") => cmd_new(&socket, &args[1..]),
        Some("send") => cmd_send(&socket, &args[1..]),
        Some("logs") => cmd_logs(&socket, args.get(1).map(String::as_str)),
        Some("close") => cmd_close(&socket, args.get(1).map(String::as_str)),
        Some("attach") => cmd_attach(&socket, args.get(1).map(String::as_str)),
        Some(other) => {
            eprintln!("неизвестная команда mux: {other}");
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    println!(
        "Использование:\n  \
         cozby-claw-cli mux ls                          список сессий по всем проектам\n  \
         cozby-claw-cli mux new [--cwd DIR] [--title T] [prompt...]   завести агента\n  \
         cozby-claw-cli mux send <id> <текст...>        отправить промпт агенту\n  \
         cozby-claw-cli mux logs <id>                   показать транскрипт сессии\n  \
         cozby-claw-cli mux close <id>                  закрыть сессию\n  \
         cozby-claw-cli mux attach <id>                 подключиться (далее)"
    );
}

fn cmd_new(socket: &std::path::Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut cwd_arg: Option<String> = None;
    let mut title: Option<String> = None;
    let mut prompt_parts = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                cwd_arg = args.get(index + 1).cloned();
                index += 2;
            }
            "--title" => {
                title = args.get(index + 1).cloned();
                index += 2;
            }
            other => {
                prompt_parts.push(other.to_string());
                index += 1;
            }
        }
    }
    let cwd = cwd_arg
        .map(|raw| std::fs::canonicalize(&raw).map_or(raw, |path| path.display().to_string()))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |path| path.display().to_string())
        });
    let prompt = (!prompt_parts.is_empty()).then(|| prompt_parts.join(" "));

    match client::request(socket, &Request::New { cwd, title, prompt })? {
        Response::Created { id } => println!("создана сессия {id}"),
        Response::Error { message } => eprintln!("ошибка: {message}"),
        other => eprintln!("неожиданный ответ: {other:?}"),
    }
    Ok(())
}

fn cmd_send(socket: &std::path::Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let Some((id, rest)) = args.split_first() else {
        eprintln!("использование: cozby-claw-cli mux send <id> <текст...>");
        return Ok(());
    };
    if rest.is_empty() {
        eprintln!("использование: cozby-claw-cli mux send <id> <текст...>");
        return Ok(());
    }
    let text = rest.join(" ");
    match client::request(
        socket,
        &Request::Prompt {
            id: id.clone(),
            text,
        },
    )? {
        Response::Ok => println!("отправлено в {id}"),
        Response::Error { message } => eprintln!("ошибка: {message}"),
        other => eprintln!("неожиданный ответ: {other:?}"),
    }
    Ok(())
}

fn cmd_attach(
    socket: &std::path::Path,
    id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(id) = id else {
        eprintln!("использование: cozby-claw-cli mux attach <id>");
        return Ok(());
    };
    eprintln!("— подключено к {id}; Ctrl-D — detach (агент продолжит в фоне) —");
    client::attach(socket, id)?;
    eprintln!("\n— отключено от {id} —");
    Ok(())
}

fn cmd_logs(
    socket: &std::path::Path,
    id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(id) = id else {
        eprintln!("использование: cozby-claw-cli mux logs <id>");
        return Ok(());
    };
    match client::request(socket, &Request::Logs { id: id.to_string() })? {
        Response::Logs { text } => print!("{text}"),
        Response::Error { message } => eprintln!("ошибка: {message}"),
        other => eprintln!("неожиданный ответ: {other:?}"),
    }
    Ok(())
}

fn cmd_close(
    socket: &std::path::Path,
    id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(id) = id else {
        eprintln!("укажите id: cozby-claw-cli mux close <id>");
        return Ok(());
    };
    match client::request(socket, &Request::Close { id: id.to_string() })? {
        Response::Ok => println!("сессия {id} закрыта"),
        Response::Error { message } => eprintln!("ошибка: {message}"),
        other => eprintln!("неожиданный ответ: {other:?}"),
    }
    Ok(())
}

fn print_list(socket: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    match client::request(socket, &Request::List)? {
        Response::Sessions { sessions } => {
            if sessions.is_empty() {
                println!("Сессий нет. Создайте: cozby-claw-cli mux new --cwd <dir> [title]");
            } else {
                println!("{:<5} {:<9} {:<28} ПРОЕКТ", "ID", "СТАТУС", "ЗАГОЛОВОК");
                for session in sessions {
                    let title: String = session.title.chars().take(26).collect();
                    println!(
                        "{:<5} {:<9} {:<28} {}",
                        session.id, session.status, title, session.cwd
                    );
                }
            }
        }
        Response::Error { message } => eprintln!("ошибка: {message}"),
        other => eprintln!("неожиданный ответ: {other:?}"),
    }
    Ok(())
}
