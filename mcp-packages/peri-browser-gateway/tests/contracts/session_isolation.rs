use serde_json::{json, Value};

use crate::support::fake_engine::{Engine, Event, Role};
use crate::support::gateway::Gateway;
use crate::support::monitor::Monitor;

fn uid(snapshot: &Value) -> &str {
    snapshot["snapshot"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .find_map(|part| part.strip_prefix("uid="))
        .expect("AX 快照中的节点 UID")
}

#[tokio::test]
async fn shared_engine_keeps_mcp_pages_snapshots_and_monitor_views_in_their_own_session(
) -> anyhow::Result<()> {
    let mut engine = Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&engine.url(), &[]).await?;
    let (a, b) = tokio::try_join!(gateway.init(), gateway.init())?;
    assert_ne!(a, b);
    gateway.connect(&a, &engine.url()).await?;
    let empty = gateway.success(&b, "browser_status", json!({})).await?;
    assert_eq!(empty["connected"], false);
    assert_eq!(empty["pageCount"], 0);
    assert_eq!(empty["endpoint"], Value::Null);
    gateway
        .error_code(&b, "take_snapshot", json!({}), "not_connected")
        .await?;
    assert!(engine.targets().is_empty());
    gateway.connect(&b, &engine.url()).await?;

    let (pa, pb) = tokio::try_join!(gateway.new_page(&a), gateway.new_page(&b))?;
    assert_ne!(pa, pb);
    let (sa, sb) = tokio::try_join!(
        gateway.success(&a, "take_snapshot", json!({})),
        gateway.success(&b, "take_snapshot", json!({})),
    )?;
    assert_ne!(uid(&sa), uid(&sb), "相同 backend node 仍需不同命名空间");
    let pa2 = gateway.new_page(&a).await?;
    let sa2 = gateway.success(&a, "take_snapshot", json!({})).await?;
    let sa3 = gateway.success(&a, "take_snapshot", json!({})).await?;
    assert_ne!(uid(&sa2), uid(&sa3));
    assert_ne!(uid(&sa), uid(&sa3));
    let before = engine.events();
    for foreign in [&pa, &pa2] {
        gateway
            .error_code(
                &b,
                "navigate_page",
                json!({"pageId": foreign, "url": "https://fixture.test/denied"}),
                "page_not_found",
            )
            .await?;
    }
    gateway
        .error_code(&b, "close_page", json!({"pageId": &pa}), "page_not_found")
        .await?;
    gateway
        .error_code(
            &b,
            "select_page",
            json!({"pageId": &pb[..8]}),
            "page_not_found",
        )
        .await?;
    gateway
        .error_code(
            &a,
            "select_page",
            json!({"pageId": "target-1"}),
            "page_not_found",
        )
        .await?;
    for (session, stale) in [(&a, uid(&sa)), (&a, uid(&sa2)), (&b, uid(&sa3))] {
        gateway
            .error_code(session, "click", json!({"uid": stale}), "stale_uid")
            .await?;
    }
    assert!(!engine.events()[before.len()..].iter().any(|event| matches!(event,
        Event::Command(_, _, method, _) if method.starts_with("Input.") || method == "Runtime.evaluate" || method.starts_with("Target."))));

    let (da, db) = tokio::try_join!(gateway.domain(&a), gateway.domain(&b))?;
    assert_ne!(da, db);
    let (mut ma, mut ma2, mut mb) = tokio::try_join!(
        Monitor::open(&gateway, &da),
        Monitor::open(&gateway, &da),
        Monitor::open(&gateway, &db)
    )?;
    let va = ma.state(|state| state["view"]["pageId"] == pa2).await?;
    assert_eq!(va["tabs"].as_array().unwrap().len(), 2);
    assert!(va["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tab| tab["id"] == pa || tab["id"] == pa2));
    let vb = mb.state(|state| state["view"]["pageId"] == pb).await?;
    assert_eq!(vb["tabs"].as_array().unwrap().len(), 1);
    assert_eq!(vb["tabs"][0]["id"], pb);
    mb.frame(&pb, &vb["view"]["viewEpoch"]).await?;
    assert_eq!(
        ma.command(1, &va, json!({"t": "switch", "pageId": pa}))
            .await?["ok"],
        true
    );
    assert_eq!(
        gateway.success(&a, "list_pages", json!({})).await?["activePageId"],
        pa
    );
    let result = gateway
        .success(
            &a,
            "navigate_page",
            json!({"url": "https://fixture.test/implicit"}),
        )
        .await?;
    assert_eq!(result["url"], "https://fixture.test/implicit");
    let switched = ma2.state(|state| state["view"]["pageId"] == pa).await?;
    ma2.frame(&pa, &switched["view"]["viewEpoch"]).await?;
    gateway
        .success(&a, "select_page", json!({"pageId": pa2}))
        .await?;
    for monitor in [&mut ma, &mut ma2] {
        let state = monitor
            .state(|state| state["view"]["pageId"] == pa2 && state["view"] != va["view"])
            .await?;
        assert_eq!(state["interactive"], false);
    }
    assert_eq!(
        gateway.success(&b, "list_pages", json!({})).await?["activePageId"],
        pb
    );
    assert_eq!(
        gateway
            .success(&a, "close_page", json!({"pageId": pa2}))
            .await?["closed"],
        pa2
    );
    assert_eq!(engine.targets().len(), 2);
    let events = engine.events();
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Command(Role::Browser, _, method, _) if method == "Target.closeTarget")).count(), 1);
    assert!(!events.contains(&Event::Enumerate));
    gateway.shutdown().await?;
    assert_eq!(engine.targets().len(), 2);
    engine.shutdown().await
}

#[tokio::test]
async fn two_engines_may_return_the_same_target_id_without_aliasing_public_pages(
) -> anyhow::Result<()> {
    let mut first = Engine::ephemeral().await?;
    let mut second = Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&first.url(), &[]).await?;
    let (a, b) = tokio::try_join!(gateway.init(), gateway.init())?;
    let (first_url, second_url) = (first.url(), second.url());
    let (race1, race2) = tokio::try_join!(
        gateway.call(&a, "browser_connect", json!({"url": first_url})),
        gateway.call(&a, "browser_connect", json!({"url": second_url})),
    )?;
    let (winner, loser, other) = if race1["structuredContent"]["connected"] == true {
        (race1, race2, &second_url)
    } else {
        (race2, race1, &first_url)
    };
    assert_eq!(winner["structuredContent"]["connected"], true);
    assert_eq!(loser["structuredContent"]["code"], "already_connected");
    gateway.connect(&b, other).await?;
    let (sa, sb) = tokio::try_join!(
        gateway.success(&a, "take_snapshot", json!({})),
        gateway.success(&b, "take_snapshot", json!({})),
    )?;
    let pa = gateway.success(&a, "list_pages", json!({})).await?["activePageId"].clone();
    let pb = gateway.success(&b, "list_pages", json!({})).await?["activePageId"].clone();
    assert!(pa.as_str().unwrap().starts_with("p_"));
    assert_ne!(pa, pb);
    assert_eq!(first.targets(), ["target-1"]);
    assert_eq!(second.targets(), ["target-1"]);
    assert_ne!(uid(&sa), uid(&sb));
    gateway
        .error_code(&b, "select_page", json!({"pageId": pa}), "page_not_found")
        .await?;
    gateway
        .error_code(&a, "click", json!({"uid": uid(&sb)}), "stale_uid")
        .await?;
    gateway.shutdown().await?;
    first.shutdown().await?;
    second.shutdown().await
}
