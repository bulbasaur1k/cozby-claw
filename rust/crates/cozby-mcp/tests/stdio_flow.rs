//! Интеграционный тест: поднимаем настоящий бинарь `cozby-mcp` в отдельном
//! процессе, пишем в его stdin LSP-framed JSON-RPC и читаем ответы.
//!
//! Проверяем три сценария: `initialize`, `tools/list`, `tools/call` для
//! `read_file` (хэппи-пас) и для пути, выходящего из `--root` (security-пас).

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

fn temp_dir(label: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "cozby-mcp-it-{label}-{}-{:08x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&base).unwrap();
    fs::canonicalize(&base).unwrap()
}

fn binary_path() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` выставляется cargo при запуске integration-тестов.
    PathBuf::from(env!("CARGO_BIN_EXE_cozby-mcp"))
}

async fn write_frame(stdin: &mut ChildStdin, payload: &Value) {
    let body = serde_json::to_vec(payload).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await.unwrap();
    stdin.write_all(&body).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_frame(stdout: &mut BufReader<ChildStdout>) -> Value {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = stdout.read_line(&mut line).await.unwrap();
        assert!(read > 0, "unexpected EOF while reading headers");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = Some(value.trim().parse().unwrap());
            }
        }
    }
    let len = content_length.expect("missing Content-Length");
    let mut buf = vec![0_u8; len];
    stdout.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[tokio::test]
async fn initialize_then_list_and_call_tools() {
    let root = temp_dir("happy");
    fs::write(root.join("hello.txt"), "world").unwrap();

    let mut child = Command::new(binary_path())
        .arg("--root")
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cozby-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // ----- initialize -----
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "it-client", "version": "0.0.0" }
            }
        }),
    )
    .await;

    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["serverInfo"]["name"], "cozby-mcp");
    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");

    // ----- tools/list -----
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )
    .await;
    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["read_file", "list_dir", "glob", "grep"]);

    // ----- tools/call: read_file (happy) -----
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "read_file", "arguments": { "path": "hello.txt" } }
        }),
    )
    .await;
    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    assert_eq!(response["id"], 3);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["content"][0]["text"], "world");

    // ----- tools/call: read_file (escape must fail safely) -----
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "read_file", "arguments": { "path": "../../../etc/passwd" } }
        }),
    )
    .await;
    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    assert_eq!(response["id"], 4);
    assert_eq!(response["result"]["isError"], true);

    drop(stdin);
    let _ = timeout(TIMEOUT, child.wait()).await;
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn custom_contract_tool_is_advertised() {
    let root = temp_dir("contract");
    let contract = root.join("weather.toml");
    fs::write(
        &contract,
        "name = \"weather\"\n\
         base_url = \"https://api.weather.example\"\n\
         [[tools]]\n\
         name = \"forecast\"\n\
         description = \"Get forecast\"\n\
         method = \"GET\"\n\
         path = \"/v1/forecast\"\n\
         response = \"data\"\n\
         [[tools.params]]\n\
         name = \"city\"\n\
         location = \"query\"\n\
         required = true\n",
    )
    .unwrap();

    let mut child = Command::new(binary_path())
        .arg("--root")
        .arg(&root)
        .arg("--contract")
        .arg(&contract)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cozby-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26", "capabilities": {} }
        }),
    )
    .await;
    let _ = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();

    write_frame(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    let tools = response["result"]["tools"].as_array().expect("tools array");
    let forecast = tools
        .iter()
        .find(|tool| tool["name"] == "forecast")
        .expect("custom contract tool advertised");
    // GET contract tool → read-only, open-world; required param in schema.
    assert_eq!(forecast["annotations"]["readOnlyHint"], true);
    assert_eq!(forecast["annotations"]["openWorldHint"], true);
    assert_eq!(forecast["inputSchema"]["required"][0], "city");

    drop(stdin);
    let _ = timeout(TIMEOUT, child.wait()).await;
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let root = temp_dir("unknown-method");

    let mut child = Command::new(binary_path())
        .arg("--root")
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cozby-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "totally/unknown"
        }),
    )
    .await;
    let response = timeout(TIMEOUT, read_frame(&mut stdout)).await.unwrap();
    assert_eq!(response["id"], 99);
    assert_eq!(response["error"]["code"], -32601);

    drop(stdin);
    let _ = timeout(TIMEOUT, child.wait()).await;
    let _ = fs::remove_dir_all(&root);
}
