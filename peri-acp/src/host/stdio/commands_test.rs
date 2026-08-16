//! Tests for `host/stdio/commands.rs`（P2-3：stdio 同步回调路径零测试缺口）。
//!
//! 与 `session/create_test.rs` 同构的双端 builder 驱动：agent 端 handler 直接
//! 调用 `send_available_commands`（`ConnectionTo`/`Responder` 无法在测试中
//! 直接实例化），client 端经 `on_receive_notification` 捕获 `SessionNotification`
//! 断言投影内容。stdio 路径的 on_change 回调为**同步发送**（区别于 notify
//! 路径的 tokio::spawn），本测试同时覆盖首发广播与回调重发两条发送路径。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{
        NewSessionRequest, NewSessionResponse, SessionId, SessionNotification, SessionUpdate,
    },
    Agent, Channel, Client, ConnectionTo,
};
use peri_acp_types::command::command_route::UiCommandSpec;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::PeriCaps;

use super::send_available_commands;
use crate::session::command::register_builtins;

/// stdio 同步回调路径：caps 带 `ui_commands` 明细 → 首发广播含 `ui:*` 条目
/// （`periKind=panel`）；注册表变更（unregister）触发**同步**回调重发且投影
/// 收缩（P2-3 stdio 接线断言；notify 路径为 tokio::spawn 异步发送，接线不同）。
#[tokio::test]
async fn test_stdio_send_available_commands_ui_entries_and_sync_resend() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionNotification>();
    // 闭包内等待通知到达（防连接提前关闭），断言内容经共享容器在闭包外进行。
    let captured: Arc<std::sync::Mutex<Vec<SessionNotification>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_inner = Arc::clone(&captured);
    let (channel_a, channel_b) = Channel::duplex();
    let caps = PeriCaps {
        ui_commands: vec![UiCommandSpec {
            name: "gallery".into(),
            description: "Open the gallery panel".into(),
            ..Default::default()
        }],
        ..Default::default()
    };

    let server = Agent
        .builder()
        .on_receive_request(
            {
                let caps = caps.clone();
                async move |_req: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                    let _ =
                        responder.respond(NewSessionResponse::new(SessionId::new("stdio-ui-test")));
                    let reg = Arc::new(CommandRegistry::new());
                    register_builtins(&reg);
                    send_available_commands(
                        &SessionId::new("stdio-ui-test"),
                        &cx,
                        &caps,
                        Some(Arc::clone(&reg)),
                    );
                    // 注册表变更 → 同步回调重发（stdio 路径非 tokio::spawn）。
                    assert!(reg.unregister("core:loop"), "unregister 应命中");
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    let result = Client
        .builder()
        .on_receive_notification(
            {
                let tx = tx.clone();
                async move |notif: SessionNotification, _cx| {
                    let _ = tx.send(notif);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(
            channel_a,
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _resp: NewSessionResponse = cx
                    .send_request(NewSessionRequest::new("/tmp"))
                    .block_task()
                    .await?;
                // 首发 + 回调重发两条通知（同步发送，无需轮询；超时防挂起）。
                let notif1 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .expect("首发通知超时")
                    .expect("首发通知应到达");
                let notif2 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .expect("回调重发通知超时")
                    .expect("回调重发通知应到达");
                assert_eq!(notif1.session_id.0.as_ref(), "stdio-ui-test");
                assert_eq!(notif2.session_id.0.as_ref(), "stdio-ui-test");
                captured_inner.lock().unwrap().push(notif1);
                captured_inner.lock().unwrap().push(notif2);
                Ok(())
            },
        )
        .await;

    assert!(result.is_ok(), "双端 builder 应成功: {result:?}");

    // 断言在连接结束后进行（通知内容已由 client 端捕获进共享容器）。
    let notifications = captured.lock().unwrap();
    let notif1 = notifications.first().expect("首发通知应已捕获");
    let notif2 = notifications.get(1).expect("回调重发通知应已捕获");

    // 首发：ui 明细条目 + 基座内置
    let SessionUpdate::AvailableCommandsUpdate(update1) = &notif1.update else {
        panic!("首发应为 available_commands_update");
    };
    let commands1 = &update1.available_commands;
    let names1: Vec<&str> = commands1.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names1.contains(&"gallery"),
        "ui 明细应随 caps 出现: {names1:?}"
    );
    let gallery = commands1
        .iter()
        .find(|c| c.name == "gallery")
        .expect("gallery 应存在");
    let meta1 = gallery.meta.clone().expect("ui 条目应带 _meta");
    assert_eq!(meta1["periKind"], "panel", "ui 条目 kind = panel");
    assert_eq!(meta1["periLevel"], 1, "core/ui 域 level = 1");
    assert_eq!(meta1["periCategory"], "ui");
    assert!(
        names1.contains(&"loop") && names1.contains(&"compact"),
        "基座内置应存在: {names1:?}"
    );

    // 回调重发：投影收缩（core:loop 已注销），ui 条目仍从注册表投影保留
    // （回调路径只重建投影、不重复注册——P2-1 回归面）。
    let SessionUpdate::AvailableCommandsUpdate(update2) = &notif2.update else {
        panic!("重发应为 available_commands_update");
    };
    let names2: Vec<&str> = update2
        .available_commands
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        !names2.contains(&"loop"),
        "unregister 后重发投影应收缩: {names2:?}"
    );
    assert!(
        names2.contains(&"gallery"),
        "重发投影应保留 ui 条目（注册表 snapshot 投影）: {names2:?}"
    );
}
