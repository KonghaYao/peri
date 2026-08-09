//! web 静态分支测试：请求行解析、upgrade 判定、路由与 Content-Type 映射、
//! 以及真实 socket 上的响应（TcpListener/TcpStream 直连，零新依赖）。

use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};

use crate::web::{content_type, header_end, is_ws_upgrade, request_path, route, serve};

/// 请求行解析：常规 GET 路径。
#[test]
fn request_path_get() {
    assert_eq!(
        request_path("GET / HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n"),
        Some("/")
    );
    assert_eq!(
        request_path("GET /app.js HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/app.js")
    );
}

/// 请求行解析：query 剥离（资源名纯 ASCII，不做 URL 解码）。
#[test]
fn request_path_strips_query() {
    assert_eq!(
        request_path("GET /app.js?v=123 HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/app.js")
    );
}

/// 请求行解析：非 GET 与格式非法 → None。
#[test]
fn request_path_rejects_non_get_and_garbage() {
    assert_eq!(request_path("POST / HTTP/1.1\r\nHost: test\r\n\r\n"), None);
    assert_eq!(request_path("GET"), None);
    assert_eq!(request_path(""), None);
    assert_eq!(request_path("GARBAGE\r\n\r\n"), None);
}

/// 头部结束符定位：完整头部返回 `\r\n\r\n` 下标，不完整返回 None。
#[test]
fn header_end_finds_terminator() {
    let full = b"GET / HTTP/1.1\r\nHost: test\r\n\r\n";
    assert_eq!(header_end(full), Some(26));
    assert_eq!(header_end(&full[..20]), None);
    assert_eq!(header_end(b""), None);
}

/// ws 升级判定：大小写不敏感；普通 GET 不含 upgrade 头 → false。
#[test]
fn ws_upgrade_detection() {
    let ws = b"GET /instance HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
    assert!(is_ws_upgrade(ws));
    let ws_lower = b"GET / HTTP/1.1\r\nupgrade: websocket\r\n\r\n";
    assert!(is_ws_upgrade(ws_lower));
    let plain = b"GET /index.html HTTP/1.1\r\nHost: test\r\n\r\n";
    assert!(!is_ws_upgrade(plain));
}

/// 路由表：首页与登记的静态资源可达，未知路径 404。
#[test]
fn route_resolves_static_assets() {
    let (name, index) = route("/").expect("/ → index.html");
    assert_eq!(name, "index.html");
    assert!(index.contains("acp-hub 验证台"));
    assert!(index.contains("htmx"));
    assert_eq!(route("/index.html").map(|(n, _)| n), Some("index.html"));

    // 至少两个 JS 文件（验证台要求）各自可达。
    let (name, app) = route("/app.js").expect("/app.js");
    assert_eq!(name, "app.js");
    assert!(app.contains("acpHubWsConnect"));
    let (name, ws) = route("/ws.js").expect("/ws.js");
    assert_eq!(name, "ws.js");
    assert!(ws.contains("acpHubWsConnect"));

    let (name, css) = route("/style.css").expect("/style.css");
    assert_eq!(name, "style.css");
    assert!(css.contains("acp-hub"));

    assert_eq!(route("/instance"), None);
    assert_eq!(route("/favicon.ico"), None);
}

/// Content-Type 按扩展名映射；未识别回 octet-stream。
#[test]
fn content_type_by_extension() {
    assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
    assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
    assert_eq!(content_type("style.css"), "text/css; charset=utf-8");
    assert_eq!(content_type("blob.bin"), "application/octet-stream");
}

/// socket 端到端：GET / → 200 + html Content-Type + 首页内容（read_to_end
/// 依赖 `Connection: close` + shutdown 产生 EOF）。
#[tokio::test]
async fn serve_returns_index() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, "GET / HTTP/1.1\r\nHost: test\r\n\r\n").await.unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(text.contains("Content-Type: text/html; charset=utf-8"), "{text:?}");
    assert!(text.contains("acp-hub 验证台"), "{text:?}");
    assert!(text.ends_with("</html>\n"), "{text:?}");
}

/// socket 端到端：未知路径 → 404（含 Content-Length，体为纯文本）。
#[tokio::test]
async fn serve_returns_404() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, "GET /no-such-resource HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{text:?}");
    assert!(text.contains("Content-Length: 14"), "{text:?}");
    assert!(text.ends_with("404 Not Found\n"), "{text:?}");
}

/// socket 端到端：query 路径同样命中（浏览器缓存失效参数）。
#[tokio::test]
async fn serve_handles_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, "GET /ws.js?t=1 HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(text.contains("Content-Type: text/javascript; charset=utf-8"), "{text:?}");
}
