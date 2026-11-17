//! 真实 bare CLI：保留基础工作区能力，同时隔离用户集成。
#![cfg(unix)]

use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

const MARKER: &str = "bare-workspace-roundtrip";

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let (body_start, body_len) = loop {
        let mut chunk = [0; 4096];
        let count = socket
            .read(&mut chunk)
            .await
            .expect("read request: fixture task and request must complete successfully");
        assert_ne!(count, 0, "provider 请求不应提前结束");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end])
                .expect("read request: fixture paths and protocol text must be valid UTF-8");
            assert!(headers.starts_with("POST /v1/messages "));
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .expect("read request: fixture must contain the searched item")
                .1
                .trim()
                .parse::<usize>()
                .expect("read request: fixture must contain the searched item");
            break (end + 4, length);
        }
    };
    while bytes.len() < body_start + body_len {
        let mut chunk = [0; 4096];
        let count = socket
            .read(&mut chunk)
            .await
            .expect("read request: fixture task and request must complete successfully");
        assert_ne!(count, 0, "provider 请求 body 不应被截断");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[body_start..body_start + body_len])
        .expect("read request: fixture JSON must match the expected schema")
}

async fn respond(socket: &mut TcpStream, tool: Option<(&str, &str, Value)>, text: &str) {
    let (content, delta, stop) = match tool {
        Some((id, name, input)) => (
            json!({"type":"tool_use", "id":id, "name":name, "input":{}}),
            json!({"type":"input_json_delta", "partial_json":input.to_string()}),
            "tool_use",
        ),
        None => (
            json!({"type":"text", "text":""}),
            json!({"type":"text_delta", "text":text}),
            "end_turn",
        ),
    };
    let events = [
        (
            "message_start",
            json!({"message": {"id":"fixture-response", "type":"message", "role":"assistant", "model":"fixture-model", "content":[], "usage":{"input_tokens":1,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"index":0, "content_block":content}),
        ),
        ("content_block_delta", json!({"index":0, "delta":delta})),
        ("content_block_stop", json!({"index":0})),
        (
            "message_delta",
            json!({"delta":{"stop_reason":stop}, "usage":{"input_tokens":100,"output_tokens":7}}),
        ),
        ("message_stop", json!({})),
    ];
    let body = events
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
    socket
        .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .expect("respond: fixture task and request must complete successfully");
    socket
        .shutdown()
        .await
        .expect("respond: fixture task and request must complete successfully");
}

fn install_integrations(root: &Path) {
    let claude = root.join(".claude");
    let plugin = claude.join("plugins/cache/fixture/blocked/1.0.0");
    std::fs::create_dir_all(plugin.join(".claude-plugin"))
        .expect("install integrations: isolated fixture workspace must remain accessible");
    std::fs::create_dir_all(root.join(".peri"))
        .expect("install integrations: isolated fixture workspace must remain accessible");
    // 非法插件 MCP 必须根本不加载，否则普通初始化会 fail closed。
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        json!({
            "name":"blocked", "version":"1.0.0", "mcpServers":{"invalid":{"command":17}}
        })
        .to_string(),
    )
    .expect("install integrations: isolated fixture workspace must remain accessible");
    std::fs::write(claude.join("plugins/installed_plugins.json"), json!({
        "version":2, "plugins":[{"id":"blocked@fixture", "name":"blocked", "version":"1.0.0", "marketplace":"fixture", "install_path":plugin, "scope":"User", "origin":"PeriInstalled"}]
    }).to_string()).expect("install integrations: isolated fixture workspace must remain accessible");
    let hooks = json!({"SessionStart":[{"hooks":[{"type":"command", "command":"touch unexpected-hook"}]}], "PreToolUse":[{"matcher":"*", "hooks":[{"type":"command", "command":"touch unexpected-hook"}]}]});
    std::fs::write(
        claude.join("settings.json"),
        json!({"enabledPlugins":["blocked@fixture"], "hooks":hooks}).to_string(),
    )
    .expect("install integrations: isolated fixture workspace must remain accessible");
    std::fs::write(
        root.join(".mcp.json"),
        json!({"mcpServers":{"external":{"command":"sh", "args":["-c", "touch unexpected-mcp"]}}})
            .to_string(),
    )
    .expect("install integrations: isolated fixture workspace must remain accessible");
    std::fs::write(
        root.join(".peri/settings.json"),
        json!({
            "mcpServers":{"global":{"command":"sh", "args":["-c", "touch unexpected-global-mcp"]}}
        })
        .to_string(),
    )
    .expect("install integrations: isolated fixture workspace must remain accessible");
}

async fn run_bare(builtins_disabled: bool) {
    let fixture =
        tempfile::tempdir().expect("run bare: isolated fixture workspace must remain accessible");
    let root = fixture
        .path()
        .canonicalize()
        .expect("run bare: isolated fixture workspace must remain accessible");
    install_integrations(&root);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("run bare: fixture task and request must complete successfully");
    let address = listener
        .local_addr()
        .expect("run bare: the fixture listener must expose its bound local address");
    let settings = root.join("settings.json");
    std::fs::write(&settings, json!({"config":{"providers":[{"id":"fixture", "type":"anthropic", "apiKey":"test-only", "baseUrl":format!("http://{address}"), "models":{"opus":"fixture-model"}}]}}).to_string()).expect("run bare: isolated fixture workspace must remain accessible");
    let calls = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&calls);
    let provider = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("run bare: fixture task and request must complete successfully");
            let request = read_request(&mut socket).await;
            if request.to_string().contains("<prediction_directive>")
                || request["tools"].as_array().is_none_or(Vec::is_empty)
            {
                respond(&mut socket, None, "").await;
                continue;
            }
            let tools = request["tools"]
                .as_array()
                .expect("run bare: fixture must have the expected variant");
            let workspace_names = [
                "Read",
                "Write",
                "Edit",
                "Glob",
                "Grep",
                "folder_operations",
                "Bash",
            ];
            let mcp_tools: Vec<_> = tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .filter(|name| {
                    workspace_names.contains(name) || name.starts_with("mcp__workspace__")
                })
                .collect();
            if builtins_disabled {
                assert!(
                    mcp_tools.is_empty(),
                    "显式 off 不能被 bare 覆盖: {mcp_tools:?}"
                );
                respond(&mut socket, None, "builtin-disabled").await;
                served.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            assert_eq!(
                mcp_tools.len(),
                7,
                "bare 只暴露 workspace 的七项基础工具: {mcp_tools:?}"
            );
            assert_eq!(
                mcp_tools
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>(),
                workspace_names.into_iter().collect(),
                "bare 中 system_mcp_tools 选中的 workspace 工具使用原名: {mcp_tools:?}"
            );
            let step = served.load(Ordering::SeqCst);
            let result = |id: &str| {
                request["messages"]
                    .as_array()
                    .expect("run bare: fixture must have the expected variant")
                    .iter()
                    .filter_map(|message| message["content"].as_array())
                    .flatten()
                    .find(|block| block["type"] == "tool_result" && block["tool_use_id"] == id)
                    .unwrap_or_else(|| panic!("缺少工具结果 {id}: {request}"))
            };
            let next = match step {
                0 => Some((
                    "write",
                    "Write",
                    json!({"file_path":"roundtrip.txt", "content":MARKER}),
                )),
                1 => {
                    assert_ne!(
                        result("write")["is_error"],
                        true,
                        "Write 必须成功: {}",
                        result("write")
                    );
                    Some(("read", "Read", json!({"file_path":"roundtrip.txt"})))
                }
                2 => {
                    assert_ne!(result("read")["is_error"], true);
                    assert!(result("read").to_string().contains(MARKER));
                    Some(("bash", "Bash", json!({"command":"cat roundtrip.txt"})))
                }
                3 => {
                    assert_ne!(result("bash")["is_error"], true);
                    assert!(result("bash").to_string().contains(MARKER));
                    None
                }
                _ => panic!("工作区操作完成后不得产生额外模型回合"),
            };
            respond(&mut socket, next, "bare-complete").await;
            served.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_peri"));
    command
        .env_clear()
        .env("HOME", &root)
        .env("TMPDIR", &root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(&root)
        .args([
            "--bare",
            "--print",
            "Write, read and verify the file.",
            "--output-format",
            "stream-json",
            "--permission-mode",
            "bypass",
        ])
        .arg("--config-file")
        .arg(&settings)
        .arg("--settings")
        .arg(&settings)
        .arg("--db-path")
        .arg(root.join("threads.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if builtins_disabled {
        command.env("PERI_MCP_BUILTIN", "off");
    }
    let result = tokio::time::timeout(
        Duration::from_secs(25),
        command
            .spawn()
            .expect("run bare: isolated fixture workspace must remain accessible")
            .wait_with_output(),
    )
    .await;
    provider.abort();
    let provider_result = provider.await;
    assert!(
        !provider_result
            .as_ref()
            .is_err_and(|error| error.is_panic()),
        "provider 验证失败: {provider_result:?}"
    );
    let output = result
        .expect("bare CLI 必须自行退出")
        .expect("run bare: the fixture operation must finish without an inner failure");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        if builtins_disabled { 1 } else { 4 }
    );
    if !builtins_disabled {
        assert_eq!(
            std::fs::read_to_string(root.join("roundtrip.txt")).expect(
                "run bare: the fixture storage and layout must support the requested operation"
            ),
            MARKER
        );
    }
    for marker in ["unexpected-hook", "unexpected-mcp", "unexpected-global-mcp"] {
        assert!(
            !root.join(marker).exists(),
            "bare 不得启动用户集成: {marker}"
        );
    }
    let events: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_slice(line)
                .expect("run bare: fixture JSON must match the expected schema")
        })
        .collect();
    let final_event = events
        .last()
        .expect("run bare: fixture collection must contain the expected item");
    assert_eq!(final_event["type"], "result");
    assert_eq!(final_event["status"], "completed");
    assert_eq!(final_event["is_error"], false);
}

/// [回归测试] MCP 迁移后的 bare 曾丢掉所有文件与终端工具。
#[tokio::test]
async fn bare_write_read_bash_exits_without_loading_user_integrations() {
    run_bare(false).await;
}

#[tokio::test]
async fn bare_respects_explicit_builtin_shutdown() {
    run_bare(true).await;
}
