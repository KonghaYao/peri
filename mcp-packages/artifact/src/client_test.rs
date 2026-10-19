//! Tests for artifact_client

use super::*;

#[test]
fn instance_environment_selects_artifact_endpoint_and_token() {
    let env = std::collections::HashMap::from([
        (
            "PERI_ARTIFACTS_URL".to_owned(),
            "https://instance.example".to_owned(),
        ),
        (
            "PERI_ARTIFACTS_TOKEN".to_owned(),
            "instance-token".to_owned(),
        ),
    ]);
    let client = ArtifactClient::from_instance_env(&env);
    assert_eq!(client.upload_url(), "https://instance.example/upload");
    assert_eq!(client.token, "instance-token");
    let default_client = ArtifactClient::from_instance_env(&Default::default());
    assert_eq!(default_client.token, DEFAULT_TOKEN);
}

#[tokio::test]
async fn test_build_url_default() {
    let client = ArtifactClient::default();
    assert_eq!(
        client.upload_url(),
        "https://cloud-artifacts.claude-code-best.win/upload"
    );
}

#[tokio::test]
async fn test_build_url_custom() {
    let client = ArtifactClient::new("https://example.com".into(), "mytoken".into());
    assert_eq!(client.upload_url(), "https://example.com/upload");
}

#[tokio::test]
async fn test_parse_success_response() {
    let body = r#"{"id":"abc123","url":"https://cloud-artifacts.claude-code-best.win/7d/abc123.html","expiresAt":"2026-06-27T12:00:00Z"}"#;
    let resp: ArtifactResponse = serde_json::from_str(body).unwrap();
    assert_eq!(resp.id, "abc123");
    assert_eq!(
        resp.url,
        "https://cloud-artifacts.claude-code-best.win/7d/abc123.html"
    );
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn test_parse_error_response() {
    // Deno Deploy 抹平 HTTP 状态码为 200，错误信息在 body 中
    let body = r#"{"error":"payload_too_large"}"#;
    let resp: ArtifactResponse = serde_json::from_str(body).unwrap();
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap(), "payload_too_large");
}

#[tokio::test]
async fn test_format_output_success() {
    let resp = ArtifactResponse {
        id: "abc123".into(),
        url: "https://cloud-artifacts.claude-code-best.win/7d/abc123.html".into(),
        expires_at: Some("2026-06-27T12:00:00Z".into()),
        error: None,
    };
    let output = ArtifactClient::format_output(&resp);
    assert!(output.contains("Artifact uploaded:"));
    assert!(output.contains("https://cloud-artifacts.claude-code-best.win/7d/abc123.html"));
    assert!(output.contains("2026-06-27T12:00:00Z"));
    // tool_result 不应含 OSC 8 转义序列（避免 LLM 把 ESC 字符当字面文本回显）
    assert!(
        !output.contains('\x1b'),
        "format_output 不应含 ESC 控制字符，实际: {:?}",
        output
    );
}

#[tokio::test]
async fn test_format_output_no_expiry() {
    let resp = ArtifactResponse {
        id: "abc123".into(),
        url: "https://example.com/file.html".into(),
        expires_at: None,
        error: None,
    };
    let output = ArtifactClient::format_output(&resp);
    assert!(output.contains("Artifact uploaded:"));
    assert!(!output.contains("Expires:"));
}

#[test]
fn non_success_status_is_an_error_and_prefers_server_error_text() {
    let error = ArtifactClient::interpret_upload_response(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":"payload_too_large"}"#,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.starts_with("HTTP 500"), "{message}");
    assert!(message.contains("payload_too_large"), "{message}");
}

#[test]
fn non_success_status_without_json_keeps_truncated_body() {
    let error = ArtifactClient::interpret_upload_response(
        reqwest::StatusCode::BAD_GATEWAY,
        "<html>upstream unavailable</html>",
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("HTTP 502"), "{message}");
    assert!(message.contains("upstream unavailable"), "{message}");
}

#[test]
fn error_body_is_capped_to_keep_model_output_bounded() {
    let huge_body = "x".repeat(MAX_ERROR_BODY_BYTES * 4);
    let error = ArtifactClient::interpret_upload_response(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        &huge_body,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.len() < MAX_ERROR_BODY_BYTES + 200, "{message}");
    assert!(message.contains("truncated"), "{message}");
}

#[test]
fn success_status_with_error_field_is_a_failure() {
    let error = ArtifactClient::interpret_upload_response(
        reqwest::StatusCode::OK,
        r#"{"error":"token 无效"}"#,
    )
    .unwrap_err();
    assert!(matches!(error, ArtifactError::Server { .. }));
    assert!(error.to_string().starts_with("Upload error: token 无效"));
}

#[test]
fn server_error_field_is_capped_to_keep_model_output_bounded() {
    let body = serde_json::json!({ "error": "y".repeat(MAX_ERROR_BODY_BYTES * 4) }).to_string();
    let error =
        ArtifactClient::interpret_upload_response(reqwest::StatusCode::OK, &body).unwrap_err();
    let message = error.to_string();
    assert!(message.len() < MAX_ERROR_BODY_BYTES + 200, "{message}");
    assert!(message.contains("truncated"), "{message}");
}

#[test]
fn malformed_json_is_a_parse_error() {
    let error =
        ArtifactClient::interpret_upload_response(reqwest::StatusCode::OK, "<html>not json</html>")
            .unwrap_err();
    assert!(matches!(error, ArtifactError::Parse { .. }));
    let message = error.to_string();
    assert!(message.starts_with("Failed to parse response"), "{message}");
    assert!(message.contains("not json"), "{message}");
}

#[test]
fn empty_object_is_a_missing_url_error() {
    let error =
        ArtifactClient::interpret_upload_response(reqwest::StatusCode::OK, "{}").unwrap_err();
    assert!(matches!(error, ArtifactError::MissingUrl));
    assert!(error.to_string().contains("no usable URL"));
}

#[test]
fn whitespace_url_is_a_missing_url_error() {
    let error =
        ArtifactClient::interpret_upload_response(reqwest::StatusCode::OK, r#"{"url":"   "}"#)
            .unwrap_err();
    assert!(matches!(error, ArtifactError::MissingUrl));
}

#[test]
fn relative_or_non_http_url_is_invalid() {
    for url in [
        "not-a-url",
        "/relative/path.html",
        "ftp://example.com/file.html",
    ] {
        let body = serde_json::json!({ "url": url }).to_string();
        let error =
            ArtifactClient::interpret_upload_response(reqwest::StatusCode::OK, &body).unwrap_err();
        assert!(
            matches!(error, ArtifactError::InvalidUrl { .. }),
            "{url}: {error}"
        );
    }
}

#[test]
fn valid_success_response_is_accepted() {
    let response = ArtifactClient::interpret_upload_response(
        reqwest::StatusCode::OK,
        r#"{"url":" https://example.com/file.html ","expiresAt":"2026-06-27T12:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(response.url, "https://example.com/file.html");
    let output = ArtifactClient::format_output(&response);
    assert!(output.contains("https://example.com/file.html"));
    assert!(output.contains("Expires: 2026-06-27T12:00:00Z"));
}
