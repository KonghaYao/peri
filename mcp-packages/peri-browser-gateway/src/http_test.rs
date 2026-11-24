use serde_json::Value;

use super::*;

#[test]
fn escape_html_covers_the_dangerous_characters() {
    assert_eq!(
        escape_html(r#"<a href="x">'&"#),
        "&lt;a href=&quot;x&quot;&gt;&#39;&amp;"
    );
    assert_eq!(escape_html("plain"), "plain");
}

#[test]
fn authority_must_name_the_actual_listener_and_monitor_origin_must_match() {
    let bound = "127.0.0.1:10001".parse().unwrap();
    for (uri, host, origin, allowed) in [
        ("/", "localhost:10001", Some("http://localhost:10001"), true),
        ("/s/fixture/ws", "127.0.0.1:10001", None, true),
        (
            "/s/fixture/ws",
            "127.0.0.1:10001",
            Some("http://127.0.0.1:10001"),
            true,
        ),
        (
            "/s/fixture/ws",
            "127.0.0.1:10001",
            Some("http://localhost:10001"),
            false,
        ),
        ("/s/fixture/ws", "127.0.0.1:10001", Some("null"), false),
        (
            "/s/fixture/ws",
            "127.0.0.1:10001",
            Some("http://127.0.0.1:10001/"),
            false,
        ),
        ("/", "127.0.0.1:10002", None, false),
        ("/", "127.0.0.1:10001@remote.invalid", None, false),
        ("/", "remote.invalid:10001", None, false),
        (
            "http://remote.invalid:10001/",
            "localhost:10001",
            None,
            false,
        ),
    ] {
        let mut request = Request::builder()
            .uri(uri)
            .header("host", host)
            .header("x-forwarded-host", "localhost:10001");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(
            allowed_authority(&request.body(axum::body::Body::empty()).unwrap(), bound),
            allowed,
            "{uri} {host} {origin:?}"
        );
    }
    let duplicate = Request::builder()
        .uri("/")
        .header("host", "localhost:10001")
        .header("host", "localhost:10001")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(!allowed_authority(&duplicate, bound));
}

#[tokio::test]
async fn sdk_expiry_revokes_even_a_live_monitor_and_drains_the_transport() -> anyhow::Result<()> {
    let config = Config::parse_from([], |key| match key {
        "HOME" => Some("/fixture".to_owned()),
        "PERI_BROWSER_SESSION_NAME" => Some("<script>fixture</script>".to_owned()),
        _ => None,
    })?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let bound = listener.local_addr()?;
    let base = format!("http://{bound}");
    let directory = Arc::new(SessionDirectory::new(
        config.browser,
        format!("{base}/"),
        config.secret,
    ));
    let stop = CancellationToken::new();
    let _on_exit = stop.clone().drop_guard();
    let server = tokio::spawn(serve_with_sessions(
        listener,
        bound,
        directory.clone(),
        Duration::from_secs(1),
        stop.clone().cancelled_owned(),
    ));
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    assert!(http
        .get(&base)
        .send()
        .await?
        .text()
        .await?
        .contains("no sessions"));
    let response = http.post(format!("{base}/mcp")).header("accept", "application/json, text/event-stream").json(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}
        }
    })).send().await?.error_for_status()?;
    let id = response.headers()["mcp-session-id"].to_str()?.to_owned();
    let body = response.text().await?;
    assert!(body.contains("peri-browser-mcp"));
    let index = http
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    assert!(index.contains("&lt;script&gt;fixture&lt;/script&gt;"));
    assert!(!index.contains("<script>fixture</script>"));
    let (token, core) = directory.list().pop().unwrap();
    let url = format!("{base}/s/{token}/");
    let monitor = http.get(&url).send().await?.error_for_status()?;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
    ] {
        assert_eq!(monitor.headers()[name], value);
    }
    assert_eq!(http.get(format!("{url}ws")).send().await?.status(), 400);
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{bound}/s/{token}/ws")).await?;
    use futures::StreamExt;
    let closed = tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(frame) = socket.next().await {
            if let tokio_tungstenite::tungstenite::Message::Close(frame) = frame? {
                return Ok::<_, anyhow::Error>(frame);
            }
        }
        anyhow::bail!("monitor 未收到 session ended")
    })
    .await??;
    assert_eq!(u16::from(closed.unwrap().code), 4001);
    assert!(
        core.stop.is_cancelled(),
        "仅持有 core 引用不能保活或保留准入"
    );
    assert_eq!(http.get(&url).send().await?.status(), 404);
    assert_eq!(
        http.get(format!("{base}/mcp"))
            .header("accept", "text/event-stream")
            .header("mcp-session-id", id)
            .send()
            .await?
            .status(),
        404
    );
    let health: Value = http
        .get(format!("{base}/healthz"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        health,
        json!({"ok": true, "sessions": 0, "pid": std::process::id()})
    );
    stop.cancel();
    server.await??;
    Ok(())
}

#[tokio::test]
async fn explicit_stop_reaps_a_router_with_no_sessions() -> anyhow::Result<()> {
    let config = Config::parse_from([], |key| (key == "HOME").then(|| "/fixture".to_owned()))?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let bound = listener.local_addr()?;
    serve(listener, config, std::future::ready(())).await?;
    assert!(tokio::net::TcpStream::connect(bound).await.is_err());
    Ok(())
}

#[test]
fn monitor_asset_retains_dedicated_handoff_devtools_and_ime_controls() {
    for needle in [
        "id=\"takeover\"",
        "id=\"release\"",
        "id=\"handoffDone\"",
        "requestId: state.handoff.requestId",
        "id=\"imeInput\"",
        "compositionend",
        "pointer-events: none;\n    width: 1px;",
        "id=\"devtoolsPop\"",
        "id=\"devtoolsOpen\"",
        "id=\"devtoolsCopy\"",
        "{ t: \"devtools\" }",
        "case \"state\":",
        "case \"result\":",
        "event.code === 4001",
        "view: state.view",
    ] {
        assert!(MONITOR_HTML.contains(needle), "monitor.html 缺少 {needle}");
    }
    for needle in [
        "id=\"drawer\"",
        "diagnosticsPage",
        "id=\"diagBuiltin\"",
        "id=\"consoleList\"",
        "id=\"networkList\"",
        "id=\"diagCount\"",
        "id=\"drawerResize\"",
        "requestId: \"current\"",
        "meta.viewportWidth",
    ] {
        assert!(
            !MONITOR_HTML.contains(needle),
            "monitor.html 不应包含 {needle}"
        );
    }
}
