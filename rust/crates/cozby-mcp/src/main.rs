//! Тонкий entrypoint: parse argv → wire_server → run over stdio.
//!
//! Вся бизнес-логика живёт в `cozby_mcp::{domain, application, infrastructure}`.

use std::env;
use std::process::ExitCode;
use std::sync::Arc;

use cozby_mcp::{parse_args, wire_server, Args, ConfigError, StdFileSystem};

const HELP: &str = "\
cozby-mcp — MCP stdio server (read-only, root-scoped filesystem tools)

USAGE:
    cozby-mcp [--root <dir>]

OPTIONS:
    --root <dir>   Restrict tool access to this directory (default: cwd)
    -h, --help     Print help
    -V, --version  Print version
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
    let mut server = wire_server(args, fs);
    match runtime.block_on(async move { server.run().await }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cozby-mcp: transport error: {error}");
            ExitCode::from(1)
        }
    }
}
