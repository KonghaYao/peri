use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelResult, TransportErrorKind};

pub(crate) type HttpBody = Pin<Box<dyn Stream<Item = ModelResult<Vec<u8>>> + Send>>;

/// 只在 crate 内使用的 HTTP 请求；认证 headers 与 client 不会离开 transport 层。
pub(crate) struct HttpRequest {
    pub(crate) request: reqwest::Request,
}

impl HttpRequest {
    pub(crate) fn new(request: reqwest::Request) -> Self {
        Self { request }
    }
}

/// 已建立连接后的安全响应元数据与可取消 body。
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) request_id: Option<String>,
    pub(crate) body: HttpBody,
}

impl HttpResponse {
    pub(crate) fn new(
        status: u16,
        request_id: Option<String>,
        body: HttpBody,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            status,
            request_id,
            body: cancellable_body(body, cancellation),
        }
    }
}

/// 可替换的 HTTP 边界，测试可模拟连接、状态、分块 body 与读取失败，且无需网络。
#[async_trait]
pub(crate) trait HttpTransport: Send + Sync {
    async fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse>;
}

/// 基于 reqwest 的生产 transport。响应 body 在取消时立即丢弃。
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub(crate) fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ModelError::cancelled()),
            response = self.client.execute(request.request) => response.map_err(map_reqwest_error)?,
        };
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .or_else(|| response.headers().get("request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map(|chunk| chunk.to_vec()).map_err(map_reqwest_error));

        Ok(HttpResponse::new(
            status,
            request_id,
            Box::pin(body),
            cancellation,
        ))
    }
}

fn cancellable_body(body: HttpBody, cancellation: CancellationToken) -> HttpBody {
    Box::pin(futures::stream::unfold(
        (body, cancellation, false),
        |(mut body, cancellation, reported)| async move {
            if reported {
                return None;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Some((Err(ModelError::cancelled()), (body, cancellation, true))),
                chunk = body.next() => chunk.map(|chunk| (chunk, (body, cancellation, false))),
            }
        },
    ))
}

fn map_reqwest_error(error: reqwest::Error) -> ModelError {
    let kind = if error.is_timeout() {
        TransportErrorKind::Timeout
    } else if error.is_connect() {
        TransportErrorKind::Connection
    } else {
        TransportErrorKind::Other
    };
    ModelError::transport(kind, None::<&str>)
}
