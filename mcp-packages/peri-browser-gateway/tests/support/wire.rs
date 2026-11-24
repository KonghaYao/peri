//! 记录测试客户端实际发给网关的 HTTP 元数据；不记录工具参数或响应正文。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::{routing::any, Router};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::gateway::Gateway;

#[derive(Clone, Debug)]
pub struct Exchange {
    pub http_method: Method,
    pub rpc_method: Option<String>,
    pub sent_session: Option<String>,
    pub sent_version: Option<String>,
    pub returned_session: Option<String>,
    pub status: StatusCode,
}

#[derive(Clone)]
struct Forward {
    client: reqwest::Client,
    upstream: String,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

pub struct Wire {
    pub uri: String,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
    stop: CancellationToken,
    server: JoinHandle<std::io::Result<()>>,
}

impl Wire {
    pub async fn start(gateway: &Gateway) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let uri = format!("http://{}/mcp", listener.local_addr()?);
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let forward = Forward {
            client: gateway.http.clone(),
            upstream: gateway.mcp(),
            exchanges: exchanges.clone(),
        };
        let app = Router::new().route("/mcp", any(relay)).with_state(forward);
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
        });
        Ok(Self {
            uri,
            exchanges,
            stop,
            server,
        })
    }

    pub fn posts(&self) -> Vec<Exchange> {
        self.exchanges
            .lock()
            .unwrap()
            .iter()
            .filter(|exchange| exchange.http_method == Method::POST)
            .cloned()
            .collect()
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), &mut self.server).await???;
        Ok(())
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

async fn relay(State(forward): State<Forward>, request: Request) -> Result<Response, StatusCode> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let rpc_method = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v["method"].as_str().map(str::to_owned));
    let mut upstream = forward
        .client
        .request(parts.method.clone(), &forward.upstream)
        .body(body);
    for name in [
        "accept",
        "content-type",
        "last-event-id",
        "mcp-session-id",
        "mcp-protocol-version",
        "mcp-method",
        "mcp-name",
    ] {
        if let Some(value) = parts.headers.get(name) {
            upstream = upstream.header(name, value);
        }
    }
    let response = upstream.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let status = response.status();
    forward.exchanges.lock().unwrap().push(Exchange {
        http_method: parts.method,
        rpc_method,
        sent_session: header(&parts.headers, "mcp-session-id"),
        sent_version: header(&parts.headers, "mcp-protocol-version"),
        returned_session: header(response.headers(), "mcp-session-id"),
        status,
    });
    let mut builder = Response::builder().status(status);
    for name in ["content-type", "mcp-session-id", "mcp-protocol-version"] {
        if let Some(value) = response.headers().get(name) {
            builder = builder.header(name, value);
        }
    }
    let stream = futures::stream::try_unfold(response, |mut response| async move {
        let chunk = response.chunk().await?;
        Ok::<_, reqwest::Error>(chunk.map(|chunk| (chunk, response)))
    });
    builder
        .body(Body::from_stream(stream))
        .map_err(|_| StatusCode::BAD_GATEWAY)
}
