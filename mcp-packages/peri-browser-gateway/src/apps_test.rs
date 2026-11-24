use std::path::PathBuf;
use std::time::Duration;

use axum::body::to_bytes;
use tokio_util::sync::CancellationToken;

use super::*;

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

fn sandbox_url() -> &'static str {
    "http://127.0.0.1:7392/apps/sandbox"
}

/// 单文件 App fixture：bridge 必须落在 head 里、App 模块脚本之前。
const APP_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>fixture app</title>
<script type="module">/*app bundle*/</script>
</head>
<body><p id="app">real app</p></body>
</html>
"#;

async fn serve_document(
    document: AppDocument,
) -> anyhow::Result<(
    String,
    CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let stop = CancellationToken::new();
    let documents = Arc::new(AppDocuments::new(document));
    let server = tokio::spawn(serve(listener, documents, stop.clone().cancelled_owned()));
    Ok((base, stop, server))
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("test client")
}

async fn finish(stop: CancellationToken, server: tokio::task::JoinHandle<anyhow::Result<()>>) {
    stop.cancel();
    server
        .await
        .expect("server task joins")
        .expect("server stops cleanly");
}

#[tokio::test]
async fn host_page_embeds_the_configured_sandbox_and_records_messages() {
    let response = host_page(Some(sandbox_url()));
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(response.headers().get("content-security-policy").is_none());
    let body = body_text(response).await;
    assert!(body.contains(&format!(
        r#"<iframe id="f" src="{}" sandbox="allow-scripts""#,
        sandbox_url()
    )));
    assert!(body.contains("window.__msgs.push(event.data)"));
    // 宿主页把 bridge 的通信目标指向 iframe；App 视图不设置该函数。
    assert!(body.contains("window.__periBridgeTarget = function ()"));
    assert!(body.contains("frame.contentWindow"));
}

#[tokio::test]
async fn disabled_sandbox_keeps_the_host_page_without_an_iframe() {
    let body = body_text(host_page(None)).await;
    assert!(body.contains("sandbox is disabled"));
    assert!(!body.contains("<iframe"));
    assert!(body.contains("window.__msgs"));
}

#[tokio::test]
async fn host_page_wires_the_app_bridge_and_forwards_app_tool_calls() {
    let body = body_text(host_page(Some(sandbox_url()))).await;
    for needle in [
        "window.__bridge",
        "sendToolInput",
        "sendToolResult",
        "ui/initialize",
        "ui/notifications/initialized",
        "ui/notifications/tool-input",
        "ui/notifications/tool-result",
        "window.__toolCalls.push(params)",
        "window.__onToolCall",
        "no __onToolCall handler",
        "tools/call",
        "window.__log.push",
        // 只信任 App 的 window，而不是 origin 字符串：opaque origin 是 "null"。
        "event.source !== win",
        "hostCapabilities",
        "hostContext",
    ] {
        assert!(body.contains(needle), "host page is missing {needle:?}");
    }
    assert!(body.contains(&format!(
        r#"{{ name: "peri-browser", version: "{}" }}"#,
        crate::version()
    )));
}

#[tokio::test]
async fn host_page_sends_pointer_messages_only_when_click_is_called() {
    let body = body_text(host_page(Some(sandbox_url()))).await;
    // 指针消息只有 __bridge.click 一处发送点（日志 + post），页面加载不会自动
    // 点任何坐标。
    assert_eq!(body.matches("type: \"peri:pointer\"").count(), 1);
    assert!(body.contains("click: function (x, y)"));
    assert!(!body.contains("setTimeout"));
}

#[tokio::test]
async fn app_view_puts_the_bridge_in_head_before_the_app_bundle() {
    let document = AppDocument::from_app_html(APP_HTML);
    let body = document.view_body();
    let head = body.find("<head>").expect("head stays in place");
    let bridge = body.find("window.__bridge =").expect("bridge injected");
    let bundle = body.find("/*app bundle*/").expect("app bundle kept");
    assert!(head < bridge, "bridge must be inside head");
    assert!(
        bridge < bundle,
        "classic bridge must run before the module app"
    );
    assert!(body.contains("id=\"app\""));
    // 顶层视图用真实 CDP 输入，不注入 sandbox 指针 shim。
    assert!(!body.contains("document.elementFromPoint"));
    assert!(!body.contains("peri:sandbox-ready"));
    // doctype 仍在首位，注入不会切到 quirks mode。
    assert!(body.starts_with("<!doctype html>"));
}

