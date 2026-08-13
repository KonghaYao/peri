//! Web 前端（静态资源）：非 ws 升级请求的 HTTP GET → 内嵌静态页。
//!
//! 定位：gateway 在回环检查（§9.5）之后、配额注册（§8.6）之前分流
//! （`channel/gateway.rs` 连接任务步骤 2）：头部含 `upgrade: websocket`
//! 的请求原样走 `accept_async`（ws 时序不变，§4.6），其余普通 HTTP 一律
//! 交给本模块——不进配额/注册表，不产生占位连接。
//!
//! 前端是独立 vite 工程（`web/`，SolidJS + Tailwind），Web 面板为唯一
//! 页面（`/` 即面板入口）；构建产物 `web/dist/` 由 `build.rs` 在编译期
//! 扫描并生成内嵌资源表（`assets.rs`，字节经 include_bytes! 引用，零
//! 运行时文件 IO、零新依赖）。产物文件名带内容 hash，本模块按实际文件
//! 清单做路径查表：`/`、`/index.html` 映射面板入口，`/panel.html` 为
//! 旧链接兼容（同样指向面板），`/assets/*` 直接映射产物相对路径，未知
//! 路径与非 GET 方法一律 404。Content-Type 按扩展名手工映射；响应固定
//! `Connection: close` + `Content-Length`，写毕 `shutdown()`。
//!
//! 设计约束：最小实现（浏览器验证用），不做 URL 解码/query 处理——资源名
//! 纯 ASCII（vite hash 文件名），取请求行 path 段（去 query）即可；不支
//! 持任何动态端点，页面的 ws 探测直连 hub 的 ws 时序（无 token 会被 1011
//! 关闭，见面板内连接逻辑，如实展示，不造假）。

use crate::auth::{AuthService, BROWSER_COOKIE};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

const MAX_HTTP_HEAD: usize = 16 * 1024;
const MAX_HTTP_BODY: usize = 4 * 1024;
const HTTP_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// 静态资源路由表：URL 路径 → 产物相对路径。`/`、`/index.html` 与旧链接
/// `/panel.html` 均映射面板入口 `index.html`，`/assets/*` 直接对应 vite
/// 产物文件名。返回 (资源名, Content-Type, 内容)；未知路径 → None。
pub(crate) fn route(path: &str) -> Option<(&'static str, &'static str, &'static [u8])> {
    let rel = match path {
        // index.html 为唯一页面（`/` 即面板）；/panel.html 仅做旧链接兼容。
        "/" | "/index.html" | "/panel.html" => "index.html",
        other => other.trim_start_matches('/'),
    };
    ASSETS
        .iter()
        .find(|a| a.url == rel)
        .map(|a| (a.url, content_type(a.url), a.bytes))
}

/// 按扩展名取 Content-Type（最小映射表；未识别回 octet-stream）。
pub(crate) fn content_type(name: &str) -> &'static str {
    if name.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if name.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if name.ends_with(".svg") {
        "image/svg+xml"
    } else if name.ends_with(".png") {
        "image/png"
    } else if name.ends_with(".ico") {
        "image/x-icon"
    } else if name.ends_with(".woff2") {
        "font/woff2"
    } else if name.ends_with(".map") || name.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

