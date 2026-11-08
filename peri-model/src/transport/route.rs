use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::header::HeaderValue;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{ModelCallContext, ModelError, ModelResult, ModelTransportRoute, ProtocolErrorKind};

use super::{HttpRequest, HttpResponse, HttpTransport};

const CONTEXT_HEADER: &str = "x-peri-replay-context";
const TARGET_HEADER: &str = "x-peri-replay-target";

pub(crate) fn routed_transport(
    transport: Arc<dyn HttpTransport>,
    route: Option<&ModelTransportRoute>,
) -> Arc<dyn HttpTransport> {
    match route {
        Some(route) => Arc::new(RoutedTransport {
            inner: transport,
            route: route.clone(),
        }),
        None => transport,
    }
}

struct RoutedTransport {
    inner: Arc<dyn HttpTransport>,
    route: ModelTransportRoute,
}

#[derive(Serialize)]
struct WireContext<'a> {
    v: u8,
    run: &'a str,
    #[serde(flatten)]
    call: &'a ModelCallContext,
    attempt: u32,
}

fn invalid(message: &str) -> ModelError {
    // 路由配置错误不可通过重试恢复，也不能回退访问逻辑 provider。
    ModelError::protocol(ProtocolErrorKind::Other).with_message(message)
}

fn apply_route(request: &mut HttpRequest, route: &ModelTransportRoute) -> ModelResult<()> {
    let context = request
        .call_context
        .as_ref()
        .ok_or_else(|| invalid("Model transport route requires a call context"))?;
    if [
        &context.owner,
        &context.execution,
        &context.call,
        &context.purpose,
    ]
    .iter()
    .any(|value| value.trim().is_empty())
        || request.attempt == 0
    {
        return Err(invalid(
            "Model transport call identity and attempt must be nonempty",
        ));
    }
    if let Some(parent) = &context.parent {
        if [
            &parent.owner,
            &parent.execution,
            &parent.call,
            &parent.tool_call,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || parent
                .slot
                .as_ref()
                .is_some_and(|slot| slot.trim().is_empty())
        {
            return Err(invalid("Model transport parent identity must be nonempty"));
        }
    }
    let wire = serde_json::to_vec(&WireContext {
        v: 1,
        run: route.run(),
        call: context,
        attempt: request.attempt,
    })
    .map_err(|error| invalid("Cannot serialize model transport context").with_error(&error))?;
    if wire.len() > 4096 {
        return Err(invalid("Model transport context exceeds 4096 bytes"));
    }
    let original = request.request.url().clone();
    let target = HeaderValue::from_str(original.as_str())
        .map_err(|error| invalid("Invalid logical model target header").with_error(&error))?;
    let context = HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(wire))
        .map_err(|error| invalid("Invalid model transport context header").with_error(&error))?;
    let mut destination = route.proxy_url().clone();
    destination.set_path(original.path());
    destination.set_query(original.query());
    *request.request.url_mut() = destination;
    request.request.headers_mut().remove(reqwest::header::HOST);
    request.request.headers_mut().insert(TARGET_HEADER, target);
    request
        .request
        .headers_mut()
        .insert(CONTEXT_HEADER, context);
    Ok(())
}

#[async_trait]
impl HttpTransport for RoutedTransport {
    async fn send(
        &self,
        mut request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        if cancellation.is_cancelled() {
            return Err(ModelError::cancelled());
        }
        apply_route(&mut request, &self.route)?;
        self.inner.send(request, cancellation).await
    }
}