#[tokio::test]
async fn app_view_bridge_is_classic_and_answers_the_sdk_protocol() {
    let bridge = bridge_script();
    assert!(bridge.starts_with("<script>"));
    assert!(!bridge.contains("type=\"module\""));
    for needle in [
        // McpUiInitializeResult 字段（ext-apps 1.7.5 spec.types.d.ts）。
        "protocolVersion: version",
        "hostInfo: HOST_INFO",
        "hostCapabilities: { serverTools: {}, logging: {}, message: {} }",
        "hostContext: hostContext()",
        "availableDisplayModes",
        "containerDimensions",
        // 通知与工具往返。
        "ui/notifications/initialized",
        "window.__bridge.initialized = true",
        "ui/notifications/tool-input",
        "ui/notifications/tool-result",
        "window.__toolCalls.push(params)",
        "window.__onToolCall",
        // 同步回复：App 自己的 MCP client 会对出站请求同步回 -32601，
        // 微任务里回复会丢掉竞态。
        "result = handler(params);",
        "if (isThenable(result))",
        "replyResult(id, result)",
        // 错误码：未注入 handler 与 handler 失败。
        "\"tools/call\", -32601",
        "\"tools/call\", -32603",
        // 同文档模式：源窗口就是自身，只能靠 method 字段过滤自己的响应。
        "typeof data.method !== \"string\"",
        "event.source !== win",
        "window.__log.push",
        "appWindow()",
    ] {
        assert!(bridge.contains(needle), "bridge is missing {needle:?}");
    }
    // __bridge 必须先就绪，再开始接收消息（initialized 标志依赖它）。
    let assigned = bridge.find("window.__bridge = {").expect("bridge object");
    let listening = bridge
        .find("window.addEventListener(\"message\", onMessage)")
        .expect("listener");
    assert!(assigned < listening);
}

