use super::*;

#[test]
fn test_bind_failed_error_format() {
    let err = CallbackError::BindFailed("addr in use".to_string());
    assert!(err.to_string().contains("绑定失败"));
}

#[test]
fn test_bind_returns_valid_redirect_uri() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(OAuthCallbackServer::bind());
    assert!(result.is_ok());
    let (_server, uri) = result.unwrap();
    assert!(uri.starts_with("http://127.0.0.1:"));
    assert!(uri.ends_with("/callback"));
}

#[test]
fn test_parse_callback_url_valid() {
    let result = parse_callback_url("/callback?code=abc123&state=mystate");
    assert!(result.is_ok());
    let (code, state) = result.unwrap();
    assert_eq!(code, "abc123");
    assert_eq!(state, "mystate");
}

#[test]
fn test_parse_callback_url_missing_code() {
    let result = parse_callback_url("/callback?state=mystate");
    assert!(result.is_err());
}

#[test]
fn test_parse_callback_url_missing_state() {
    let result = parse_callback_url("/callback?code=abc123");
    assert!(result.is_err());
}

#[test]
fn test_parse_callback_url_invalid_path() {
    let result = parse_callback_url("not-a-url");
    assert!(result.is_err());
}

/// [回归测试] parse_callback_url 不做 CSRF 校验
///
/// 历史背景：曾经存在 state_param 字段做伪校验，但因为从未赋值
/// 导致校验永远跳过。修复后 CSRF 完全委托 rmcp 在 token 交换阶段
/// 通过 state_store 查找完成。本测试明确断言：传入任意 state 都能
/// 成功解析返回，不再有「state 不匹配」错误路径。
#[test]
fn test_parse_callback_url_does_not_validate_csrf() {
    // 任意 state（包括看起来「错误」的）都应原样返回
    let result = parse_callback_url("/callback?code=abc&state=any_value");
    assert!(result.is_ok());
    let (code, state) = result.unwrap();
    assert_eq!(code, "abc");
    assert_eq!(state, "any_value");
}

#[test]
fn test_parse_code_from_url_valid() {
    let result = parse_code_from_url("http://localhost:12345/callback?code=xyz&state=s");
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_wait_for_code_timeout() {
    let (server, _uri) = OAuthCallbackServer::bind().await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        server.wait_for_code(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_bind_multiple_servers() {
    let (s1, uri1) = OAuthCallbackServer::bind().await.unwrap();
    let (s2, uri2) = OAuthCallbackServer::bind().await.unwrap();
    assert_ne!(uri1, uri2);
    drop(s1);
    drop(s2);
}
