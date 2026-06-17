//! Тонкий entrypoint: parse argv → wire_server → run over stdio.
//!
//! Вся бизнес-логика живёт в `cozby_mcp::{domain, application, infrastructure}`.

use std::env;
use std::process::ExitCode;
use std::sync::Arc;

use cozby_mcp::application::ports::HttpTransport;
use cozby_mcp::domain::Contract;
use cozby_mcp::{
    builtin_brain_contract, load_contract_file, parse_args, wire_server, Args, ConfigError,
    ReqwestTransport, StdFileSystem,
};

const HELP: &str = "\
cozby-mcp — MCP stdio server: read-only filesystem tools + HTTP contracts

USAGE:
    cozby-mcp [--root <dir>] [--brain-url <url>] [--contract <file.toml> ...]

OPTIONS:
    --root <dir>        Restrict filesystem tool access to this directory (default: cwd)
    --brain-url <url>   Attach the built-in cozby-brain contract (save_note, search_notes,
                        save_doc, recall). Falls back to env COZBY_BRAIN_URL.
    --contract <file>   Attach a TOML contract describing an HTTP service as MCP tools.
                        Repeatable. Falls back to env COZBY_MCP_CONTRACTS (':'-separated).
    -h, --help          Print help
    -V, --version       Print version
";

fn main() -> ExitCode {
    let args = match parse_args(env::args().skip(1)) {
        Ok(args) => args,
        Err(ConfigError::HelpRequested) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(ConfigError::VersionRequested) => {
            println!("cozby-mcp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("cozby-mcp: {error}");
            return ExitCode::from(2);
        }
    };

    run(args)
}

fn run(args: Args) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            eprintln!("cozby-mcp: failed to start tokio runtime: {error}");
            return ExitCode::from(1);
        }
    };

    let fs = Arc::new(StdFileSystem::new());

    // Транспорт строим здесь, до входа в async-рантайм: blocking-reqwest не
    // должен инициализироваться внутри tokio-контекста.
    let transport: Arc<dyn HttpTransport> = match ReqwestTransport::new() {
        Ok(transport) => Arc::new(transport),
        Err(error) => {
            eprintln!("cozby-mcp: cannot build HTTP transport: {error}");
            return ExitCode::from(1);
        }
    };

    // Собираем контракты: встроенный brain (если задан --brain-url) + файлы.
    let mut contracts: Vec<Contract> = Vec::new();
    if let Some(url) = &args.brain_url {
        contracts.push(builtin_brain_contract(url.clone()));
    }
    for path in &args.contracts {
        match load_contract_file(path) {
            Ok(contract) => contracts.push(contract),
            Err(error) => {
                eprintln!("cozby-mcp: {error}");
                return ExitCode::from(1);
            }
        }
    }

    let mut server = wire_server(args, fs, transport, contracts);
    match runtime.block_on(async move { server.run().await }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cozby-mcp: transport error: {error}");
            ExitCode::from(1)
        }
    }
}
