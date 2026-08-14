//! Web 前端与浏览器认证入口。
//!
//! 定位：gateway 在回环检查（§9.5）之后、配额注册（§8.6）之前分流
//! （`channel/gateway.rs` 连接任务步骤 2）：头部含 `upgrade: websocket`
//! 的请求原样走 WebSocket 握手（ws 时序不变，§4.6），其余普通 HTTP 一律
//! 交给本模块——不进连接配额/注册表，不产生占位连接。HTTP 面只包含两类
//! 路由：内嵌 Vite 静态资源，以及同源 `/api/auth/session` 的 bounded
//! GET/POST/DELETE cookie bootstrap。
//!
//! 前端是独立 Vite 工程（`web/`，SolidJS），Web 面板为唯一
//! 页面（`/` 即面板入口）；构建产物 `web/dist/` 由 `build.rs` 在编译期
//! 扫描并生成内嵌资源表（`assets.rs`，字节经 include_bytes! 引用，零
//! 运行时文件 IO、零新依赖）。产物文件名带内容 hash，本模块按实际文件
//! 清单做路径查表：`/`、`/index.html` 映射面板入口，`/panel.html` 为
//! 旧链接兼容（同样指向面板），`/assets/*` 直接映射产物相对路径，未知
//! 路径与非 GET 方法一律 404。Content-Type 按扩展名手工映射；响应固定
//! `Connection: close` + `Content-Length`，写毕 `shutdown()`。
//!
//! 设计约束：静态资源名不做 URL 解码，取请求行 path 段（去 query）查内嵌
//! 清单。认证端点使用独立的有限 HTTP/1.1 解析：限制头/body/读取总时长，
//! 拒绝 chunked/重复 framing，校验 loopback Host 与同源 Origin，并固定
//! `no-store` 安全头。浏览器 bearer 只出现在登录请求 body；成功后仅使用
//! HttpOnly opaque cookie，WebSocket 不持有或重放 bearer。

use crate::auth::{AuthService, BROWSER_COOKIE, BROWSER_SESSION_TTL_SECS, TOKENS_FILE};
use crate::config::Config;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

const MAX_HTTP_HEAD: usize = 16 * 1024;
const MAX_HTTP_BODY: usize = 4 * 1024;
const HTTP_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserLoginRequest {
    token: String,
}

/// Credential-free setup metadata for the loopback login surface.
///
/// This descriptor is derived from the authoritative runtime Config before any
/// auth lock is acquired. It never contains token records, ids or file data.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserAuthSetup {
    token_file: String,
    generate_command: String,
}

