//! Web 验证台（静态资源）：非 ws 升级请求的 HTTP GET → 内嵌静态页。
//!
//! 定位：gateway 在回环检查（§9.5）之后、配额注册（§8.6）之前分流
//! （`channel/gateway.rs` 连接任务步骤 2）：头部含 `upgrade: websocket`
//! 的请求原样走 `accept_async`（ws 时序不变，§4.6），其余普通 HTTP 一律
//! 交给本模块——不进配额/注册表，不产生占位连接。
//!
//! 资源经 `include_str!` 编译期内嵌（零运行时文件 IO、零新依赖）；路由为
//! 路径查表：验证台 `/`、`/index.html`、`/app.js`、`/ws.js`、`/style.css`，
//! Web 面板（M3）`/panel.html`、`/protocol.js`、`/ws-client.js`、
//! `/yjs-view.js`、`/ui.js`、`/main.js`，未知路径与非 GET 方法一律 404。
//! Content-Type 按扩展名手工映射；响应固定
//! `Connection: close` + `Content-Length`，写毕 `shutdown()`。
//!
//! 设计约束：最小实现（浏览器验证用），不做 URL 解码/query 处理——资源名
//! 纯 ASCII，取请求行 path 段（去 query）即可；不支持任何动态端点，页面
//! 的 ws 探测直连 hub 的 ws 时序（无 token 会被 1011 关闭，见 index.html
//! 内的说明，如实展示，不造假）。

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// 静态资源路由表（路径 → (资源名, 内嵌内容)）。
pub(crate) fn route(path: &str) -> Option<(&'static str, &'static str)> {
    let asset = match path {
        "/" | "/index.html" => ("index.html", include_str!("index.html")),
        "/app.js" => ("app.js", include_str!("app.js")),
        "/ws.js" => ("ws.js", include_str!("ws.js")),
        "/style.css" => ("style.css", include_str!("style.css")),
        // Web 面板（M3，独立路径，与验证台并存）：纯静态登记，零业务逻辑。
        "/panel.html" => ("panel.html", include_str!("panel.html")),
        "/protocol.js" => ("protocol.js", include_str!("protocol.js")),
        "/ws-client.js" => ("ws-client.js", include_str!("ws-client.js")),
        "/yjs-view.js" => ("yjs-view.js", include_str!("yjs-view.js")),
        "/ui.js" => ("ui.js", include_str!("ui.js")),
        "/main.js" => ("main.js", include_str!("main.js")),
        _ => return None,
    };
    Some(asset)
}

/// 按扩展名取 Content-Type（最小映射表；未识别回 octet-stream）。
pub(crate) fn content_type(name: &str) -> &'static str {
    if name.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if name.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

/// 解析头部首行请求行，返回路径（去 query）；非 GET / 格式非法 → None。
pub(crate) fn request_path(head: &str) -> Option<&str> {
    let mut parts = head.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" {
        return None;
    }
    Some(target.split('?').next().unwrap_or(target))
}

/// 定位头部结束符 `\r\n\r\n` 的起始下标；头部未完整 → None。
pub(crate) fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// 头部字节是否含 `upgrade: websocket`（ASCII 大小写不敏感；保守判定，
/// 含该子串即视为 ws 升级请求——误判只会让 `accept_async` 按原逻辑拒绝）。
pub(crate) fn is_ws_upgrade(buf: &[u8]) -> bool {
    let needle = b"upgrade: websocket";
    buf.to_ascii_lowercase()
        .windows(needle.len())
        .any(|w| w == needle)
}

/// 处理一个非 ws 的 HTTP 连接：请求行解析 → 路由 → 响应 → shutdown。
/// 未知路径 / 非 GET → 404（最小实现，不区分 405）。调用方保证已窥探
/// 出请求首段（`head` 为头部文本，本函数只取首行）。
pub(crate) async fn serve(mut stream: TcpStream, head: &str) -> std::io::Result<()> {
    let (status, content_type, body): (&str, &str, &[u8]) = match request_path(head).and_then(route)
    {
        Some((name, body)) => ("200 OK", content_type(name), body.as_bytes()),
        None => ("404 Not Found", "text/plain; charset=utf-8", b"404 Not Found\n"),
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    // AsyncWriteExt::shutdown：写半部 FIN（配合 `Connection: close`，
    // 客户端 read_to_end 得 EOF）。
    stream.shutdown().await
}

#[cfg(test)]
#[path = "web_test.rs"]
mod web_test;