/// 解析头部首行请求行，返回路径（去 query）；非 GET / 格式非法 → None。
#[cfg(test)]
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
#[cfg(test)]
pub(crate) async fn serve(mut stream: TcpStream, head: &str) -> std::io::Result<()> {
    let (status, content_type, body): (&str, &str, &[u8]) = match request_path(head).and_then(route)
    {
        Some((_, ct, bytes)) => ("200 OK", ct, bytes),
        None => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"404 Not Found\n",
        ),
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

pub(crate) async fn serve_http(
    mut stream: TcpStream,
    peer: SocketAddr,
    auth: Arc<Mutex<AuthService>>,
) -> std::io::Result<()> {
    let deadline = tokio::time::Instant::now() + HTTP_READ_TIMEOUT;
    let mut buf = Vec::with_capacity(2048);
    let head_end = loop {
        if buf.len() >= MAX_HTTP_HEAD {
            return write_http(
                &mut stream,
                "431 Request Header Fields Too Large",
                "text/plain",
                b"bad request",
                &[],
            )
            .await;
        }
        let mut chunk = [0u8; 1024];
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(result) => result?,
            Err(_) => {
                return write_http(
                    &mut stream,
                    "408 Request Timeout",
                    "text/plain",
                    b"timeout",
                    &[],
                )
                .await
            }
        };
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HTTP_HEAD && header_end(&buf).is_none() {
            return write_http(
                &mut stream,
                "431 Request Header Fields Too Large",
                "text/plain",
                b"bad request",
                &[],
            )
            .await;
        }
        if let Some(end) = header_end(&buf) {
            break end + 4;
        }
    };
    if head_end > MAX_HTTP_HEAD {
        return write_http(
            &mut stream,
            "431 Request Header Fields Too Large",
            "text/plain",
            b"bad request",
            &[],
        )
        .await;
    }
    let head = match std::str::from_utf8(&buf[..head_end]) {
        Ok(v) => v.to_string(),
        Err(_) => {
            return write_http(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                b"bad request",
                &[],
            )
            .await
        }
    };
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut request_parts = request.split_whitespace();
    let method = request_parts.next().unwrap_or_default();
    let path = request_parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    let mut host = None;
    let mut origin = None;
    let mut cookie = None;
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = Some(value.trim()),
            "origin" => origin = Some(value.trim()),
            "cookie" => cookie = cookie_value(value.trim(), BROWSER_COOKIE),
            "content-length" => content_length = value.trim().parse().unwrap_or(MAX_HTTP_BODY + 1),
            _ => {}
        }
    }
    if path != "/api/auth/session" {
        return serve_static_consumed(stream, method, path).await;
    }
    if !peer.ip().is_loopback()
        || !valid_loopback_host(host.unwrap_or_default())
        || !valid_origin(origin, host.unwrap_or_default())
    {
        return write_http(
            &mut stream,
            "403 Forbidden",
            "application/json",
            br#"{"error":"forbidden"}"#,
            &security_headers(),
        )
        .await;
    }
    if content_length > MAX_HTTP_BODY {
        return write_http(
            &mut stream,
            "413 Payload Too Large",
            "application/json",
            br#"{"error":"too_large"}"#,
            &security_headers(),
        )
        .await;
    }
    while buf.len() - head_end < content_length {
        let mut chunk = [0u8; 1024];
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(result) => result?,
            Err(_) => {
                return write_http(
                    &mut stream,
                    "408 Request Timeout",
                    "application/json",
                    br#"{"error":"timeout"}"#,
                    &security_headers(),
                )
                .await
            }
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() - head_end > MAX_HTTP_BODY {
            return write_http(
                &mut stream,
                "413 Payload Too Large",
                "application/json",
                br#"{"error":"too_large"}"#,
                &security_headers(),
            )
            .await;
        }
    }
    if buf.len() - head_end < content_length {
        return write_http(
            &mut stream,
            "400 Bad Request",
            "application/json",
            br#"{"error":"short_body"}"#,
            &security_headers(),
        )
        .await;
    }
    let response = match method {
        "POST" => {
            let body: serde_json::Value = serde_json::from_slice(
                &buf[head_end..head_end + content_length.min(buf.len() - head_end)],
            )
            .unwrap_or_default();
            match body
                .get("token")
                .and_then(|v| v.as_str())
                .and_then(|token| {
                    auth.try_lock()
                        .ok()
                        .and_then(|mut a| a.create_browser_session(token).ok())
                }) {
                Some((sid, ctx)) => (
                    "200 OK",
                    serde_json::to_vec(
                        &serde_json::json!({"authenticated":true,"role":ctx.role.as_str()}),
                    )
                    .unwrap(),
                    vec![(
                        "Set-Cookie".to_string(),
                        format!("{BROWSER_COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/"),
                    )],
                ),
                None => (
                    "401 Unauthorized",
                    br#"{"authenticated":false}"#.to_vec(),
                    vec![],
                ),
            }
        }
        "GET" => match cookie.as_deref().and_then(|sid| {
            auth.try_lock()
                .ok()
                .and_then(|mut a| a.validate_browser_session(sid, peer).ok())
        }) {
            Some(ctx) => (
                "200 OK",
                serde_json::to_vec(
                    &serde_json::json!({"authenticated":true,"role":ctx.role.as_str()}),
                )
                .unwrap(),
                vec![],
            ),
            None => (
                "401 Unauthorized",
                br#"{"authenticated":false}"#.to_vec(),
                vec![],
            ),
        },
        "DELETE" => {
            if let Some(sid) = cookie.as_deref() {
                if let Ok(mut a) = auth.try_lock() {
                    a.delete_browser_session(sid);
                }
            }
            (
                "204 No Content",
                Vec::new(),
                vec![(
                    "Set-Cookie".to_string(),
                    format!("{BROWSER_COOKIE}=; Max-Age=0; HttpOnly; SameSite=Strict; Path=/"),
                )],
            )
        }
        _ => (
            "405 Method Not Allowed",
            br#"{"error":"method"}"#.to_vec(),
            vec![],
        ),
    };
    let mut extra = security_headers();
    extra.extend(response.2);
    write_http(
        &mut stream,
        response.0,
        "application/json",
        &response.1,
        &extra,
    )
    .await
}

async fn serve_static_consumed(
    mut stream: TcpStream,
    method: &str,
    path: &str,
) -> std::io::Result<()> {
    let found = if method == "GET" { route(path) } else { None };
    match found {
        Some((_, ct, body)) => {
            write_http(&mut stream, "200 OK", ct, body, &security_headers()).await
        }
        None => {
            write_http(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"404 Not Found\n",
                &security_headers(),
            )
            .await
        }
    }
}
async fn write_http(
    stream: &mut TcpStream,
    status: &str,
    ct: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(k.as_ref());
        head.push_str(": ");
        head.push_str(v.as_ref());
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
fn security_headers() -> Vec<(String, String)> {
    vec![
        ("Cache-Control".into(), "no-store".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
        (
            "Content-Security-Policy".into(),
            "default-src 'self'; connect-src 'self' ws://127.0.0.1:* ws://localhost:*".into(),
        ),
        ("Referrer-Policy".into(), "no-referrer".into()),
        ("X-Frame-Options".into(), "DENY".into()),
    ]
}
pub(crate) fn valid_loopback_host(host: &str) -> bool {
    let h = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(h, "localhost" | "127.0.0.1" | "::1")
}
fn valid_origin(origin: Option<&str>, host: &str) -> bool {
    origin
        .map(|o| o == format!("http://{host}"))
        .unwrap_or(true)
}
pub(crate) fn cookie_value(header: &str, name: &str) -> Option<String> {
    header
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

#[cfg(test)]
#[path = "web_test.rs"]
mod web_test;
