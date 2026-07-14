use std::path::PathBuf;

use super::*;
use crate::hooks::types::HookEvent;

fn make_registered() -> RegisteredHook {
    RegisteredHook {
        hook: serde_json::from_str(r#"{"type":"command","command":"echo"}"#).unwrap(),
        event: HookEvent::PreToolUse,
        matcher: None,
        plugin_name: "test-plugin".to_string(),
        plugin_id: "test-id".to_string(),
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: std::collections::HashMap::new(),
    }
}

fn make_hook_input() -> HookInput {
    HookInput::session_start(
        "sess-1",
        "/tmp/transcript.json",
        "/project",
        "startup",
        "opus",
    )
}

fn make_command_hook(command: &str) -> HookType {
    serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": command
    }))
    .unwrap()
}

#[tokio::test]
async fn test_command_hook_echo_plain_text() {
    let hook = make_command_hook("cat");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_exit_code_2_blocks() {
    let hook = make_command_hook("exit 2");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

#[tokio::test]
async fn test_command_hook_exit_code_1_allows() {
    let hook = make_command_hook("echo 'error msg' >&2 && exit 1");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_json_output_continue_false() {
    let hook = make_command_hook(r#"echo '{"continue":false,"stopReason":"test stop"}'"#);
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::PreventContinuation {
            stop_reason: Some(ref s)
        } if s == "test stop"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_json_output_block() {
    let hook = make_command_hook(r#"echo '{"decision":"block","reason":"not allowed"}'"#);
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::Block {
            reason: ref r
        } if r == "not allowed"
    ));
}

#[tokio::test]
async fn test_command_hook_timeout() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": "sleep 10",
        "timeout": 1
    }))
    .unwrap();
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_exit_code_2_with_stdout_reason() {
    let hook = make_command_hook("echo 'custom block reason' && exit 2");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::Block {
            reason: ref r
        } if r == "custom block reason"
    ));
}

#[tokio::test]
async fn test_command_hook_plugin_options_env() {
    let mut registered = make_registered();
    registered
        .plugin_options
        .insert("api_key".to_string(), serde_json::json!("sk-test-123"));

    let hook = make_command_hook("echo $CLAUDE_PLUGIN_OPTION_API_KEY");
    let input = make_hook_input();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

// === sanitize_header_value tests ===

#[test]
fn test_sanitize_crlf_injection() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\r\nX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_lf_only() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\nX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_cr_only() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\rX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_env_var_expansion_allowed() {
    std::env::set_var("TEST_SANITIZE_HOOK_VAR", "secret-value");
    let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_VAR".to_string()].into_iter().collect();
    let result = sanitize_header_value("Bearer ${TEST_SANITIZE_HOOK_VAR}", &allowed);
    assert_eq!(result, "Bearer secret-value");
    std::env::remove_var("TEST_SANITIZE_HOOK_VAR");
}

#[test]
fn test_sanitize_env_var_expansion_not_allowed() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("Bearer ${SECRET_KEY}", &allowed);
    assert_eq!(result, "Bearer ${SECRET_KEY}");
}

#[test]
fn test_sanitize_env_var_brace_expansion() {
    std::env::set_var("TEST_SANITIZE_HOOK_BRACE", "expanded");
    let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_BRACE".to_string()]
        .into_iter()
        .collect();
    let result = sanitize_header_value("token-${TEST_SANITIZE_HOOK_BRACE}", &allowed);
    assert_eq!(result, "token-expanded");
    std::env::remove_var("TEST_SANITIZE_HOOK_BRACE");
}

// === HTTP hook tests (no mock server, just SSRF/blocking logic) ===

#[tokio::test]
async fn test_http_hook_ssrf_blocked() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "http://192.168.1.1/hook",
        "timeout": 5
    }))
    .unwrap();
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

#[tokio::test]
async fn test_http_hook_invalid_url() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "not-a-valid-url",
        "timeout": 5
    }))
    .unwrap();
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

// === Wrong hook type dispatch tests ===

#[tokio::test]
async fn test_command_hook_wrong_type_returns_allow() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "http://example.com"
    }))
    .unwrap();
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_prompt_hook_wrong_type_returns_allow() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": "echo test"
    }))
    .unwrap();
    let input = make_hook_input();
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!());
    let action = execute_prompt_hook(&hook, &input, &llm_factory).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_http_hook_wrong_type_returns_allow() {
    let hook = make_command_hook("echo test");
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_agent_hook_wrong_type_returns_allow() {
    let hook = make_command_hook("echo test");
    let input = make_hook_input();
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!());
    let action = execute_agent_hook(&hook, &input, &llm_factory, "/tmp").await;
    assert!(matches!(action, HookAction::Allow));
}
