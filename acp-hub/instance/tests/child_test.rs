//! child 集成测试：spawn/进程组/kill 幂等/进程组 kill 孙进程（T8）、kill_on_drop
//! （T12）、stdout 事件与缺口计数。
//!
//! 位于 `tests/`（integration）：`CARGO_BIN_EXE_test-child` 仅对集成测试可用。

use std::sync::Arc;
use std::time::Duration;

use acp_instance::child::{self, sys, AcpProcess, ChildOutput, ProcessState};
use tokio::sync::mpsc;

/// test-child 二进制路径。
fn test_child() -> String {
    env!("CARGO_BIN_EXE_test-child").to_string()
}

async fn spawn_child(
    cmd: &[String],
    session_id: &str,
) -> (Arc<AcpProcess>, mpsc::UnboundedReceiver<ChildOutput>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let acp = child::spawn(cmd, ".", None, session_id, tx)
        .await
        .expect("spawn 成功");
    (acp, rx)
}

#[tokio::test]
async fn test_spawn_basic() {
    let cmd = vec![test_child(), "--crash-after".into(), "100".into()];
    let (acp, _rx) = spawn_child(&cmd, "s1").await;
    assert_eq!(acp.session_id(), "s1");
    assert!(acp.pgid() > 0);
    assert_eq!(acp.state(), ProcessState::Running);
    // 进程组存在性探测（信号 0）。
    assert!(sys::kill_group(acp.pgid(), 0), "进程组应存在");
    acp.kill(Duration::from_millis(100)).await.unwrap();
}

