//! child 集成测试：spawn/进程组/kill 幂等/进程组 kill 孙进程（T8）、kill_on_drop
//! （T12）、stdout 事件与缺口计数。
//!
//! 位于 `tests/`（integration）：`CARGO_BIN_EXE_test-child` 仅对集成测试可用。

use std::sync::Arc;
use std::time::Duration;

use acp_machine::child::{self, sys, AcpProcess, ChildOutput, ProcessState};
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
    // rpcId 匹配）→ 兜底归属 inner.session_id 转发（§6.1 is_json_rpc_response）。
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
    // 预期：5 帧全部 Frame（2 响应兜底 s1 + 3 通知 test-sid-001），无丢弃。
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
    assert_eq!(frames.len(), 5, "应收到 2 响应 + 3 通知（got={}", frames.len());
    assert_eq!(
        frames.iter().filter(|e| e.session_id == "test-sid-001").count(),
        3,
        "prompt 通知应携带 sessionId 并全部提取"
    );
    assert_eq!(
        frames.iter().filter(|e| e.session_id == "s1").count(),
        2,
        "无 sessionId 的 JSON-RPC 响应应兜底归属 inner.session_id"
    );
    // 兜底帧仍保持 response 形态（有 id、无 method）。
    assert!(
        frames.iter().any(|e| e.frame.get("id") == Some(&serde_json::json!(1))),
        "initialize response 应以原帧转发（rpcId=1）"
    );

    acp.kill(Duration::from_millis(200)).await.unwrap();

    // 缺口计数路径：非 JSON-RPC、无 sessionId 的帧 → DroppedNoSessionId。
    // （test_child 对任意输入都回 jsonrpc+id 响应，故用 sh 直接吐裸 JSON。）
    let (acp2, mut rx2) = spawn_child(
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo '{\"foo\":\"bar\"}'".to_string(),
        ],
        "s2",
    )
    .await;
    let mut saw_dropped = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), rx2.recv()).await {
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
        "非响应且无 sessionId 的帧 → 缺口计数 DroppedNoSessionId"
    );
    acp2.kill(Duration::from_millis(200)).await.unwrap();
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
    assert!(matches!(acp.state(), ProcessState::Exited(_)), "状态应迁移为 Exited");

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
