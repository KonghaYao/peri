use std::time::Duration;

use serde_json::json;

use crate::support::fake_engine::{Engine, Event, Role};
use crate::support::gateway::{rpc, Gateway};
use crate::support::monitor::Monitor;

const WAIT: Duration = Duration::from_secs(5);

fn writers(events: &[Event], target: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Open(Role::Writer, id) if id == target))
        .count()
}

async fn fixture(
    env: &[(&str, &str)],
) -> anyhow::Result<(Engine, Gateway, String, String, Monitor)> {
    let engine = Engine::ephemeral().await?;
    let gateway = Gateway::start(&engine.url(), env).await?;
    let id = gateway.init().await?;
    gateway.connect(&id, &engine.url()).await?;
    let page = gateway.new_page(&id).await?;
    let domain = gateway.domain(&id).await?;
    let monitor = Monitor::open(&gateway, &domain).await?;
    Ok((engine, gateway, id, page, monitor))
}

#[tokio::test]
async fn takeover_waits_for_running_agent_and_reuses_only_the_settled_writer() -> anyhow::Result<()>
{
    let (mut engine, mut gateway, id, page, mut monitor) = fixture(&[]).await?;
    let view = monitor.state(|s| s["view"]["pageId"] == page).await?;
    let frame = monitor.frame(&page, &view["view"]["viewEpoch"]).await?;
    assert_eq!(frame["viewportWidth"], 640);
    assert_eq!(frame["deviceWidth"], 1280);
    let denied = monitor
        .command(
            1,
            &view,
            json!({"t": "navigate", "url": "https://fixture.test/denied"}),
        )
        .await?;
    assert_eq!(denied["code"], "read_only");

    let gate = engine.gate_reply("Input.dispatchKeyEvent");
    let mut agent = Box::pin(gateway.success(&id, "press_key", json!({"key": "Enter"})));
    tokio::select! {
        result = &mut agent => anyhow::bail!("按键在引擎回执前结束：{result:?}"),
        ready = tokio::time::timeout(WAIT, gate.arrived.cancelled()) => ready?,
    }
    let running = monitor.state(|s| s["agent"] == "driving").await?;
    monitor.send(2, &running, json!({"t": "takeover"})).await?;
    let pending = monitor.state(|s| s["pending"] == true).await?;
    assert_eq!(pending["interactive"], false);
    assert_ne!(pending["view"], running["view"]);
    let stale = monitor
        .command(3, &running, json!({"t": "text", "text": "stale fixture"}))
        .await?;
    assert_eq!(stale["code"], "stale_view");
    assert_eq!(writers(&engine.events(), "target-1"), 1);
    gate.release.cancel();
    agent.as_mut().await?;
    assert_eq!(monitor.result(2).await?["ok"], true);
    let human = monitor.state(|s| s["interactive"] == true).await?;
    monitor.frame(&page, &human["view"]["viewEpoch"]).await?;
    let navigation = monitor
        .command(
            4,
            &human,
            json!({"t": "navigate", "url": "https://fixture.test/human"}),
        )
        .await?;
    assert_eq!(navigation["ok"], true);
    assert_eq!(navigation["value"]["url"], "https://fixture.test/human");
    assert_eq!(
        monitor
            .command(
                5,
                &human,
                json!({"t": "key", "key": "a", "code": "KeyA", "type": "down", "text": "a"})
            )
            .await?["ok"],
        true
    );
    assert_eq!(
        monitor
            .command(
                6,
                &human,
                json!({"t": "mouse", "type": "down", "button": "left", "x": 100, "y": 200})
            )
            .await?["ok"],
        true
    );
    gateway
        .error_code(
            &id,
            "press_key",
            json!({"key": "Enter"}),
            "page_busy_by_human",
        )
        .await?;
    let events = engine.events();
    assert_eq!(writers(&events, "target-1"), 1);
    assert!(events.iter().any(|e| matches!(e, Event::Command(Role::Writer, _, method, p) if method == "Input.dispatchMouseEvent" && p["x"] == 50.0 && p["y"] == 100.0)));
    assert!(!events
        .iter()
        .any(|e| matches!(e, Event::Command(_, _, method, _) if method == "Input.insertText")));
    assert!(!events.iter().any(|e| matches!(e, Event::Command(Role::Viewer, _, method, _) if method.starts_with("Input.") || method == "Runtime.evaluate")));

    assert_eq!(
        monitor.command(7, &human, json!({"t": "release"})).await?["ok"],
        true
    );
    gateway
        .success(&id, "press_key", json!({"key": "Enter"}))
        .await?;
    assert_eq!(writers(&engine.events(), "target-1"), 2);
    drop(agent);
    drop(monitor);
    // 仍使用同一 MCP 会话结算，不向引擎发关 target。
    assert_eq!(gateway.delete(&id).send().await?.status(), 202);
    engine
        .wait(|events| events.contains(&Event::End(Role::Writer, "target-1".to_owned())))
        .await?;
    assert_eq!(engine.targets(), ["target-1"]);
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn devtools_reservation_waits_for_close_and_reports_the_external_409() -> anyhow::Result<()> {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::{Error, Message};

    let (mut engine, mut gateway, id, page, mut monitor) = fixture(&[]).await?;
    gateway.success(&id, "take_snapshot", json!({})).await?;
    let view = monitor
        .state(|s| s["view"]["pageId"] == page && s["view"]["viewEpoch"] != "1:0")
        .await?;
    let gate = engine.gate_close();
    monitor.send(1, &view, json!({"t": "devtools"})).await?;
    tokio::time::timeout(WAIT, gate.arrived.cancelled()).await?;
    let pending = monitor.state(|s| s["pending"] == true).await?;
    assert_eq!(pending["externalReserved"], false);
    {
        let mut result = std::pin::pin!(monitor.result(1));
        assert!(
            futures::poll!(&mut result).is_pending(),
            "未确认 Close 不可返回链接"
        );
    }
    gate.release.cancel();
    let receipt = monitor.result(1).await?;
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["value"]["pageId"], page);
    assert_eq!(
        receipt["value"]["url"],
        format!(
            "devtools://devtools/bundled/inspector.html?ws={}/devtools/page/target-1",
            engine.addr
        )
    );
    engine
        .wait(|events| events.contains(&Event::End(Role::Writer, "target-1".to_owned())))
        .await?;
    let reserved = monitor.state(|s| s["externalReserved"] == true).await?;
    assert_eq!(reserved["interactive"], false);
    gateway
        .error_code(
            &id,
            "press_key",
            json!({"key": "Enter"}),
            "page_busy_by_human",
        )
        .await?;

    let writer_url = format!("ws://{}/devtools/page/target-1", engine.addr);
    let (mut external, _) = tokio_tungstenite::connect_async(&writer_url).await?;
    let error = tokio_tungstenite::connect_async(&writer_url)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http(response) if response.status() == 409));
    let taken = monitor
        .command(2, &reserved, json!({"t": "takeover"}))
        .await?;
    assert_eq!(taken["ok"], false);
    assert_eq!(taken["code"], "another_controller");
    let refreshed = monitor
        .state(|s| s["externalReserved"] == true && s["view"] != reserved["view"])
        .await?;
    external.close(None).await?;
    assert!(matches!(
        tokio::time::timeout(WAIT, external.next()).await?,
        Some(Ok(Message::Close(_)))
    ));
    assert_eq!(
        monitor
            .command(3, &refreshed, json!({"t": "takeover"}))
            .await?["ok"],
        true
    );
    let control = monitor.state(|s| s["interactive"] == true).await?;
    assert_eq!(control["externalReserved"], false);
    assert_eq!(
        monitor
            .command(4, &control, json!({"t": "release"}))
            .await?["ok"],
        true
    );
    assert_eq!(writers(&engine.events(), "target-1"), 3);
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn abandoned_takeover_waits_for_late_attach_and_closes_it_before_next_writer(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, page, mut monitor) =
        fixture(&[("PERI_BROWSER_CONNECT_TIMEOUT_MS", "2000")]).await?;
    let mut observer = Monitor::open(&gateway, &gateway.domain(&id).await?).await?;
    let view = monitor.state(|s| s["view"]["pageId"] == page).await?;
    let upgrade = engine.gate_upgrade();
    monitor.send(1, &view, json!({"t": "takeover"})).await?;
    tokio::time::timeout(WAIT, upgrade.arrived.cancelled()).await?;
    let pending = observer.state(|s| s["pending"] == true).await?;
    drop(monitor);
    let abandoned = observer
        .state(|s| s["pending"] == false && s["view"] != pending["view"])
        .await?;
    assert_eq!(abandoned["interactive"], false);
    assert_eq!(writers(&engine.events(), "target-1"), 0);

    upgrade.release.cancel();
    engine
        .wait(|events| events.contains(&Event::End(Role::Writer, "target-1".to_owned())))
        .await?;
    let events = engine.events();
    assert_eq!(writers(&events, "target-1"), 1);
    assert!(events.contains(&Event::Close(Role::Writer, "target-1".to_owned())));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Event::Command(Role::Writer, _, _, _))));
    assert_eq!(
        observer
            .command(2, &abandoned, json!({"t": "takeover"}))
            .await?["ok"],
        true
    );
    let human = observer.state(|s| s["interactive"] == true).await?;
    assert_eq!(
        observer.command(3, &human, json!({"t": "release"})).await?["ok"],
        true
    );
    let events = engine.events();
    assert_eq!(writers(&events, "target-1"), 2);
    assert!(!events
        .iter()
        .any(|event| matches!(event, Event::Conflict(_))));
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn cancelled_agent_write_drains_old_socket_and_never_sends_the_next_key_event(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, page, mut monitor) = fixture(&[]).await?;
    let gate = engine.gate_reply("Input.dispatchKeyEvent");
    let (request_id, request) = gateway.tool_request("press_key", json!({"key": "Enter"}));
    let response = gateway
        .post(Some(&id), request)
        .send()
        .await?
        .error_for_status()?;
    tokio::time::timeout(WAIT, gate.arrived.cancelled()).await?;
    let running = monitor.state(|s| s["agent"] == "driving").await?;
    assert_eq!(running["view"]["pageId"], page);
    monitor.send(1, &running, json!({"t": "takeover"})).await?;
    let pending = monitor.state(|s| s["pending"] == true).await?;
    assert_eq!(pending["interactive"], false);
    let cancellation = json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":request_id}});
    assert_eq!(
        gateway.post(Some(&id), cancellation).send().await?.status(),
        202
    );
    // SDK 取消请求时回收对应 SSE 路由；不把流关闭当成引擎回执。
    let body = response.text().await?;
    assert!(!body.lines().any(|line| line
        .strip_prefix("data:")
        .is_some_and(|data| !data.trim().is_empty())));
    assert_eq!(monitor.result(1).await?["ok"], true);
    let human = monitor.state(|s| s["interactive"] == true).await?;
    let events = engine.events();
    assert_eq!(writers(&events, "target-1"), 2);
    let closed = events
        .iter()
        .position(|e| matches!(e, Event::Close(Role::Writer, _)))
        .expect("旧 writer 已收到 Close");
    let new = events
        .iter()
        .rposition(|e| matches!(e, Event::Open(Role::Writer, _)))
        .unwrap();
    assert!(closed < new, "第二次握手必须在旧 writer 关闭后");
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Command(Role::Writer, _, method, _) if method == "Input.dispatchKeyEvent")).count(), 1);
    assert_eq!(
        monitor.command(2, &human, json!({"t": "release"})).await?["ok"],
        true
    );
    engine
        .wait(|events| {
            events
                .iter()
                .filter(|event| matches!(event, Event::End(Role::Writer, _)))
                .count()
                == 2
        })
        .await?;
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn devtools_refuses_url_and_replacement_writer_when_close_is_unconfirmed(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, page, mut monitor) = fixture(&[]).await?;
    gateway.success(&id, "take_snapshot", json!({})).await?;
    let view = monitor
        .state(|s| s["view"]["pageId"] == page && s["view"]["viewEpoch"] != "1:0")
        .await?;
    let gate = engine.gate_close();
    monitor.send(1, &view, json!({"t": "devtools"})).await?;
    tokio::time::timeout(WAIT, gate.arrived.cancelled()).await?;
    let result = monitor.result(1).await?;
    assert_eq!(result["ok"], false);
    assert_eq!(result["code"], "cdp_close_unconfirmed");
    assert!(result["value"]["url"].is_null());
    let fenced = monitor
        .state(|s| s["pending"] == false && s["view"] != view["view"])
        .await?;
    assert_eq!(fenced["interactive"], false);
    assert_eq!(fenced["externalReserved"], false);
    assert_eq!(
        monitor
            .command(2, &fenced, json!({"t": "takeover"}))
            .await?["code"],
        "session_closed"
    );
    gateway
        .error_code(&id, "press_key", json!({"key": "Enter"}), "session_closed")
        .await?;
    assert_eq!(writers(&engine.events(), "target-1"), 1);
    assert_eq!(gateway.delete(&id).send().await?.status(), 500);
    assert_eq!(u16::from(monitor.close_frame().await?.code), 4001);
    gate.release.cancel();
    engine
        .wait(|events| events.contains(&Event::End(Role::Writer, "target-1".to_owned())))
        .await?;
    let failed = gateway.shutdown().await.unwrap_err();
    assert!(failed
        .to_string()
        .contains("engine did not confirm release"));
    engine.shutdown().await
}

