use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::service::serve_client_with_lifecycle;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::support::fake_engine::{Engine, Event, Role};
use crate::support::gateway::{rpc, Gateway};
use crate::support::monitor::Monitor;
use crate::support::wire::Wire;
use crate::{peri_config, peri_mode};

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn sse_resume_replays_a_reply_without_executing_the_write_twice() -> anyhow::Result<()> {
    let mut engine = Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&engine.url(), &[]).await?;
    let id = gateway.init().await?;
    gateway.connect(&id, &engine.url()).await?;
    let page = gateway.new_page(&id).await?;
    let domain = gateway.domain(&id).await?;
    let mut monitor = Monitor::open(&gateway, &domain).await?;
    assert_eq!(
        monitor.state(|s| s["view"]["pageId"] == page).await?["tabs"][0]["id"],
        page
    );
    drop(monitor);
    let gate = engine.gate_reply("Input.dispatchKeyEvent");
    let (request_id, message) = gateway.tool_request("press_key", json!({"key": "Enter"}));
    let mut response = gateway
        .post(Some(&id), message)
        .send()
        .await?
        .error_for_status()?;
    tokio::time::timeout(WAIT, gate.arrived.cancelled()).await?;
    let mut priming = String::new();
    let cursor = loop {
        let chunk = response.chunk().await?.expect("SDK priming event");
        priming.push_str(std::str::from_utf8(&chunk)?);
        if let Some((event, _)) = priming.split_once("\n\n") {
            if let Some(cursor) = event.lines().find_map(|line| line.strip_prefix("id:")) {
                break cursor.trim().to_owned();
            }
        }
    };
    // 先 attach 再放行引擎：rmcp 在投递回复的同一处删除该请求的 request-wise 通道，
    // 投递之后再 resume 只能得到 200 + 空 body。“回复只出现在 resumed 流”
    // 只有在回复投递前建立 resumed 流才是确定性的。
    //
    // attach 屏障不用 sleep：resume 会替换该请求的 sender（rmcp session/local.rs），
    // 旧 POST SSE body 随之 EOF。旧 body 结束是可观测的“已被接管”证据；
    // 固定等待既证明不了 attach 生效，也会在负载下重新引入同一竞态。
    let resumed = gateway
        .events(&id)
        .header("last-event-id", cursor)
        .send()
        .await?
        .error_for_status()?;
    let attached = tokio::time::timeout(WAIT, async {
        while response.chunk().await?.is_some() {}
        Ok::<(), anyhow::Error>(())
    })
    .await;
    match attached {
        Ok(ended) => ended?,
        Err(_) => anyhow::bail!("resume 未在 {WAIT:?} 内接管旧 POST 流：旧 SSE body 仍未结束"),
    }
    gate.release.cancel();
    let response = rpc(resumed).await?;
    assert_eq!(response["id"], request_id);
    assert_ne!(response["result"]["isError"], true);
    assert_eq!(engine.events().iter().filter(|event| matches!(event, Event::Command(Role::Writer, _, method, _) if method == "Input.dispatchKeyEvent")).count(), 2, "一次按键只发 down/up");
    assert_eq!(gateway.domain(&id).await?, domain);
    let mut reopened = Monitor::open(&gateway, &domain).await?;
    assert_eq!(
        reopened.state(|s| s["view"]["pageId"] == page).await?["tabs"][0]["id"],
        page
    );
    assert_eq!(
        gateway.success(&id, "list_pages", json!({})).await?["activePageId"],
        page
    );
    gateway.shutdown().await?;
    assert_eq!(u16::from(reopened.close_frame().await?.code), 4001);
    engine.shutdown().await
}

