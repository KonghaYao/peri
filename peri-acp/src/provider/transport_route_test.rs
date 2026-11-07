use super::*;

#[test]
fn absent_route_preserves_normal_deployment() {
    assert_eq!(parse(None, None).unwrap(), None);
}

#[test]
fn explicit_route_captures_origin_and_run() {
    let route = parse(Some("http://127.0.0.1:9010"), Some("case-one"))
        .unwrap()
        .unwrap();
    assert_eq!(route.proxy_url().as_str(), "http://127.0.0.1:9010/");
    assert_eq!(route.run(), "case-one");
}

fn provider(kind: &str, endpoint: &str) -> super::super::LlmProvider {
    let settings = serde_json::from_value(serde_json::json!({"config": {
        "providers": [{"id":"fixture", "type":kind, "baseUrl":endpoint,
            "apiKey":"fixture-api-key"}],
        "profiles": {"opus": {"provider":"fixture", "model":"fixture-model",
            "effort":"high", "max_tokens":4096, "contextWindow":120000}}
    }}))
    .unwrap();
    super::super::LlmProvider::from_config_for_alias(&settings, "opus").unwrap()
}

#[test]
fn route_preserves_provider_checkpoint_and_effective_profile() {
    let route = parse(Some("http://127.0.0.1:9010"), Some("case-one")).unwrap();
    for (kind, endpoint) in [
        ("anthropic", "https://anthropic.fixture.invalid"),
        ("openai", "https://openai.fixture.invalid/v1"),
        (
            "openai_responses",
            "https://responses.fixture.invalid/v1/responses",
        ),
    ] {
        let provider = provider(kind, endpoint);
        let direct = provider.clone().into_model();
        let routed = provider.into_model_with_transport_route(route.clone());
        let request =
            peri_model::ModelRequest::new(vec![peri_model::ModelMessage::user_text("hello")]);
        let before = direct.prepare_stream(request.clone()).unwrap();
        let after = routed.prepare_stream(request).unwrap();
        assert_eq!(before.checkpoint(), after.checkpoint(), "{kind}");
        assert_eq!(direct.context_window(), routed.context_window());
        assert_eq!(direct.output_token_limit(), routed.output_token_limit());
        assert!(!after.checkpoint().to_string().contains("case-one"));
        assert!(!after.checkpoint().to_string().contains("127.0.0.1"));
        if kind == "openai_responses" {
            assert!(after.checkpoint()["credentialRef"].is_string());
        }
    }
}

#[tokio::test]
async fn routed_provider_sends_original_body_and_auth_only_to_proxy() {
    let mut upstream = mockito::Server::new_async().await;
    let mut proxy = mockito::Server::new_async().await;
    let endpoint = format!("{}/v1/responses", upstream.url());
    let upstream_mock = upstream
        .mock("POST", "/v1/responses")
        .expect(0)
        .create_async()
        .await;
    let terminal = serde_json::json!({"type":"response.completed", "response":{
        "id":"response-fixture", "status":"completed", "output":[{
            "id":"message-fixture", "type":"message", "status":"completed", "role":"assistant",
            "content":[{"type":"output_text", "text":"routed", "annotations":[]}]
        }], "usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}
    }});
    let proxy_mock = proxy
        .mock("POST", "/v1/responses")
        .match_header("authorization", "Bearer fixture-api-key")
        .match_header("x-peri-replay-target", endpoint.as_str())
        .match_header("x-peri-replay-context", mockito::Matcher::Any)
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "model":"fixture-model", "stream":true, "store":false,
            "max_output_tokens":4096, "reasoning":{"effort":"high"}
        })))
        .with_header("content-type", "text/event-stream")
        .with_body(format!("event: response.completed\ndata: {terminal}\n\n"))
        .expect(1)
        .create_async()
        .await;
    let route = parse(Some(&proxy.url()), Some("wire-case")).unwrap();
    let model = provider("openai_responses", &endpoint).into_model_with_transport_route(route);
    let mut request =
        peri_model::ModelRequest::new(vec![peri_model::ModelMessage::user_text("hello")]);
    request.call_context = Some(peri_model::ModelCallContext {
        owner: "session-one".into(),
        execution: "execution-one".into(),
        call: "reason:1".into(),
        purpose: "reason".into(),
        parent: None,
    });
    let response = model
        .complete(request, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.message().text_content().as_deref(), Some("routed"));
    proxy_mock.assert_async().await;
    upstream_mock.assert_async().await;
}

#[test]
fn incomplete_route_never_falls_back_to_direct() {
    for (proxy, run) in [
        (Some("http://127.0.0.1:9010"), None),
        (None, Some("case-one")),
    ] {
        let error = parse(proxy, run).unwrap_err().to_string();
        assert!(error.contains(PROXY_ENV));
        assert!(error.contains(RUN_ENV));
        assert!(error.contains("must be set together"));
    }
}

#[test]
fn invalid_route_fails_with_configuration_diagnostic() {
    for proxy in [
        "",
        "not-a-url",
        "file:///tmp/model",
        "http://user:key@localhost",
        "http://localhost/path",
        "http://localhost?token=synthetic",
        "http://localhost/#fragment",
    ] {
        let error = format!("{:#}", parse(Some(proxy), Some("case-one")).unwrap_err());
        assert!(error.contains(PROXY_ENV), "{proxy}: {error}");
    }
    for run in ["".to_owned(), "   ".to_owned(), "x".repeat(257)] {
        let error = format!(
            "{:#}",
            parse(Some("http://localhost:9010"), Some(&run)).unwrap_err()
        );
        assert!(error.contains(RUN_ENV));
        assert!(error.contains("nonempty run"));
    }
}
