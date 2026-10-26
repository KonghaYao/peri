use super::*;
use peri_model::{ModelMessage, ModelRequest, ProviderProtocol};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn settings(endpoint: &str) -> PeriConfig {
    serde_json::from_value(json!({"config": {
        "active_alias":"opus",
        "providers":[{"id":"responses", "type":"openai_responses", "baseUrl":endpoint,
            "apiKey":"fixture-api-key", "models":{"opus":"main-model", "haiku":"small-model"}}],
        "profiles":{
            "opus":{"provider":"responses","effort":"high","max_tokens":4096,"contextWindow":120000},
            "haiku":{"provider":"responses","effort":"low","max_tokens":2048,"contextWindow":64000}
        }
    }})).unwrap()
}

#[test]
fn test_responses_main_and_auxiliary_factory_preserve_profiles() {
    let settings = settings("https://api.example.invalid/v1/responses");
    for (alias, expected_model, effort, capacity, tokens) in [
        ("opus", "main-model", "high", 120000, 4096),
        ("haiku", "small-model", "low", 64000, 2048),
    ] {
        let provider = LlmProvider::from_config_for_alias(&settings, alias).unwrap();
        assert_eq!(provider.effective_context_window(), capacity);
        let model = provider.into_model();
        assert_eq!(model.context_window(), Some(capacity));
        assert_eq!(model.output_token_limit(), Some(tokens));
        let request = model.prepare_request(&ModelRequest::default()).unwrap();
        assert_eq!(request.protocol(), &ProviderProtocol::OpenAiResponses);
        assert_eq!(request.model_id(), expected_model);
        assert_eq!(request.endpoint().path(), "/v1/responses");
        assert_eq!(request.body().as_value()["reasoning"]["effort"], effort);
        assert_eq!(request.body().as_value()["max_output_tokens"], tokens);
    }
}

#[test]
fn test_subscription_factory_exposes_unknown_output_limit_and_no_api_key_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings = settings("https://api.openai.com/v1/responses");
    settings.config.providers[0].api_key.clear();
    settings.config.providers[0].auth = Some(config::ProviderAuthConfig::ChatGptPlan {
        store: directory.path().to_owned(),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    let model = LlmProvider::from_config_for_alias(&settings, "opus")
        .unwrap()
        .with_max_tokens(512)
        .into_model();
    assert_eq!(model.output_token_limit(), None);
    assert_eq!(model.context_window(), Some(120000));
    let request = model.prepare_request(&ModelRequest::default());
    #[cfg(windows)]
    assert!(request
        .err()
        .expect("Windows must reject subscription credential storage")
        .to_string()
        .contains("requires Unix owner-only file permissions"));
    #[cfg(not(windows))]
    {
        let request = request.unwrap();
        assert!(request.body().as_value().get("max_output_tokens").is_none());
        assert!(request.body().as_value().get("temperature").is_none());
    }
}

#[tokio::test]
async fn test_unlogged_subscription_fails_before_any_model_request() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings = settings("https://api.openai.com/v1/responses");
    settings.config.providers[0].api_key.clear();
    settings.config.providers[0].auth = Some(config::ProviderAuthConfig::ChatGptPlan {
        store: directory.path().join("credentials"),
        account: "work".into(),
        subject: "subject-test".into(),
        client_id: "issued-client-test".into(),
    });
    let model = LlmProvider::from_config_for_alias(&settings, "opus")
        .unwrap()
        .into_model();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        model.complete(
            ModelRequest::new(vec![ModelMessage::user_text("hello")]),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("未登录必须在凭据能力边界立即失败")
    .unwrap_err();
    #[cfg(windows)]
    assert!(
        error
            .to_string()
            .contains("requires Unix owner-only file permissions"),
        "{error}"
    );
    #[cfg(not(windows))]
    assert!(error.to_string().contains("Sign in to ChatGPT"), "{error}");
}

#[tokio::test]
async fn test_config_to_responses_http_post_uses_resolved_profile() {
    let mut server = mockito::Server::new_async().await;
    let terminal = json!({"type":"response.completed", "response":{
        "id":"response-fixture", "status":"completed", "output":[{
            "id":"message-fixture", "type":"message", "status":"completed", "role":"assistant",
            "content":[{"type":"output_text", "text":"done", "annotations":[]}]
        }], "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}
    }});
    let reply = format!("event: response.completed\ndata: {terminal}\n\n");
    let mock = server
        .mock("POST", "/v1/responses")
        .match_header("authorization", "Bearer fixture-api-key")
        .match_body(mockito::Matcher::PartialJson(json!({
            "model":"small-model", "stream":true, "store":false,
            "max_output_tokens":2048, "reasoning":{"effort":"low"}
        })))
        .with_header("content-type", "text/event-stream")
        .with_body(reply)
        .expect(1)
        .create_async()
        .await;
    let settings = settings(&format!("{}/v1/responses", server.url()));
    let model = LlmProvider::from_config_for_alias(&settings, "haiku")
        .unwrap()
        .into_model();
    let response = model
        .complete(
            ModelRequest::new(vec![ModelMessage::user_text("hello")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.message().text_content().as_deref(), Some("done"));
    mock.assert_async().await;
}
