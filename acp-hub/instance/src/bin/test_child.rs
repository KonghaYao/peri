//! 模拟 ACP 子进程，用于集成测试
//!
//! 从 stdin 读取 JSON-RPC 行，按简单协议响应：
//! - initialize → 返回 capabilities
//! - session/new → 返回 {session_id: "test-sid-001"}
//! - session/load → 恢复请求中的 sessionId，后续 update 继续归属该会话
//! - prompt → 逐行返回 session/update 通知（模拟流式输出）
//! - session/close → 返回 {closed: true} 后退出
//!
//! 支持 --crash-after=N：处理 N 条消息后 exit(1) 模拟崩溃

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let crash_after: Option<usize> = args
        .iter()
        .position(|a| a == "--crash-after")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());
    let audit_file = args
        .iter()
        .position(|a| a == "--audit-file")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut message_count: usize = 0;
    let mut session_counter: usize = 0;
    // session/new 返回的真实 ACP session id（prompt 通知复用，保证归属一致）。
    let mut current_session_id: String = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        message_count += 1;

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[test-child] JSON 解析失败: {}", e);
                continue;
            }
        };

        let method = msg.get("method").and_then(|v| v.as_str());
        let id = msg.get("id").cloned();

        match method {
            Some("initialize") => {
                send_response(
                    &id,
                    &serde_json::json!({
                        "protocolVersion": 1,
                        "capabilities": {
                            "prompt": {"stream": true}
                        },
                        "serverInfo": {
                            "name": "test-child",
                            "version": "0.1.0"
                        }
                    }),
                );
            }

            Some("session/new") => {
                session_counter += 1;
                // 使用 PID + 计数器确保不同进程返回唯一 ID
                current_session_id = format!("test-sid-{}-{}", std::process::id(), session_counter);
                send_response(
                    &id,
                    &serde_json::json!({
                        "sessionId": current_session_id
                    }),
                );
            }

            Some("session/load") => {
                let requested = msg
                    .get("params")
                    .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if requested.is_empty() {
                    send_error(&id, -32602, "session/load requires sessionId");
                } else {
                    write_audit(&audit_file, "session/load", requested);
                    current_session_id = requested.to_string();
                    send_response(&id, &serde_json::json!({"loaded": true}));
                }
            }

            Some("session/prompt") | Some("prompt") => {
                // 惰性生成：真实链路 server 必先 session/new；直接 prompt
                // （单测/异常链路）也保证通知携带非空 sessionId。
                if current_session_id.is_empty() {
                    current_session_id = format!("test-sid-{}-{}", std::process::id(), 0);
                }
                // 事件格式遵循 agent-client-protocol（真实 peri 实测）：
                // session/update 通知的 params.update 为
                // `{sessionUpdate, content:{type,text}}`——**无 turnId**，
                // 聚合器按 active_turn 归位（§7.2 宿主驱动 turn 模型）。
                let chunk = |i: u8| {
                    send_notification(
                        "session/update",
                        &serde_json::json!({
                            "sessionId": current_session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {
                                    "type": "text",
                                    "text": format!("chunk_{i}")
                                }
                            }
                        }),
                    );
                };
                // chunk_1 同步输出（脚本收到首个 delta 后即可发 cancel）；
                // 其余 chunk 延迟输出（模拟真实 ACP 的异步流式）——主循环
                // 随即读 stdin，可响应运行中的 cancel。
                chunk(1);
                let sid = current_session_id.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    for i in 2..=3 {
                        send_notification(
                            "session/update",
                            &serde_json::json!({
                                "sessionId": sid,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {
                                        "type": "text",
                                        "text": format!("chunk_{i}")
                                    }
                                }
                            }),
                        );
                    }
                });
                // 最后返回 prompt 完成响应（真实 peri：终态唯一信号
                // stopReason；`end_turn` → turn 完成，§7.2）。
                send_response(
                    &id,
                    &serde_json::json!({
                        "stopReason": "end_turn"
                    }),
                );
            }

            Some("session/close") => {
                send_response(
                    &id,
                    &serde_json::json!({
                        "closed": true
                    }),
                );
                std::process::exit(0);
            }

            Some("session/cancel") => {
                // 真实 peri：session/cancel 是 **notification**（无 id、无响应
                // 帧）——server 发送成功即确认并注入 turn 终态（§7.2）；
                // 本进程不回复、不另发终态通知（L3/cancel 确认已驱动）。
                eprintln!("[test-child] session/cancel (notification) 收到");
            }

            _ => {
                // 未知方法 → 返回 OK（兼容性）
                send_response(&id, &serde_json::json!({"ok": true}));
            }
        }

        // 检查 crash-after
        if let Some(limit) = crash_after {
            if message_count >= limit {
                eprintln!("[test-child] 模拟崩溃 (消息数: {})", message_count);
                std::process::exit(1);
            }
        }
    }
}

/// Test-only wire observation. Production instance logging intentionally never
/// records ACP stderr bodies; integration fixtures opt into this private temp file
/// so the restart product journey can prove the exact session/load identity.
fn write_audit(path: &Option<PathBuf>, method: &str, session_id: &str) {
    let Some(path) = path else { return };
    let record = serde_json::json!({"method": method, "sessionId": session_id});
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{record}");
    let _ = file.flush();
}

fn send_response(id: &Option<serde_json::Value>, result: &serde_json::Value) {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.clone().unwrap_or(serde_json::Value::Null),
        "result": result,
    });
    send(&resp);
}

fn send_error(id: &Option<serde_json::Value>, code: i64, message: &str) {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.clone().unwrap_or(serde_json::Value::Null),
        "error": {"code": code, "message": message},
    });
    send(&resp);
}

fn send_notification(method: &str, params: &serde_json::Value) {
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    send(&notif);
}

fn send(value: &serde_json::Value) {
    let line = serde_json::to_string(value).unwrap();
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", line).unwrap();
    stdout.flush().unwrap();
}
