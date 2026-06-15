//! SessionManager 单元测试。
//!
//! 覆盖 `ensure_session` / `goal_state_for` / `cancel_cascade_children_for` /
//! `build_frozen_data` 四个新方法，验证 TUI/stdio 三合一重构后的行为契约。

use std::sync::Arc;

use crate::provider::{LlmProvider, PeriConfig, ProviderConfig, ProviderModels};
use crate::session::SessionManager;
use peri_agent::thread::FilesystemThreadStore;
use peri_middlewares::prelude::{PermissionMode, SharedPermissionMode};

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(id: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: model.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造测试用 SessionManager + 临时 thread store
fn make_session_manager(tmp: &tempfile::TempDir) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_provider_id = "a".to_string();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![make_provider_config("a", "gpt-4o")];
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    SessionManager::new(
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
    )
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// 验证 ensure_session 幂等：重复调用不会覆盖已有记录
#[tokio::test]
async fn test_ensure_session_幂等不覆盖已有记录() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    let session_id = "test-session-idempotent";

    // 第一次插入
    mgr.ensure_session(session_id, "/tmp");
    let goal_state_first = mgr.goal_state_for(session_id);
    assert!(
        goal_state_first.is_some(),
        "首次 ensure_session 后应能取到 goal_state"
    );

    // 第二次插入（幂等）— 不应覆盖已有记录
    mgr.ensure_session(session_id, "/tmp/different");
    let goal_state_second = mgr.goal_state_for(session_id);
    assert!(
        goal_state_second.is_some(),
        "幂等调用后仍应能取到 goal_state"
    );

    // 两次取出的 goal_state 应为同一句柄（Arc 共享）
    let g1 = goal_state_first.unwrap();
    let g2 = goal_state_second.unwrap();
    // 写入一条用户消息，验证两个句柄共享同一内部状态
    g1.put_pending_user_message("hello".to_string());
    assert_eq!(
        g2.take_pending_user_message(),
        Some("hello".to_string()),
        "两次 ensure_session 后的 goal_state 应共享内部状态"
    );
}

/// 验证 goal_state_for 在 session 不存在时返回 None
#[tokio::test]
async fn test_goal_state_for_不存在返回none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    assert!(
        mgr.goal_state_for("non-existent").is_none(),
        "不存在的 session_id 应返回 None"
    );
}

/// 验证 build_frozen_data 返回非空 system_prompt 且日期格式正确
#[tokio::test]
async fn test_build_frozen_data_返回非空system_prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);

    let frozen = mgr.build_frozen_data(tmp.path().to_str().unwrap(), &[], &[]);
    assert!(
        !frozen.system_prompt().is_empty(),
        "frozen system_prompt 不应为空"
    );
    // 日期格式 YYYY-MM-DD（10 字符，含两个连字符）
    let date_chars: Vec<char> = frozen.date().chars().collect();
    assert_eq!(date_chars.len(), 10, "日期长度应为 10");
    assert_eq!(date_chars[4], '-', "第 5 个字符应为连字符");
    assert_eq!(date_chars[7], '-', "第 8 个字符应为连字符");
}

/// 验证 cancel_cascade_children_for 在 session 不存在时不 panic
#[tokio::test]
async fn test_cancel_cascade_children_for_不存在不panic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    // 不应 panic
    mgr.cancel_cascade_children_for("non-existent");
}

/// 验证 close_session 移除 AcpSession 记录后 goal_state_for 返回 None
#[tokio::test]
async fn test_close_session_移除记录后goal_state返回none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    let session_id = "test-close-session";

    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.goal_state_for(session_id).is_some());

    mgr.close_session(session_id).await.unwrap();
    assert!(
        mgr.goal_state_for(session_id).is_none(),
        "close_session 后 goal_state_for 应返回 None"
    );
}