#[tokio::test]
async fn test_stdout_events_and_dropped_no_sid() {
    let cmd = vec![test_child(), "--crash-after".into(), "100".into()];
    let (acp, mut rx) = spawn_child(&cmd, "s1").await;

    // initialize → test_child 回 JSON-RPC response（无 sessionId，§4.4 L3 靠
    // rpcId 匹配）→ 兜底按 inner.session_id 转发（#5：有 jsonrpc 键即兜底）。
    acp.write_line(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .await
        .unwrap();
    // prompt → test_child 输出 3 条 session/update 通知（带 sessionId）+ 1 条
    // prompt response（无 sessionId）。
    acp.write_line(&serde_json::json!({
        "jsonrpc":"2.0","id":2,"method":"prompt",
        "params":{"sessionId":"test-sid-001","messages":[]}
    }))
    .await
    .unwrap();
    // 预期：5 帧全部 Frame（2 响应 + 3 通知），无丢弃。通知带 ACP 内部
    // sessionId（test-sid-001）并原样转发（dumb pipe，§3.3）——server 凭其
    // 查可信 binding（§6.2：acp_session_id → hub session_id）。
    // （曾误改 hub id 归属导致 server relay binding_missing 全量丢弃。）
    let mut saw_dropped = false;
    let mut frames = Vec::new();
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ChildOutput::DroppedNoSessionId)) => saw_dropped = true,
            Ok(Some(ChildOutput::Frame(evt))) => frames.push(evt),
            Ok(Some(ChildOutput::Exit { .. })) | Ok(None) | Err(_) => break,
        }
        if !saw_dropped && frames.len() >= 5 {
            break;
        }
    }
    assert!(!saw_dropped, "JSON-RPC response 应兜底转发，不再缺口丢弃");
    assert_eq!(
        frames.len(),
        5,
        "应收到 2 响应 + 3 通知（got={}",
        frames.len()
    );
    assert_eq!(
        frames.iter().filter(|e| e.session_id == "s1").count(),
        5,
        "信封 session_id 一律为进程归属（hub session id，§4.5.1）；帧内 ACP sessionId 原样保留（可信 binding 校验键，§495）"
    );
    // 通知原帧保留 ACP sessionId（数据完整透传；test-child 输出的是
    // session/new 返回的真实 id，形如 test-sid-<pid>-1）。
    assert_eq!(
        frames
            .iter()
            .filter(|e| {
                e.frame.get("method") == Some(&serde_json::json!("session/update"))
                    && e.frame
                        .get("params")
                        .and_then(|p| p.get("sessionId"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.starts_with("test-sid-"))
            })
            .count(),
        3,
        "通知帧内 sessionId 原样保留（instance 不篡改数据）"
    );
    // 兜底帧仍保持 response 形态（有 id、无 method）。
    assert!(
        frames
            .iter()
            .any(|e| e.frame.get("id") == Some(&serde_json::json!(1))),
        "initialize response 应以原帧转发（rpcId=1）"
    );

    acp.kill(Duration::from_millis(200)).await.unwrap();

    // 无 sessionId 帧的两类处置（#5 双端点统一，与 relay C2 同判据）：
    // ① JSON-RPC 形态（有 jsonrpc 键，如 agent/status 通知）→ 按进程归属
    //    兜底转发（ChildOutput::Frame，信封 session_id = inner.session_id）；
    // ② 原始 {type,payload} 形态 → 仍 DroppedNoSessionId（缺口计数）。
    // （test_child 对任意输入都回 jsonrpc+id 响应，故用 sh 直接吐裸 JSON。）
    let (acp2, mut rx2) = spawn_child(
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo '{\"jsonrpc\":\"2.0\",\"method\":\"agent/status\",\"params\":{\"status\":\"busy\"}}'".to_string(),
        ],
        "s2",
    )
    .await;
    let mut saw_dropped = false;
    let mut frames2 = Vec::new();
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_secs(2), rx2.recv()).await {
            Ok(Some(ChildOutput::DroppedNoSessionId)) => saw_dropped = true,
            Ok(Some(ChildOutput::Frame(evt))) => frames2.push(evt),
            Ok(Some(ChildOutput::Exit { .. })) | Ok(None) | Err(_) => break,
        }
        if !saw_dropped && !frames2.is_empty() {
            break;
        }
    }
    assert_eq!(
        frames2.len(),
        1,
        "JSON-RPC 形态（有 jsonrpc 键）无 sessionId 应兜底转发（got={}）",
        frames2.len()
    );
    assert_eq!(frames2[0].session_id, "s2", "信封 session_id = 进程归属");
    assert_eq!(
        frames2[0].frame["method"],
        serde_json::json!("agent/status"),
        "原帧透传"
    );
    assert!(!saw_dropped, "JSON-RPC 形态不得缺口丢弃");
    acp2.kill(Duration::from_millis(200)).await.unwrap();

    // 原始形态（无 jsonrpc 键、无 sessionId）→ 仍 DroppedNoSessionId。
    let (acp3, mut rx3) = spawn_child(
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo '{\"foo\":\"bar\"}'".to_string(),
        ],
        "s3",
    )
    .await;
    let mut saw_dropped = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), rx3.recv()).await {
            Ok(Some(ChildOutput::DroppedNoSessionId)) => saw_dropped = true,
            Ok(Some(ChildOutput::Exit { .. })) | Ok(None) | Err(_) => break,
            _ => {}
        }
        if saw_dropped {
            break;
        }
    }
    assert!(
        saw_dropped,
        "原始形态（无 jsonrpc 键）无 sessionId 帧 → 缺口计数 DroppedNoSessionId"
    );
    acp3.kill(Duration::from_millis(200)).await.unwrap();
}

