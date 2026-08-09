//! web 静态分支测试：请求行解析、upgrade 判定、路由与 Content-Type 映射、
//! 以及真实 socket 上的响应（TcpListener/TcpStream 直连，零新依赖）。
//!
//! 路由断言面向 vite 构建产物（web/dist，build.rs 编译期内嵌）：页面入口
//! 固定（/、/panel.html），assets 文件名带内容 hash —— 测试遍历 ASSETS
//! 表断言 js/css 存在，不硬编码 hash 文件名。

use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};

use crate::web::{content_type, header_end, is_ws_upgrade, request_path, route, serve, ASSETS};

/// 请求行解析：常规 GET 路径。
#[test]
fn request_path_get() {
    assert_eq!(
        request_path("GET / HTTP/1.1\r\nHost: 127.0.0.1:8456\r\n\r\n"),
        Some("/")
    );
    assert_eq!(
        request_path("GET /assets/index-abc.js HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/assets/index-abc.js")
    );
}

/// 请求行解析：query 剥离（资源名纯 ASCII，不做 URL 解码）。
#[test]
fn request_path_strips_query() {
    assert_eq!(
        request_path("GET /panel.html?v=123 HTTP/1.1\r\nHost: test\r\n\r\n"),
        Some("/panel.html")
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

/// 路由表：面板为唯一页面（/、/index.html、旧链接 /panel.html 同源），
/// 资产表非空且含 js/css，未知路径 404。
#[test]
fn route_resolves_static_assets() {
    // Web 面板入口：/、/index.html 与 /panel.html（旧链接兼容）同源。
    let (name, ct, index) = route("/").expect("/ → index.html");
    assert_eq!(name, "index.html");
    assert_eq!(ct, "text/html; charset=utf-8");
    assert!(String::from_utf8_lossy(index).contains("acp-hub Web 面板"));
    assert_eq!(route("/index.html").map(|(n, _, _)| n), Some("index.html"));
    assert_eq!(route("/panel.html").map(|(n, _, _)| n), Some("index.html"));
    let (_, _, compat) = route("/panel.html").expect("/panel.html 兼容映射");
    assert_eq!(compat, index);

    // vite 产物：至少一个 js、一个 css（hash 文件名，不硬编码）。
    let js: Vec<_> = ASSETS.iter().filter(|a| a.url.ends_with(".js")).collect();
    let css: Vec<_> = ASSETS.iter().filter(|a| a.url.ends_with(".css")).collect();
    assert!(!js.is_empty(), "产物应含 js 资源");
    assert!(!css.is_empty(), "产物应含 css 资源");
    for a in js.iter().chain(css.iter()) {
        assert!(!a.bytes.is_empty(), "{} 应为非空", a.url);
    }

    // assets/ 前缀的产物路径可经 URL 命中（取第一个 js 走路由）。
    let first_js = js[0].url;
    let routed = route(&format!("/{}", first_js)).expect("assets 可经 URL 路由");
    assert_eq!(routed.0, first_js);

    assert_eq!(route("/instance"), None);
    assert_eq!(route("/favicon.ico"), None);
}

/// Content-Type 按扩展名映射；未识别回 octet-stream。
#[test]
fn content_type_by_extension() {
    assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
    assert_eq!(content_type("assets/app.js"), "text/javascript; charset=utf-8");
    assert_eq!(content_type("assets/style.css"), "text/css; charset=utf-8");
    assert_eq!(content_type("icon.svg"), "image/svg+xml");
    assert_eq!(content_type("icon.png"), "image/png");
    assert_eq!(content_type("favicon.ico"), "image/x-icon");
    assert_eq!(content_type("font.woff2"), "font/woff2");
    assert_eq!(content_type("chunk.js.map"), "application/json");
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
    assert!(text.contains("acp-hub Web 面板"), "{text:?}");
    assert!(text.contains("</html>"), "{text:?}");
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
    // 取资产表中第一个 js 作为探测目标（hash 文件名不硬编码）。
    let js = ASSETS.iter().find(|a| a.url.ends_with(".js")).expect("有 js 产物");
    let path = format!("/{}?t=1", js.url);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, &format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n")).await.unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    server.await.unwrap();

    let text = String::from_utf8(buf).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
    assert!(text.contains("Content-Type: text/javascript; charset=utf-8"), "{text:?}");
    // 响应体与内嵌字节一致（Content-Length 精确匹配）。
    let body_start = text.find("\r\n\r\n").map(|i| i + 4).expect("有头部结束符");
    assert_eq!(&text.as_bytes()[body_start..], js.bytes);
}
