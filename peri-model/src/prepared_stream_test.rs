use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::*;

#[derive(Default)]
struct CapturedTransport(Mutex<Vec<Value>>);

#[async_trait]
impl HttpTransport for CapturedTransport {
    async fn send(
        &self,
        request: HttpRequest,
        cancel: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        let body = request
            .request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .unwrap();
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_slice(body).unwrap());
        Ok(HttpResponse::new(
            400,
            None,
            Box::pin(stream::iter([Ok(b"{}".to_vec())])),
            cancel,
        ))
    }
}

async fn exact_prepared_body(model: &dyn Model, transport: Arc<CapturedTransport>) {
    let text = "complete untruncated prompt ".repeat(6000);
    let request = ModelRequest {
        messages: vec![ModelMessage::system_text("frozen + dynamic system"), ModelMessage::user_text(text.clone())],
        tools: vec![ToolDefinition::new("proof", JsonObject::from_value(json!({"type":"object","properties":{"secret_field":{"const":"full model schema"}}})).unwrap())],
        ..Default::default()
    };
    let prepared = model.prepare_stream(request).unwrap();
    let checkpoint = prepared.checkpoint().clone();
    let serialized = serde_json::to_string(&checkpoint).unwrap();
    assert!(serialized.contains(&text));
    assert!(serialized.contains("full model schema"));
    assert!(!serialized.contains("transport-test-api-key"));
    assert!(transport.0.lock().unwrap().is_empty());
    let mut response = prepared.start(CancellationToken::new()).unwrap();
    assert!(transport.0.lock().unwrap().is_empty());
    assert!(response.next().await.unwrap().is_err());
    assert_eq!(
        transport.0.lock().unwrap().as_slice(),
        &[checkpoint["body"].clone()]
    );
}

#[tokio::test]
async fn anthropic_prepared_stream_sends_exact_full_checkpoint_body() {
    let transport = Arc::new(CapturedTransport::default());
    let config = AnthropicConfig::new(
        url::Url::parse("https://example.invalid").unwrap(),
        "transport-test-api-key",
        "model-frozen",
    );
    let model = AnthropicModel::with_transport(config, transport.clone());
    exact_prepared_body(&model, transport).await;
}

#[tokio::test]
async fn openai_prepared_stream_sends_exact_full_checkpoint_body() {
    let transport = Arc::new(CapturedTransport::default());
    let config = OpenAiConfig::new(
        url::Url::parse("https://example.invalid").unwrap(),
        "transport-test-api-key",
        "model-frozen",
    );
    let model = OpenAiModel::with_transport(config, transport.clone());
    exact_prepared_body(&model, transport).await;
}

#[tokio::test]
async fn cancelled_prepared_call_has_zero_http_effects() {
    let transport = Arc::new(CapturedTransport::default());
    let model = OpenAiModel::with_transport(
        OpenAiConfig::new(
            url::Url::parse("https://example.invalid").unwrap(),
            "key",
            "model",
        ),
        transport.clone(),
    );
    let prepared = model
        .prepare_stream(ModelRequest {
            messages: vec![ModelMessage::user_text("never sent")],
            ..Default::default()
        })
        .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(prepared.start(cancel).is_err());
    assert!(transport.0.lock().unwrap().is_empty());
}
