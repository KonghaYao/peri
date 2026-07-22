//! Tests for artifact_client

use super::*;

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