#[tokio::test]
async fn test_kill_idempotent_and_exit_event() {
    let cmd = vec![test_child(), "--crash-after".into(), "100".into()];
    let (acp, mut rx) = spawn_child(&cmd, "s1").await;
    let pgid = acp.pgid();

    acp.kill(Duration::from_millis(100)).await.unwrap();
    // 等待退出事件。
    let exit = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await {
                Some(ChildOutput::Exit { session_id, code }) => {
                    return (session_id, code);
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("应收到 Exit 事件");
    assert_eq!(exit.0, "s1");
    assert!(
        matches!(acp.state(), ProcessState::Exited(_)),
        "状态应迁移为 Exited"
    );

    // 已退出 → kill 立即成功（幂等，§4.5「已死成功返回」）。
    acp.kill(Duration::from_millis(100)).await.unwrap();
    // 进程组已不存在。
    assert!(!sys::kill_group(pgid, 0), "kill 后进程组应消失");
}

#[tokio::test]
async fn test_kill_group_kills_grandchildren() {
    // sh 起孙进程（sleep）：kill 后整组（含孙进程）一并终止（§4.1 进程组 kill）。
    let script = "sleep 30 & wait";
    let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    let (acp, mut rx) = spawn_child(&cmd, "g1").await;
    let pgid = acp.pgid();

    // 等孙进程就位。
    tokio::time::sleep(Duration::from_millis(300)).await;

    acp.kill(Duration::from_millis(200)).await.unwrap();

    let exit = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await {
                Some(ChildOutput::Exit { code, .. }) => return code,
                _ => continue,
            }
        }
    })
    .await
    .expect("应收到 Exit 事件");
    assert!(exit == 0 || exit == -1 || exit == 143, "退出码: {exit}");

    // 整棵进程树（含 sleep 孙进程）应已终止：进程组不存在。
    let mut probe = false;
    for _ in 0..20 {
        if !sys::kill_group(pgid, 0) {
            probe = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(probe, "进程组 kill 后孙进程（sleep）必须同组被杀");
}

#[tokio::test]
async fn test_write_line_after_exit_fails() {
    let cmd = vec![test_child(), "--crash-after".into(), "100".into()];
    let (acp, mut rx) = spawn_child(&cmd, "s1").await;
    acp.kill(Duration::from_millis(100)).await.unwrap();
    // 等退出。
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(ChildOutput::Exit { .. }) = rx.recv().await {
                break;
            }
        }
    })
    .await;
    assert!(
        acp.write_line(&serde_json::json!({"x": 1})).await.is_err(),
        "进程已退出 → 写失败（hub 上报失败语义，§4.4 L2）"
    );
}

/// #1 官方 `session/request_permission`（JSON-RPC **request**：有 id+method，
/// 无顶层 sessionId 字段）→ 按「有 jsonrpc 键」判据兜底转发（OQ6：解锁 #1
/// 端到端——child.rs 放行后 server relay 才能收到官方 request）。
#[tokio::test]
async fn request_permission_request_forwarded() {
    // 用 sh 直接吐裸 JSON（test_child 会对任意输入回 jsonrpc+id 响应，
    // 会多出一帧 response 干扰「仅转发 1 帧」断言）。
    let (acp, mut rx) = spawn_child(
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo '{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"session/request_permission\",\"params\":{\"toolCall\":{\"toolCallId\":\"tc1\",\"title\":\"run\"},\"options\":[{\"optionId\":\"o1\",\"name\":\"Allow once\",\"kind\":\"allow_once\"}]}}'".to_string(),
        ],
        "s4",
    )
    .await;
    let mut saw_dropped = false;
    let mut frames = Vec::new();
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ChildOutput::DroppedNoSessionId)) => saw_dropped = true,
            Ok(Some(ChildOutput::Frame(evt))) => frames.push(evt),
            Ok(Some(ChildOutput::Exit { .. })) | Ok(None) | Err(_) => break,
        }
        if !saw_dropped && !frames.is_empty() {
            break;
        }
    }
    assert!(!saw_dropped, "request 形态（有 jsonrpc 键）不得缺口丢弃");
    assert_eq!(
        frames.len(),
        1,
        "request_permission 应兜底转发（got={}）",
        frames.len()
    );
    assert_eq!(frames[0].session_id, "s4", "信封 session_id = 进程归属");
    assert_eq!(
        frames[0].frame["method"],
        serde_json::json!("session/request_permission"),
        "原帧透传"
    );
    assert_eq!(
        frames[0].frame["id"],
        serde_json::json!(5),
        "id 原样保留（number）"
    );
    acp.kill(Duration::from_millis(200)).await.unwrap();
}

/// kill_on_drop（§7.5 兜底语义）：runtime 销毁 → 子进程随 daemon 死亡。
#[test]
fn test_kill_on_drop_on_runtime_exit() {
    let pgid = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cmd = vec![test_child(), "--crash-after".into(), "100".into()];
            let (tx, _rx) = mpsc::unbounded_channel();
            let acp = child::spawn(&cmd, ".", None, "s1", tx).await.unwrap();
            let pgid = acp.pgid();
            // AcpProcess 与 rx 在 block_on 结束时 drop；Arc 仍被读任务持有，
            // runtime 销毁时任务中止 → Child drop → kill_on_drop。
            pgid
        })
    };
    // 等进程随 runtime 退出而终止。
    let mut dead = false;
    for _ in 0..40 {
        if !sys::kill_group(pgid, 0) {
            dead = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(dead, "runtime 退出后子进程必须被杀（kill_on_drop，§7.5）");
}