#[tokio::test]
async fn bridge_injection_handles_head_variants_and_falls_back() {
    let upper = inject_bridge(
        r#"<!doctype html><HTML><HEAD data-x="1"><title>t</title></HEAD></HTML>"#,
        None,
    );
    assert!(upper.starts_with(r#"<!doctype html><HTML><HEAD data-x="1"><script>"#));

    let no_head = inject_bridge("<!doctype html><html><body>bare</body></html>", None);
    assert!(no_head.starts_with("<!doctype html><html><script>"));

    let bare = inject_bridge("plain text", None);
    assert!(bare.starts_with("<script>"));
    assert!(bare.ends_with("plain text"));
}

#[tokio::test]
async fn bridge_injection_keeps_app_bytes_and_its_own_script_tag() {
    // bridge 自带的那一个 </script> 之外不能再出现终止序列，否则会截断注入。
    assert_eq!(BRIDGE_SCRIPT.matches("</script").count(), 1);
    assert!(BRIDGE_SCRIPT.trim_end().ends_with("</script>"));

    // 注入只做切片：App 自己脚本里的 </script> 原样保留，不会被拼接破坏。
    let app = r#"<html><head></head><body><script>var s = "</script>";</script></body></html>"#;
    let view = inject_bridge(app, None);
    assert!(view.contains(r#"var s = "</script>";"#));
    assert_eq!(
        view.matches("</script").count(),
        app.matches("</script").count() + 1
    );
}

#[tokio::test]
async fn app_view_listener_serves_app_html_with_bridge_csp_and_no_store() -> anyhow::Result<()> {
    let (base, stop, server) = serve_document(AppDocument::from_app_html(APP_HTML)).await?;
    let response = test_client()
        .get(format!("{base}{VIEW_PATH}"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.headers()["content-security-policy"], SANDBOX_CSP);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.headers().get("x-frame-options").is_none());
    let body = response.text().await?;
    assert!(body.contains("real app"));
    assert!(body.contains("window.__bridge"));
    assert!(body.contains(&format!(r#"version: "{}""#, crate::version())));
    assert!(!body.contains("document.elementFromPoint"));
    finish(stop, server).await;
    Ok(())
}

#[tokio::test]
async fn sandbox_listener_serves_the_app_html_with_shim_and_isolating_csp() -> anyhow::Result<()> {
    let (base, stop, server) = serve_document(AppDocument::from_app_html(APP_HTML)).await?;
    let response = test_client()
        .get(format!("{base}{SANDBOX_PATH}"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(response.headers()["content-security-policy"], SANDBOX_CSP);
    for directive in [
        "default-src 'none'",
        "script-src 'unsafe-inline'",
        "style-src 'unsafe-inline'",
        "img-src data: blob:",
        "connect-src 'none'",
    ] {
        assert!(SANDBOX_CSP.contains(directive), "{directive}");
    }
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.headers().get("x-frame-options").is_none());
    let body = response.text().await?;
    assert!(body.contains("real app"));
    // iframe 诊断页仍用指针 shim，且不安装 bridge。
    assert!(body.contains("peri:pointer"));
    assert!(body.contains("document.elementFromPoint"));
    assert!(body.contains("\"pointerdown\""));
    assert!(body.contains("\"pointerup\""));
    assert!(body.contains("\"mousedown\""));
    assert!(body.contains("\"mouseup\""));
    assert!(body.contains("peri:sandbox-ready"));
    assert!(!body.contains("window.__bridge"));
    // 每个指针消息只派发一次 click；down/up 不依赖浏览器合成。
    assert_eq!(
        body.matches("fire(element, MouseEvent, \"click\"").count(),
        1
    );
    finish(stop, server).await;
    Ok(())
}

#[tokio::test]
async fn placeholder_serves_the_color_block_to_sandbox_and_the_bridge_to_view() -> anyhow::Result<()>
{
    let (base, stop, server) = serve_document(AppDocument::placeholder()).await?;
    let sandbox = test_client()
        .get(format!("{base}{SANDBOX_PATH}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    assert_eq!(sandbox.matches("background:#4ade80").count(), 1);
    assert!(sandbox.contains(r#"parent.postMessage("sandbox-ready", "*")"#));
    assert!(!sandbox.contains("peri:pointer"));

    let view_response = test_client()
        .get(format!("{base}{VIEW_PATH}"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        view_response.headers()["content-security-policy"],
        SANDBOX_CSP
    );
    let view = view_response.text().await?;
    assert!(view.contains("background:#4ade80"));
    // 占位页没有 <head>，bridge 落在 <html> 之后仍然生效。
    assert!(view.contains("window.__bridge"));
    assert!(view.contains("window.addEventListener(\"message\", onMessage)"));
    finish(stop, server).await;
    Ok(())
}

#[tokio::test]
async fn app_html_keeps_its_content_and_gets_the_pointer_shim_appended() {
    let document = AppDocument::from_app_html(APP_HTML);
    let sandbox = document.sandbox_body();
    let app = sandbox.find("id=\"app\"").expect("app html kept");
    let shim = sandbox.find("peri:pointer").expect("shim injected");
    assert!(app < shim, "shim runs after the app document");
    // 视图文档不带 shim（bridge 里只有诊断用的 click 转发，没有合成事件），
    // 但保留原文。
    assert!(document.view_body().contains("id=\"app\""));
    assert!(!document.view_body().contains("document.elementFromPoint"));
    assert!(!document.view_body().contains("peri:sandbox-ready"));
}

#[tokio::test]
async fn app_html_is_loaded_from_disk() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("app.html");
    std::fs::write(&path, APP_HTML)?;
    let document = AppDocument::load(&path)?;
    assert!(document.view_body().contains("real app"));
    assert!(document.view_body().contains("window.__bridge"));
    assert!(document.sandbox_body().contains("peri:pointer"));
    Ok(())
}

#[tokio::test]
async fn app_html_over_the_limit_is_rejected_before_serving() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("huge.html");
    std::fs::write(&path, vec![b'x'; MAX_APP_HTML_BYTES + 1])?;
    let error = AppDocument::load(&path).expect_err("oversized html must fail");
    assert!(matches!(error, AppHtmlError::TooLarge { limit, .. } if limit == MAX_APP_HTML_BYTES));
    assert!(error.to_string().contains(&MAX_APP_HTML_BYTES.to_string()));
    Ok(())
}

#[tokio::test]
async fn app_html_that_is_missing_or_not_utf8_is_rejected() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("missing.html");
    let error = AppDocument::load(&missing).expect_err("missing html must fail");
    assert!(matches!(error, AppHtmlError::Read { ref path, .. } if path == &missing));

    let binary = dir.path().join("binary.html");
    std::fs::write(&binary, [0xff, 0xfe, 0xfd])?;
    let error = AppDocument::load(&binary).expect_err("non-utf8 html must fail");
    assert!(matches!(error, AppHtmlError::NotUtf8 { ref path } if path == &binary));
    Ok(())
}

#[test]
fn app_html_error_messages_name_the_path() {
    let error = AppHtmlError::Read {
        path: PathBuf::from("/fixture/app.html"),
        source: std::io::Error::other("denied"),
    };
    assert_eq!(
        error.to_string(),
        "cannot read app html /fixture/app.html: denied"
    );
}

#[tokio::test]
async fn session_documents_use_the_relay_bridge_and_404_when_unknown() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let stop = CancellationToken::new();
    let documents = Arc::new(AppDocuments::new(AppDocument::from_app_html(APP_HTML)));
    documents.insert_view("abc", APP_HTML);
    let server = tokio::spawn(serve(
        listener,
        documents.clone(),
        stop.clone().cancelled_owned(),
    ));
    let http = test_client();

    let response = http
        .get(format!("{base}{VIEW_PATH}?view=abc"))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(response.headers()["content-security-policy"], SANDBOX_CSP);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let body = response.text().await?;
    assert!(body.contains(r#"window.__periViewSession = { id: "abc" };"#));
    assert!(body.contains("takeRequests: function"));
    assert!(body.contains("hostReply: function"));
    // 会话配置先于 bridge：bridge 在加载时读取 __periViewSession。
    let config = body
        .find("window.__periViewSession = {")
        .expect("session config");
    let bridge = body.find("window.__bridge = {").expect("bridge");
    assert!(config < bridge);

    let fallback = http
        .get(format!("{base}{VIEW_PATH}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    // bridge 源码总会提到 __periViewSession；只有会话文档会注入配置。
    assert!(!fallback.contains("window.__periViewSession = {"));
    assert!(fallback.contains("window.__bridge = {"));

    let unknown = http
        .get(format!("{base}{VIEW_PATH}?view=missing"))
        .send()
        .await?;
    assert_eq!(unknown.status(), 404);

    documents.remove_view("abc");
    let removed = http
        .get(format!("{base}{VIEW_PATH}?view=abc"))
        .send()
        .await?;
    assert_eq!(removed.status(), 404);

    stop.cancel();
    server.await.expect("server joins")?;
    Ok(())
}
