//! Tests for auto_classifier

use std::sync::atomic::{AtomicUsize, Ordering};

use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ProtocolErrorKind, StopReason,
};
use tokio_util::sync::CancellationToken;

use super::*;

struct MockClassifyModel {
    response: std::sync::Mutex<String>,
    call_count: AtomicUsize,
    should_fail: std::sync::Mutex<bool>,
}

impl MockClassifyModel {
    fn new(response: &str) -> Self {
        Self {
            response: std::sync::Mutex::new(response.to_string()),
            call_count: AtomicUsize::new(0),
            should_fail: std::sync::Mutex::new(false),
        }
    }

    fn _call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }

    fn set_should_fail(&self, fail: bool) {
        *self.should_fail.lock().unwrap() = fail;
    }
}

#[async_trait]
impl Model for MockClassifyModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        // 分类路径只走 complete()，stream() 不应被调用
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        if *self.should_fail.lock().unwrap() {
            return Err(ModelError::protocol(ProtocolErrorKind::Provider));
        }
        self.call_count.fetch_add(1, Ordering::Relaxed);
        Ok(ModelResponse::new(
            ModelMessage::assistant_text(self.response.lock().unwrap().clone()),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

#[test]
fn test_classification_variants() {
    assert_ne!(Classification::Allow, Classification::Deny);
    assert_ne!(Classification::Allow, Classification::Unsure);
    assert_ne!(Classification::Deny, Classification::Unsure);
    let _ = Classification::Unsure;
}

#[test]
fn test_cache_key_same_input() {
    let input = serde_json::json!({"cmd": "ls"});
    let (name1, hash1) = LlmAutoClassifier::cache_key("Bash", &input);
    let (name2, hash2) = LlmAutoClassifier::cache_key("Bash", &input);
    assert_eq!(name1, name2);
    assert_eq!(hash1, hash2);
}

#[test]
fn test_cache_key_different_input() {
    let input1 = serde_json::json!({"cmd": "ls"});
    let input2 = serde_json::json!({"cmd": "rm -rf /"});
    let (_, hash1) = LlmAutoClassifier::cache_key("Bash", &input1);
    let (_, hash2) = LlmAutoClassifier::cache_key("Bash", &input2);
    assert_ne!(hash1, hash2);
}

#[tokio::test]
async fn test_classify_allow() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("ALLOW")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Allow);
}

#[tokio::test]
async fn test_classify_deny() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("DENY")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "rm -rf /"}))
        .await;
    assert_eq!(result, Classification::Deny);
}

#[tokio::test]
async fn test_classify_unsure() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("UNSURE")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Unsure);
}

#[tokio::test]
async fn test_classify_garbage_response() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("xyz123")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Unsure);
}

#[tokio::test]
async fn test_classify_llm_failure() {
    let mock = MockClassifyModel::new("ALLOW");
    mock.set_should_fail(true);
    let model = Arc::new(AsyncMutex::new(Box::new(mock) as Box<dyn peri_model::Model>));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Unsure);
}

#[tokio::test]
async fn test_cache_hit() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("ALLOW")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let input = serde_json::json!({"cmd": "ls"});
    classifier.classify("Bash", &input).await;
    // 缓存命中验证通过 cache_key + lookup_cache 间接测试
    let key = LlmAutoClassifier::cache_key("Bash", &input);
    assert!(classifier.lookup_cache(&key).is_some());
}

#[tokio::test]
async fn test_cache_expiry() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("ALLOW")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::with_cache_ttl(model, Duration::from_millis(50));
    let input = serde_json::json!({"cmd": "ls"});
    classifier.classify("Bash", &input).await;
    // 等待缓存过期
    tokio::time::sleep(Duration::from_millis(60)).await;
    let key = LlmAutoClassifier::cache_key("Bash", &input);
    assert!(classifier.lookup_cache(&key).is_none(), "缓存应已过期");
}

// ─── 子串误判防护测试 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_not_allow_should_not_match() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("NOT ALLOW")) as Box<dyn peri_model::Model>,
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "rm -rf /"}))
        .await;
    assert_ne!(result, Classification::Allow, "NOT ALLOW 不应被判为 Allow");
    assert_eq!(result, Classification::Unsure);
}

#[tokio::test]
async fn test_disallow_should_not_match() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("DISALLOW")) as Box<dyn peri_model::Model>,
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "rm -rf /"}))
        .await;
    assert_ne!(result, Classification::Allow, "DISALLOW 不应被判为 Allow");
    assert_eq!(
        result,
        Classification::Unsure,
        "DISALLOW 应判为 Unsure（无独立 DENY/ALLOW）"
    );
}

#[tokio::test]
async fn test_allow_as_standalone_word() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("ALLOW")) as Box<dyn peri_model::Model>
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Allow, "独立 ALLOW 应判为 Allow");
}

#[tokio::test]
async fn test_i_allow_this() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("I ALLOW THIS")) as Box<dyn peri_model::Model>,
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "ls"}))
        .await;
    assert_eq!(result, Classification::Allow, "I ALLOW THIS 应判为 Allow");
}

#[tokio::test]
async fn test_i_deny_this() {
    let model = Arc::new(AsyncMutex::new(
        Box::new(MockClassifyModel::new("I DENY THIS")) as Box<dyn peri_model::Model>,
    ));
    let classifier = LlmAutoClassifier::new(model);
    let result = classifier
        .classify("Bash", &serde_json::json!({"cmd": "rm -rf /"}))
        .await;
    assert_eq!(result, Classification::Deny, "I DENY THIS 应判为 Deny");
}
