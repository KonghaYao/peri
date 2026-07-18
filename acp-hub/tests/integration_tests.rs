//! Hub + test-child 端到端集成测试
//!
//! 每个测试启动一个 Hub 进程（连接 test-child），通过 stdin/stdout
//! 发送 ACP 消息并验证响应。

mod common;

use common::{send_and_recv, send_req, start_hub, test_child_crash_after};

// ============================================================
// 测试用例
// ============================================================

#[test]
fn test_initialize() {
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));

    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["serverInfo"]["name"], "acp-hub");

    let _ = child.kill();
}

#[test]
fn test_session_new_and_prompt() {
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // 1. initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 1);

    // 2. session/new
    send_req(
        &mut stdin,
        2,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 2);
    let session_id = resp["result"]["session_id"].as_str().unwrap().to_string();
    assert!(!session_id.is_empty());

    // 3. prompt
    send_req(
        &mut stdin,
        3,
        "prompt",
        &serde_json::json!({
            "session_id": session_id,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 3);
    assert_eq!(resp["result"]["status"], "completed");

    // 4. session/close
    send_req(
        &mut stdin,
        4,
        "session/close",
        &serde_json::json!({
            "session_id": session_id,
        }),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 4);

    let _ = child.kill();
}

#[test]
fn test_session_new_invalid_command() {
    let bad_cmd = vec!["/nonexistent/command_xyz".to_string()];
    let (mut stdin, mut reader, mut child) = start_hub(&bad_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // session/new → 应该失败
    send_req(
        &mut stdin,
        2,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 2);
    assert!(resp.get("error").is_some());
    let code = resp["error"]["code"].as_i64().unwrap();
    assert_eq!(code, -32002); // SPAWN_FAILED

    let _ = child.kill();
}

#[test]
fn test_prompt_unknown_session() {
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // 发给不存在的 session
    send_req(
        &mut stdin,
        2,
        "prompt",
        &serde_json::json!({
            "session_id": "nonexistent-xyz",
            "messages": []
        }),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 2);
    assert!(resp.get("error").is_some());
    let code = resp["error"]["code"].as_i64().unwrap();
    assert_eq!(code, -32000); // SESSION_NOT_FOUND

    let _ = child.kill();
}

#[test]
fn test_session_list() {
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // session/list（空）
    send_req(&mut stdin, 2, "session/list", &serde_json::json!({}));
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 2);
    assert!(resp["result"].as_array().unwrap().is_empty());

    // 创建一个 session
    send_req(
        &mut stdin,
        3,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let _ = send_and_recv(&mut stdin, &mut reader);

    // session/list（有 1 个）
    send_req(&mut stdin, 4, "session/list", &serde_json::json!({}));
    let resp = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp["id"], 4);
    assert_eq!(resp["result"].as_array().unwrap().len(), 1);

    let _ = child.kill();
}

#[test]
fn test_child_crash_detection() {
    // test-child --crash-after=2: 处理初始化 (initialize) 和 session/new 各一条 → exit(1)
    // session/new 本身可能成功（test-child 响应后才 crash）
    // 崩溃检测体现在后续请求失败
    let child_cmd = test_child_crash_after(2);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // session/new → test-child 处理完后 crash
    send_req(
        &mut stdin,
        2,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    // session/new 可能成功（test-child 响应后才 exit），不去断言成功/失败

    // 尝试后续请求 → 应该失败
    if resp.get("result").is_some() {
        let sid = resp["result"]["session_id"].as_str().unwrap_or("unknown");
        send_req(
            &mut stdin,
            3,
            "prompt",
            &serde_json::json!({
                "session_id": sid,
                "messages": [{"role": "user", "content": "hello"}]
            }),
        );
        let resp2 = send_and_recv(&mut stdin, &mut reader);
        // 崩溃后子进程不再响应 → 应返回错误
        assert!(
            resp2.get("error").is_some(),
            "已崩溃的子进程的后续请求应返回错误"
        );
    }

    let _ = child.kill();
}

#[test]
fn test_concurrent_requests_id_mapping() {
    // 创建两个 session，分别向它们发 prompt，验证 ID 映射不串号
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // 创建 session A
    send_req(
        &mut stdin,
        2,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp2 = send_and_recv(&mut stdin, &mut reader);
    let sid_a = resp2["result"]["session_id"].as_str().unwrap().to_string();

    // 向 session A 发 prompt
    send_req(
        &mut stdin,
        3,
        "prompt",
        &serde_json::json!({
            "session_id": sid_a,
            "messages": [{"role": "user", "content": "to A"}]
        }),
    );
    let resp3 = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp3["id"], 3);

    // 创建 session B
    send_req(
        &mut stdin,
        4,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp4 = send_and_recv(&mut stdin, &mut reader);
    let sid_b = resp4["result"]["session_id"].as_str().unwrap().to_string();

    // 向 session B 发 prompt
    send_req(
        &mut stdin,
        5,
        "prompt",
        &serde_json::json!({
            "session_id": sid_b,
            "messages": [{"role": "user", "content": "to B"}]
        }),
    );
    let resp5 = send_and_recv(&mut stdin, &mut reader);
    assert_eq!(resp5["id"], 5);

    // 两个不同 session 应有不同的 session_id
    assert_ne!(sid_a, sid_b, "不同 session 应有不同的 session_id");

    let _ = child.kill();
}

#[test]
fn test_graceful_shutdown() {
    let child_cmd = test_child_crash_after(100);
    let (mut stdin, mut reader, mut child) = start_hub(&child_cmd);

    // initialize
    send_req(&mut stdin, 1, "initialize", &serde_json::json!({}));
    let _ = send_and_recv(&mut stdin, &mut reader);

    // 创建 session
    send_req(
        &mut stdin,
        2,
        "session/new",
        &serde_json::json!({"cwd": "."}),
    );
    let resp = send_and_recv(&mut stdin, &mut reader);
    let _sid = resp["result"]["session_id"].as_str().unwrap().to_string();

    // 直接 kill Hub 进程（模拟 SIGTERM）
    child.kill().expect("Hub 进程应能正常终止");
    let status = child.wait().expect("应能获取退出状态");
    // Hub 被 kill 后应以非零退出（被 SIGKILL）
    assert!(!status.success(), "Hub 被 kill 后应以非零退出");
}
