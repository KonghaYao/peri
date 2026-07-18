//! Hub 主循环：tokio::select! 驱动的事件循环
//!
//! 同时监听 4 个事件源：
//! 1. IDE stdin → 解析 JSON-RPC，路由到全局处理器或子进程
//! 2. 子进程消息 channel → 转发给 IDE stdout
//! 3. 关闭信号 (Ctrl+C) → 优雅退出

use crate::error::{
    error_response, extract_method, extract_session_id, METHOD_NOT_FOUND, PARSE_ERROR,
    SESSION_NOT_FOUND,
};
use crate::global::{handle_commands_list, handle_initialize, handle_session_list};
use crate::router::{RouterEvent, SessionRouter};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;

/// Hub 配置
pub struct HubConfig {
    pub child_cmd: Vec<String>,
    pub spawn_timeout: u64,
    pub child_timeout: u64,
}

/// 启动 Hub 主循环
pub async fn run_hub(config: HubConfig) -> anyhow::Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdin_lines = stdin.lines();
    let mut stdout = BufWriter::new(tokio::io::stdout());

    // 子进程 → Hub 消息通道
    let (child_msg_tx, mut child_msg_rx) = mpsc::unbounded_channel::<RouterEvent>();

    // 路由器
    let mut router = SessionRouter::new(
        config.child_cmd,
        child_msg_tx,
        config.spawn_timeout,
        config.child_timeout,
    );

    tracing::info!(target: "acp_hub", "Hub 就绪");

    // 主事件循环
    loop {
        tokio::select! {
            // 1. IDE stdin 输入
            line_result = stdin_lines.next_line() => {
                match line_result {
                    Ok(Some(line)) => {
                        let parsed: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(target: "acp_hub", "无法解析 IDE 输入: {}", e);
                                let err = error_response(None, PARSE_ERROR, &e.to_string());
                                write_response(&mut stdout, &err).await?;
                                continue;
                            }
                        };
                        handle_ide_message(&mut router, &parsed, &mut stdout).await?;
                    }
                    Ok(None) => {
                        tracing::info!(target: "acp_hub", "IDE stdin 关闭，退出");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(target: "acp_hub", "IDE stdin 读取错误: {}", e);
                        break;
                    }
                }
            }

            // 2. 子进程消息 → 转发给 IDE
            Some(event) = child_msg_rx.recv() => {
                match event {
                    RouterEvent::ChildMessage(_sid, msg) => {
                        write_response(&mut stdout, &msg).await?;
                    }
                }
            }

            // 3. Ctrl+C 信号
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(target: "acp_hub", "收到 SIGINT，开始优雅退出");
                shutdown_all_sessions(&mut router).await;
                break;
            }

            else => {
                tracing::info!(target: "acp_hub", "所有事件源关闭，退出");
                break;
            }
        }
    }

    Ok(())
}

/// 处理来自 IDE 的一条 JSON-RPC 消息
async fn handle_ide_message(
    router: &mut SessionRouter,
    msg: &Value,
    stdout: &mut BufWriter<tokio::io::Stdout>,
) -> anyhow::Result<()> {
    let method = extract_method(msg);
    let id = msg.get("id");
    let session_id = extract_session_id(msg);

    match method {
        // === 全局请求 ===
        Some("initialize") => {
            let id = id.unwrap_or(&Value::Null);
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            // 缓存 IDE 的 initialize params，后续创建子进程时透传
            router.set_client_init_params(params);
            let resp = handle_initialize(id);
            write_response(stdout, &resp).await?;
        }

        Some("session/new") => {
            let id = id.unwrap_or(&Value::Null);
            let params = msg.get("params").unwrap_or(&Value::Null);
            let resp = router.create_session(id, params).await;
            write_response(stdout, &resp).await?;
        }

        Some("session/close") => {
            let id = id.unwrap_or(&Value::Null);
            if let Some(sid) = session_id {
                let resp = router.close_session(id, sid).await;
                write_response(stdout, &resp).await?;
            } else {
                let resp = error_response(Some(id), SESSION_NOT_FOUND, "missing session_id");
                write_response(stdout, &resp).await?;
            }
        }

        Some("session/list") => {
            let id = id.unwrap_or(&Value::Null);
            let sessions = router.list_sessions();
            let resp = handle_session_list(id, &sessions);
            write_response(stdout, &resp).await?;
        }

        Some("commands/list") => {
            let id = id.unwrap_or(&Value::Null);
            let resp = handle_commands_list(id);
            write_response(stdout, &resp).await?;
        }

        // === session 请求（有 id） ===
        Some(method) if id.is_some() => {
            if let Some(sid) = session_id {
                let id = id.unwrap();
                let params = msg.get("params").unwrap_or(&Value::Null);
                let resp = router.forward_request(id, sid, method, params).await;
                write_response(stdout, &resp).await?;
            } else {
                let id = id.unwrap();
                let resp = error_response(Some(id), SESSION_NOT_FOUND, "missing session_id");
                write_response(stdout, &resp).await?;
            }
        }

        // === session 通知（无 id） ===
        Some(method) => {
            if let Some(sid) = session_id {
                let params = msg.get("params").unwrap_or(&Value::Null);
                router.forward_notification(sid, method, params).await;
            } else {
                tracing::warn!(target: "acp_hub", "忽略无 session_id 的通知: {}", method);
            }
        }

        // === 未知 ===
        None => {
            if let Some(id) = id {
                let resp = error_response(Some(id), METHOD_NOT_FOUND, "unknown method");
                write_response(stdout, &resp).await?;
            } else {
                tracing::warn!(target: "acp_hub", "忽略未知的无方法消息");
            }
        }
    }

    Ok(())
}

/// 向 IDE stdout 写入一行 JSON-RPC 消息
async fn write_response(
    stdout: &mut BufWriter<tokio::io::Stdout>,
    value: &Value,
) -> anyhow::Result<()> {
    let line = serde_json::to_string(value)?;
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

/// 优雅关闭所有 session
async fn shutdown_all_sessions(router: &mut SessionRouter) {
    let sessions: Vec<String> = router
        .list_sessions()
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    for sid in &sessions {
        let _ = router.close_session(&Value::Null, sid).await;
    }
    tracing::info!(target: "acp_hub", "所有 session 已关闭");
}
