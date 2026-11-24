//! 测试用极简 HTTP stub（`tokio::net::TcpListener` + 手写解析）。
//!
//! 只服务 view_client 的确定性测试：记录请求方法/路径/头/body，按 handler
//! 返回原始响应字节。不引入新依赖，也不依赖真实网关/Kitesurf。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 记录到的 HTTP 请求。
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// 头名统一小写。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

type Handler = Arc<dyn Fn(RecordedRequest) -> Vec<u8> + Send + Sync>;

/// 已启动的 stub 服务。
pub struct StubServer {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl StubServer {
    /// 启动并返回 base URL（`http://127.0.0.1:<port>`）。
    pub async fn start(
        handler: impl Fn(RecordedRequest) -> Vec<u8> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let address = listener.local_addr().expect("stub addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let handler: Handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = Arc::clone(&handler);
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    serve_connection(stream, handler, recorded).await;
                });
            }
        });
        Self {
            url: format!("http://{address}"),
            requests,
        }
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 等到至少 `count` 个请求（超时返回已记录数量，供断言给出真实值）。
    pub async fn wait_requests(&self, count: usize, timeout: Duration) -> Vec<RecordedRequest> {
        let deadline = Instant::now() + timeout;
        loop {
            let requests = self.requests();
            if requests.len() >= count || Instant::now() >= deadline {
                return requests;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    handler: Handler,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    recorded
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(request.clone());
    let response = handler(request);
    let _ = stream.write_all(&response).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(end) = find_subslice(&buffer, b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 8 * 1024 * 1024 {
            return None;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 构造完整 HTTP/1.1 响应（`Connection: close`）。
#[must_use]
pub fn http_response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        _ => "Response",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// 构造 SSE 响应；`events` 是已拼好的 wire 文本。
#[must_use]
pub fn sse_response(events: &str) -> Vec<u8> {
    http_response(200, "text/event-stream", events.as_bytes())
}