impl BrowserAuthSetup {
    pub(crate) fn from_config(cfg: &Config) -> Self {
        let executable = std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "acp-hub-server".to_string());
        Self::from_parts(cfg, &executable)
    }

    fn from_parts(cfg: &Config, executable: &str) -> Self {
        let config_dir = cfg.config_dir.to_string_lossy();
        Self {
            token_file: cfg
                .config_dir
                .join(TOKENS_FILE)
                .to_string_lossy()
                .into_owned(),
            generate_command: format!(
                "ACP_HUB_CONFIG_DIR={} {} token generate --name web --role full",
                shell_single_quote(&config_dir),
                shell_single_quote(executable),
            ),
        }
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

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
    auth_setup: BrowserAuthSetup,
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
    let version = request_parts.next().unwrap_or_default();
    let mut malformed = version != "HTTP/1.1" || request_parts.next().is_some();
    let mut host = None;
    let mut origin = None;
    let mut cookie = None;
    let mut content_type = None;
    let mut content_length = None;
    let mut transfer_encoding = None;
    let mut cookie_header_seen = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            malformed = true;
            continue;
        };
        if name != name.trim() || !is_http_header_name(name) {
            malformed = true;
            continue;
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "host" => set_unique_header(&mut host, value, &mut malformed),
            "origin" => set_unique_header(&mut origin, value, &mut malformed),
            "cookie" => {
                if cookie_header_seen {
                    malformed = true;
                } else {
                    cookie_header_seen = true;
                    cookie = cookie_value(value, BROWSER_COOKIE);
                }
            }
            "content-type" => set_unique_header(&mut content_type, value, &mut malformed),
            "content-length" => {
                if content_length.is_some() {
                    malformed = true;
                } else {
                    content_length = value.parse::<usize>().ok();
                    if content_length.is_none() {
                        malformed = true;
                    }
                }
            }
            "transfer-encoding" => set_unique_header(&mut transfer_encoding, value, &mut malformed),
            _ => {}
        }
    }
    if malformed || method.is_empty() || path.is_empty() {
        return write_http(
            &mut stream,
            "400 Bad Request",
            "application/json",
            br#"{"error":"bad_request"}"#,
            &security_headers(),
        )
        .await;
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
    if transfer_encoding.is_some() {
        return write_http(
            &mut stream,
            "400 Bad Request",
            "application/json",
            br#"{"error":"transfer_encoding_not_supported"}"#,
            &security_headers(),
        )
        .await;
    }
    let content_length = content_length.unwrap_or(0);
    match method {
        "POST" => {
            if origin.is_none() {
                return write_http(
                    &mut stream,
                    "403 Forbidden",
                    "application/json",
                    br#"{"error":"origin_required"}"#,
                    &security_headers(),
                )
                .await;
            }
            if !content_type.is_some_and(is_json_content_type) {
                return write_http(
                    &mut stream,
                    "415 Unsupported Media Type",
                    "application/json",
                    br#"{"error":"content_type"}"#,
                    &security_headers(),
                )
                .await;
            }
            if content_length == 0 {
                return write_http(
                    &mut stream,
                    "400 Bad Request",
                    "application/json",
                    br#"{"error":"body_required"}"#,
                    &security_headers(),
                )
                .await;
            }
        }
        "GET" => {
            if content_length != 0 {
                return write_http(
                    &mut stream,
                    "400 Bad Request",
                    "application/json",
                    br#"{"error":"body_not_allowed"}"#,
                    &security_headers(),
                )
                .await;
            }
        }
        "DELETE" => {
            if origin.is_none() {
                return write_http(
                    &mut stream,
                    "403 Forbidden",
                    "application/json",
                    br#"{"error":"origin_required"}"#,
                    &security_headers(),
                )
                .await;
            }
            if content_length != 0 {
                return write_http(
                    &mut stream,
                    "400 Bad Request",
                    "application/json",
                    br#"{"error":"body_not_allowed"}"#,
                    &security_headers(),
                )
                .await;
            }
        }
        _ => {
            return write_http(
                &mut stream,
                "405 Method Not Allowed",
                "application/json",
                br#"{"error":"method"}"#,
                &security_headers(),
            )
            .await;
        }
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
    if buf.len() - head_end > content_length {
        return write_http(
            &mut stream,
            "400 Bad Request",
            "application/json",
            br#"{"error":"body_length_mismatch"}"#,
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
        if buf.len() - head_end > content_length {
            return write_http(
                &mut stream,
                "400 Bad Request",
                "application/json",
                br#"{"error":"body_length_mismatch"}"#,
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
            let body: BrowserLoginRequest = match serde_json::from_slice(
                &buf[head_end..head_end + content_length.min(buf.len() - head_end)],
            ) {
                Ok(body) => body,
                Err(_) => {
                    return write_http(
                        &mut stream,
                        "400 Bad Request",
                        "application/json",
                        br#"{"error":"invalid_json"}"#,
                        &security_headers(),
                    )
                    .await;
                }
            };
            match auth.try_lock() {
                Ok(mut auth) => match auth.create_browser_session(&body.token) {
                    Ok((sid, ctx)) => (
                    "200 OK",
                    serde_json::to_vec(
                        &serde_json::json!({"authenticated":true,"role":ctx.role.as_str(),"setup":auth_setup}),
                    )
                    .unwrap(),
                    vec![(
                        "Set-Cookie".to_string(),
                        format!(
                            "{BROWSER_COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age={BROWSER_SESSION_TTL_SECS}"
                        ),
                    )],
                    ),
                    Err(_) => (
                        "401 Unauthorized",
                        serde_json::to_vec(&serde_json::json!({"authenticated":false,"setup":auth_setup})).unwrap(),
                        vec![],
                    ),
                },
                Err(_) => (
                    "503 Service Unavailable",
                    serde_json::to_vec(&serde_json::json!({"error":"auth_busy","setup":auth_setup})).unwrap(),
                    vec![("Retry-After".to_string(), "1".to_string())],
                ),
            }
        }
        "GET" => match cookie.as_deref() {
            Some(sid) => match auth.try_lock() {
                Ok(mut auth) => match auth.validate_browser_session(sid, peer) {
                    Ok(ctx) => (
                        "200 OK",
                        serde_json::to_vec(
                            &serde_json::json!({"authenticated":true,"role":ctx.role.as_str(),"setup":auth_setup}),
                        )
                        .unwrap(),
                        vec![],
                    ),
                    Err(_) => (
                        "401 Unauthorized",
                        serde_json::to_vec(&serde_json::json!({"authenticated":false,"setup":auth_setup})).unwrap(),
                        vec![],
                    ),
                },
                Err(_) => (
                    "503 Service Unavailable",
                    serde_json::to_vec(&serde_json::json!({"error":"auth_busy","setup":auth_setup})).unwrap(),
                    vec![("Retry-After".to_string(), "1".to_string())],
                ),
            },
            None => (
                "401 Unauthorized",
                serde_json::to_vec(&serde_json::json!({"authenticated":false,"setup":auth_setup})).unwrap(),
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
        _ => unreachable!("method surface validated before body read"),
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
    let is_head = method == "HEAD";
    let found = if method == "GET" || is_head {
        route(path)
    } else {
        None
    };
    match found {
        Some((name, ct, body)) => {
            let mut headers = base_security_headers();
            headers.extend(cache_headers_for_static(path, Some(name)));
            write_http_response(&mut stream, "200 OK", ct, body, &headers, !is_head).await
        }
        None => {
            let mut headers = base_security_headers();
            headers.extend(cache_headers_for_static(path, None));
            write_http_response(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"404 Not Found\n",
                &headers,
                !is_head,
            )
            .await
        }
    }
}

/// Static cache policy is identity-based, not extension-based. Only a real embedded
/// Vite asset under `/assets/` is immutable; entry documents and misses always fetch
/// fresh so a restarted server cannot be paired with an obsolete client bundle.
pub(crate) fn cache_headers_for_static(
    request_path: &str,
    routed_name: Option<&str>,
) -> Vec<(String, String)> {
    let immutable =
        request_path.starts_with("/assets/") && routed_name.is_some_and(is_fingerprinted_asset);
    vec![(
        "Cache-Control".into(),
        if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        }
        .into(),
    )]
}

fn is_fingerprinted_asset(name: &str) -> bool {
    let Some(file_name) = name
        .strip_prefix("assets/")
        .and_then(|path| path.rsplit('/').next())
    else {
        return false;
    };
    let stem = file_name.split('.').next().unwrap_or_default();
    // Rollup's URL-safe base64 hash alphabet includes `-`, including as the
    // final character (for example `index-CBgKAe6-.css`). Split at the first
    // separator so a trailing hash character is not mistaken for a delimiter.
    let Some((_, fingerprint)) = stem.split_once('-') else {
        return false;
    };
    fingerprint.len() >= 8
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
async fn write_http(
    stream: &mut TcpStream,
    status: &str,
    ct: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> std::io::Result<()> {
    write_http_response(stream, status, ct, body, headers, true).await
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    ct: &str,
    body: &[u8],
    headers: &[(String, String)],
    include_body: bool,
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
    if include_body {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}
fn security_headers() -> Vec<(String, String)> {
    let mut headers = base_security_headers();
    headers.extend([
        ("Cache-Control".into(), "no-store".into()),
        ("Pragma".into(), "no-cache".into()),
    ]);
    headers
}

fn base_security_headers() -> Vec<(String, String)> {
    vec![
        ("X-Content-Type-Options".into(), "nosniff".into()),
        (
            "Content-Security-Policy".into(),
            "default-src 'self'; connect-src 'self' ws://127.0.0.1:* ws://localhost:*".into(),
        ),
        ("Referrer-Policy".into(), "no-referrer".into()),
        ("X-Frame-Options".into(), "DENY".into()),
    ]
}

fn set_unique_header<'a>(slot: &mut Option<&'a str>, value: &'a str, malformed: &mut bool) {
    if slot.replace(value).is_some() {
        *malformed = true;
    }
}

fn is_http_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn is_json_content_type(value: &str) -> bool {
    let mut parts = value.split(';').map(str::trim);
    if !parts
        .next()
        .is_some_and(|media| media.eq_ignore_ascii_case("application/json"))
    {
        return false;
    }
    match parts.next() {
        None => true,
        Some(parameter) => {
            parameter.eq_ignore_ascii_case("charset=utf-8") && parts.next().is_none()
        }
    }
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