#[tokio::test]
async fn accepted_declined_timed_out_and_cancelled_human_requests_are_correlated(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, page, mut monitor) = fixture(&[]).await?;
    let initial = monitor.state(|s| s["view"]["pageId"] == page).await?;
    let (done, done_view) = async {
        let request = gateway.success(
            &id,
            "request_human",
            json!({"prompt": "fixture done", "timeoutMs": 3000}),
        );
        let human = async {
            let offer = monitor
                .state(|s| s["handoff"]["prompt"] == "fixture done")
                .await?;
            let request_id = offer["handoff"]["requestId"].clone();
            assert_eq!(offer["agent"], "waiting_for_human");
            gateway
                .error_code(
                    &id,
                    "press_key",
                    json!({"key": "Enter"}),
                    "waiting_for_human",
                )
                .await?;
            assert_eq!(
                monitor.command(1, &offer, json!({"t": "devtools"})).await?["code"],
                "waiting_for_human"
            );
            assert_eq!(
                monitor
                    .command(
                        2,
                        &offer,
                        json!({"t": "answer", "accepted": true, "requestId": request_id})
                    )
                    .await?["ok"],
                true
            );
            let human = monitor.state(|s| s["handoff"]["accepted"] == true).await?;
            assert_eq!(
                monitor
                    .command(3, &human, json!({"t": "text", "text": "fixture handoff"}))
                    .await?["ok"],
                true
            );
            assert_eq!(
                monitor
                    .command(4, &human, json!({"t": "done", "requestId": request_id}))
                    .await?["ok"],
                true
            );
            let ended = monitor
                .state(|s| s["handoff"].is_null() && s["view"] != initial["view"])
                .await?;
            assert_eq!(
                monitor
                    .command(
                        5,
                        &ended,
                        json!({"t": "answer", "requestId": request_id, "accepted": true})
                    )
                    .await?["code"],
                "stale_view"
            );
            Ok::<_, anyhow::Error>(ended)
        };
        tokio::try_join!(request, human)
    }
    .await?;
    assert_eq!(done["status"], "done");
    assert_eq!(done["pageId"], page);
    assert_eq!(done["title"], "Fixture");
    assert_eq!(done["screenshot"]["data"], "/9j/2Q==");

    let request = gateway.success(
        &id,
        "request_human",
        json!({"prompt": "fixture decline", "timeoutMs": 3000}),
    );
    let decline = async {
        let view = monitor
            .state(|s| s["handoff"]["prompt"] == "fixture decline")
            .await?;
        assert_eq!(monitor.command(6, &view, json!({"t": "answer", "requestId": view["handoff"]["requestId"], "accepted": false})).await?["ok"], true);
        Ok::<_, anyhow::Error>(())
    };
    let (declined, ()) = tokio::try_join!(request, decline)?;
    assert_eq!(declined["reason"], "declined");
    let request = gateway.success(
        &id,
        "request_human",
        json!({"prompt": "fixture timeout", "timeoutMs": 600}),
    );
    let accept = async {
        let view = monitor
            .state(|s| s["handoff"]["prompt"] == "fixture timeout")
            .await?;
        assert_eq!(monitor.command(7, &view, json!({"t": "answer", "requestId": view["handoff"]["requestId"], "accepted": true})).await?["ok"], true);
        Ok::<_, anyhow::Error>(())
    };
    let (expired, ()) = tokio::try_join!(request, accept)?;
    assert_eq!(expired["status"], "no_human");
    assert_eq!(expired["reason"], "timeout");
    let view = monitor
        .state(|s| s["handoff"].is_null() && s["view"] != done_view["view"])
        .await?;
    assert_eq!(view["interactive"], true, "超时不强抢人工租约");
    assert_eq!(
        monitor.command(8, &view, json!({"t": "release"})).await?["ok"],
        true
    );

    let (request_id, message) = gateway.tool_request(
        "request_human",
        json!({"prompt": "fixture cancel", "timeoutMs": 4000}),
    );
    let pending = async { rpc(gateway.post(Some(&id), message).send().await?).await };
    let cancel = async {
        let offer = monitor
            .state(|s| s["handoff"]["prompt"] == "fixture cancel")
            .await?;
        let cancel = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": request_id}});
        assert_eq!(gateway.post(Some(&id), cancel).send().await?.status(), 202);
        let cleared = monitor.state(|s| s["handoff"].is_null()).await?;
        assert_eq!(monitor.command(9, &cleared, json!({"t": "answer", "requestId": offer["handoff"]["requestId"], "accepted": true})).await?["code"], "stale_view");
        Ok::<_, anyhow::Error>(())
    };
    let (cancelled, cleared) = tokio::join!(pending, cancel);
    cleared?;
    if let Ok(response) = cancelled {
        assert!(response.get("result").is_none());
    }
    gateway
        .success(&id, "press_key", json!({"key": "Enter"}))
        .await?;
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn automatic_idle_can_reclaim_but_manual_lease_survives_until_last_viewer_leaves(
) -> anyhow::Result<()> {
    let (mut engine, mut gateway, id, page, mut first) = fixture(&[
        ("PERI_BROWSER_IDLE_YIELD_MS", "100"),
        ("PERI_BROWSER_YIELD_TIMEOUT_MS", "500"),
    ])
    .await?;
    gateway.success(&id, "take_snapshot", json!({})).await?;
    let auto = first.state(|s| s["interactive"] == true).await?;
    assert_eq!(auto["view"]["pageId"], page);
    gateway
        .success(&id, "press_key", json!({"key": "Enter"}))
        .await?;
    assert_eq!(writers(&engine.events(), "target-1"), 1);
    let agent = first
        .state(|s| s["interactive"] == false && s["view"] != auto["view"])
        .await?;
    assert_eq!(
        first.command(1, &agent, json!({"t": "takeover"})).await?["ok"],
        true
    );
    let domain = gateway.domain(&id).await?;
    let mut second = Monitor::open(&gateway, &domain).await?;
    let manual = second.state(|s| s["interactive"] == true).await?;
    drop(first);
    gateway
        .error_code(
            &id,
            "press_key",
            json!({"key": "Enter"}),
            "page_busy_by_human",
        )
        .await?;
    assert_eq!(
        second
            .command(
                2,
                &manual,
                json!({"t": "text", "text": "fixture second viewer"})
            )
            .await?["ok"],
        true
    );
    drop(second);
    engine
        .wait(|events| events.contains(&Event::End(Role::Writer, "target-1".to_owned())))
        .await?;
    let mut reopened = Monitor::open(&gateway, &domain).await?;
    assert_eq!(
        reopened
            .state(|s| s["view"]["pageId"] == page && s["pending"] == false)
            .await?["interactive"],
        false
    );
    drop(reopened);
    let unclaimed = gateway.success(&id, "yield_page", json!({})).await?;
    assert_eq!(
        unclaimed,
        json!({"pageId": page, "reclaimed": false, "status": "yield_pending"})
    );
    gateway.success(&id, "take_snapshot", json!({})).await?;
    engine
        .wait(|events| {
            events
                .iter()
                .filter(|e| matches!(e, Event::End(Role::Writer, _)))
                .count()
                >= 2
        })
        .await?;
    let mut monitor = Monitor::open(&gateway, &domain).await?;
    let view = monitor
        .state(|s| s["view"]["pageId"] == page && s["pending"] == false)
        .await?;
    assert_eq!(
        monitor.command(3, &view, json!({"t": "takeover"})).await?["ok"],
        true
    );
    let human = monitor.state(|s| s["interactive"] == true).await?;
    let reclaim = gateway.success(&id, "yield_page", json!({}));
    let release = async {
        monitor.state(|s| s["agent"] == "waiting_for_human").await?;
        assert_eq!(
            monitor.command(4, &human, json!({"t": "release"})).await?["ok"],
            true
        );
        Ok::<_, anyhow::Error>(())
    };
    let (reclaimed, ()) = tokio::try_join!(reclaim, release)?;
    assert_eq!(reclaimed["reclaimed"], true);
    assert_eq!(reclaimed["status"], "reclaimed");
    gateway.shutdown().await?;
    engine.shutdown().await
}