#[tokio::test]
async fn delete_revokes_admission_then_waits_for_writer_viewer_and_ws_even_if_one_caller_leaves(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, mut monitor) = live_with_writer().await?;
    let domain = gateway.domain(&id).await?;
    let monitor_path = gateway.monitor_path(&domain).await?;
    let mut sse = gateway.events(&id).send().await?.error_for_status()?;
    let gate = engine.gate_close();
    let mut delete = tokio::spawn(gateway.delete(&id).send());
    tokio::select! {
        result = &mut delete => anyhow::bail!("DELETE 在收到引擎 Close 应答前返回：{result:?}"),
        ready = tokio::time::timeout(WAIT, gate.arrived.cancelled()) => ready?,
    }
    assert_eq!(gateway.count().await?, 0);
    assert_eq!(gateway.events(&id).send().await?.status(), 404);
    assert_eq!(
        gateway
            .http
            .get(format!("{}{monitor_path}", gateway.base))
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(u16::from(monitor.close_frame().await?.code), 4001);
    while sse.chunk().await?.is_some() {}
    let mut duplicate = Box::pin(gateway.delete(&id).send());
    assert!(futures::poll!(&mut duplicate).is_pending());
    delete.abort();
    assert!(delete.await.unwrap_err().is_cancelled());
    gate.release.cancel();
    assert_eq!(duplicate.await?.status(), 202);
    engine
        .wait(|events| all_page_connections_ended(events, "target-1"))
        .await?;
    assert_eq!(engine.targets(), ["target-1"]);
    assert!(!engine.events().iter().any(|event| matches!(event, Event::Command(Role::Browser, _, method, _) if method == "Target.closeTarget")));
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn shutdown_retires_old_ids_and_tokens_without_closing_targets_or_replaying_writes(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, mut monitor) = live_with_writer().await?;
    let domain = gateway.domain(&id).await?;
    let old_url = gateway.monitor_path(&domain).await?;
    let gateway_addr = gateway.base.strip_prefix("http://").unwrap().to_owned();
    let mut sse = gateway.events(&id).send().await?.error_for_status()?;
    let (_request_id, request) =
        gateway.tool_request("request_human", json!({"prompt": "fixture shutdown"}));
    let mut response = gateway
        .post(Some(&id), request)
        .send()
        .await?
        .error_for_status()?;
    monitor
        .state(|s| s["handoff"]["prompt"] == "fixture shutdown")
        .await?;
    gateway.shutdown().await?;
    assert_eq!(u16::from(monitor.close_frame().await?.code), 4001);
    while sse.chunk().await?.is_some() {}
    while response.chunk().await?.is_some() {}
    engine
        .wait(|events| all_page_connections_ended(events, "target-1"))
        .await?;
    assert_eq!(engine.targets(), ["target-1"]);

    let mut replacement =
        Gateway::serve(TcpListener::bind(gateway_addr).await?, &engine.url(), &[]).await?;
    let (_, write) = replacement.tool_request("press_key", json!({"key": "Enter"}));
    assert_eq!(
        replacement
            .post(Some(&id), write.clone())
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(
        replacement
            .http
            .get(format!("{}{old_url}", replacement.base))
            .send()
            .await?
            .status(),
        404
    );
    let new_id = replacement.init().await?;
    assert_ne!(new_id, id);
    let status = replacement
        .success(&new_id, "browser_status", json!({}))
        .await?;
    assert_eq!(status["endpoint"], Value::Null);
    assert_eq!(status["pages"], json!([]));
    assert_ne!(status["sessionId"], domain);
    let reply = rpc(replacement.post(Some(&new_id), write).send().await?).await?;
    assert_eq!(
        reply["result"]["structuredContent"]["code"],
        "not_connected"
    );
    assert!(!engine.events().iter().any(|event| matches!(event, Event::Command(_, _, method, _) if method.starts_with("Input.") || method == "Target.closeTarget")));
    replacement.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn sdk_recovers_from_attached_post_404_without_replaying_a_write_against_old_pages(
) -> anyhow::Result<()> {
    let mut engine = Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&engine.url(), &[]).await?;
    let wire = Wire::start(&gateway).await?;
    let transport = StreamableHttpClientTransport::with_client(
        gateway.http.clone(),
        StreamableHttpClientTransportConfig::with_uri(wire.uri.as_str()),
    );
    let client = serve_client_with_lifecycle(peri_config(), transport, peri_mode()).await?;
    let old_id = wire
        .posts()
        .into_iter()
        .find_map(|post| post.returned_session)
        .expect("初始 HTTP session ID");
    gateway.connect(&old_id, &engine.url()).await?;
    gateway.new_page(&old_id).await?;
    let old_domain = gateway.domain(&old_id).await?;
    let old_monitor = gateway.monitor_path(&old_domain).await?;
    let start = wire.posts().len();
    assert_eq!(gateway.delete(&old_id).send().await?.status(), 202);
    let write = CallToolRequestParams::new("press_key")
        .with_arguments(json!({"key":"Enter"}).as_object().unwrap().clone());
    let result = tokio::time::timeout(WAIT, client.call_tool(write)).await??;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["code"], "not_connected");
    let posts = wire.posts();
    let posts = &posts[start..];
    let methods: Vec<_> = posts
        .iter()
        .map(|p| p.rpc_method.as_deref().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "tools/call",
            "initialize",
            "notifications/initialized",
            "tools/call"
        ]
    );
    assert_eq!(posts[0].status, 404);
    assert!(posts[0].sent_session.as_deref() == Some(old_id.as_str()));
    let new_id = posts[1]
        .returned_session
        .as_deref()
        .expect("恢复的 HTTP session ID");
    assert!(new_id != old_id);
    assert!(posts[2..]
        .iter()
        .all(|post| post.sent_session.as_deref() == Some(new_id)));
    let status = client
        .call_tool(CallToolRequestParams::new("browser_status"))
        .await?
        .structured_content
        .unwrap();
    assert_ne!(status["sessionId"], old_domain);
    assert!(status["endpoint"].is_null());
    assert_eq!(status["pages"], json!([]));
    assert_eq!(
        gateway
            .http
            .get(format!("{}{old_monitor}", gateway.base))
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(engine.targets(), ["target-1"]);
    assert!(!engine
        .events()
        .iter()
        .any(|event| matches!(event, Event::Command(Role::Writer, _, _, _))));
    client.cancel().await?;
    gateway.shutdown().await?;
    wire.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn delete_settles_when_the_engine_drops_tcp_without_a_close_reply() -> anyhow::Result<()> {
    let mut engine = Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&engine.url(), &[]).await?;
    let id = gateway.init().await?;
    gateway.connect(&id, &engine.url()).await?;
    gateway.new_page(&id).await?;
    gateway.success(&id, "take_snapshot", json!({})).await?;
    engine.reset_on_close();
    assert_eq!(gateway.delete(&id).send().await?.status(), 202);
    assert_eq!(gateway.count().await?, 0);
    engine
        .wait(|events| {
            events.iter().any(
                |event| matches!(event, Event::End(Role::Writer, target) if target == "target-1"),
            )
        })
        .await?;
    gateway.shutdown().await?;
    engine.shutdown().await
}

async fn live_with_writer() -> anyhow::Result<(Engine, Gateway, String, Monitor)> {
    let engine = Engine::ephemeral().await?;
    let gateway = Gateway::start(&engine.url(), &[]).await?;
    let id = gateway.init().await?;
    gateway.connect(&id, &engine.url()).await?;
    let page = gateway.new_page(&id).await?;
    gateway.success(&id, "take_snapshot", json!({})).await?;
    let domain = gateway.domain(&id).await?;
    let mut monitor = Monitor::open(&gateway, &domain).await?;
    let state = monitor.state(|s| s["view"]["pageId"] == page).await?;
    monitor.frame(&page, &state["view"]["viewEpoch"]).await?;
    Ok((engine, gateway, id, monitor))
}

fn all_page_connections_ended(events: &[Event], target: &str) -> bool {
    [Role::Writer, Role::Viewer].into_iter().all(|role| {
        let opened = events
            .iter()
            .filter(|event| matches!(event, Event::Open(kind, id) if *kind == role && id == target))
            .count();
        let ended = events
            .iter()
            .filter(|event| matches!(event, Event::End(kind, id) if *kind == role && id == target))
            .count();
        opened > 0 && opened == ended
    })
}
