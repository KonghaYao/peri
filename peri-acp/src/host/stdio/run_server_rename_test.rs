use super::*;

/// `session/rename` 经 stdio wire 完整链路：initialize → session/new →
/// session/rename。通知先于响应（`handle_rename` 中
/// `send_session_info_update_with_title` 在响应返回前推送），通知携带
/// `SessionInfoUpdate.title`，响应往返 `{sessionId, title}`，thread store
/// 持久化标题——证明 stdio 与 TUI 共用统一 host 后 rename RPC 对 stdio 生效
/// （请求注册于 `host/requests.rs`）。
#[tokio::test]
async fn test_rename_over_stdio_transport() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = test_config(&tmp).await;
    let session_resources = Arc::clone(&cfg.session_resources);
    let (transport, mut input_write, mut output_read) = duplex_transport();
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    let server_task = tokio::spawn(host::run_acp_server(transport, cfg));

    send_initialize(&mut input_write, &mut output_read).await;

    // ── session/new ──
    let cwd = tmp.path().to_str().unwrap();
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": cwd }
        })
        .to_string(),
    )
    .await;
    let line = read_line(&mut output_read).await;
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 2, "session/new RequestId 往返: {resp}");
    assert!(resp.get("error").is_none(), "session/new 不应报错: {resp}");
    let session_id = resp["result"]["sessionId"]
        .as_str()
        .expect("session/new 返回非空 sessionId")
        .to_string();
    // 其他异步通知可以先到；等待本会话的命令目录更新。
    let mut commands_updated = false;
    for _ in 0..8 {
        let line = read_line(&mut output_read).await;
        let notif: Value = serde_json::from_str(&line).unwrap();
        assert!(
            notif.get("id").is_none(),
            "命令目录通知之前不应有响应: {notif}"
        );
        if notif["method"] == "session/update"
            && notif["params"]["update"]["sessionUpdate"] == "available_commands_update"
        {
            commands_updated = true;
            break;
        }
    }
    assert!(commands_updated, "session/new 须发布命令目录更新");

    // ── session/rename ──
    write_line(
        &mut input_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/rename",
            "params": { "sessionId": session_id, "title": "stdio 命名会话" }
        })
        .to_string(),
    )
    .await;

    // 通知先于响应（handle_rename 在返回前推送 SessionInfoUpdate）
    let mut rename_notice = None;
    for _ in 0..8 {
        let line = read_line(&mut output_read).await;
        let notif: Value = serde_json::from_str(&line).unwrap();
        assert!(
            notif.get("id").is_none(),
            "rename 通知前不应有响应: {notif}"
        );
        if notif["method"] == "session/update"
            && notif["params"]["update"]["sessionUpdate"] == "session_info_update"
        {
            rename_notice = Some(notif);
            break;
        }
    }
    let notif = rename_notice.expect("rename 须发布 session_info_update");
    assert_eq!(
        notif["method"], "session/update",
        "rename 通知 method: {notif}"
    );
    assert_eq!(notif["params"]["sessionId"], session_id);
    assert_eq!(
        notif["params"]["update"]["sessionUpdate"], "session_info_update",
        "SessionInfoUpdate 判别字符串: {notif}"
    );
    assert_eq!(notif["params"]["update"]["title"], "stdio 命名会话");

    // 响应往返 {sessionId, title}
    let line = read_line(&mut output_read).await;
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 3, "rename RequestId 往返: {resp}");
    assert!(resp.get("error").is_none(), "rename 不应报错: {resp}");
    assert_eq!(resp["result"]["sessionId"], session_id, "响应: {resp}");
    assert_eq!(resp["result"]["title"], "stdio 命名会话", "响应: {resp}");

    // 持久化：thread store 标题已更新
    let meta = session_resources
        .load_session_meta(&session_id)
        .await
        .unwrap();
    assert_eq!(meta.title.as_deref(), Some("stdio 命名会话"));

    // ── EOF → 宿主优雅退出 ──
    drop(input_write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
        .await
        .expect("run_acp_server 应在 stdin EOF 后退出")
        .expect("server task 不应 panic");
}
