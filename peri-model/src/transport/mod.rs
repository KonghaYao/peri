//! 与 Provider 无关的 HTTP 与 SSE 传输基础设施。
#![allow(dead_code, unused_imports)]

//!
//! HTTP seam 仅供 crate 内 adapter 使用；公共协议 API 不暴露 client、headers 或原始请求。

pub(crate) mod http;
pub(crate) mod sse;

pub(crate) use http::{HttpBody, HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub(crate) use sse::{SseEvent, SseParser};

#[cfg(test)]
#[path = "http_test.rs"]
mod http_test;

#[cfg(test)]
#[path = "sse_test.rs"]
mod sse_test;
