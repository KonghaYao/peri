//! Tests for popup_overlay

#[cfg(test)]
use super::*;
#[cfg(test)]
use serial_test::serial;

#[cfg(test)]
fn setup_atoms() {
    crate::kit::atoms::init_atoms();
    *atoms::POPUP_KIND.state().write() = None;
    *atoms::POPUP_AREA.state().write() = None;
}

#[test]
#[serial]
fn test_open_popup_sets_atom() {
    setup_atoms();
    open_popup(PopupKind::Hitl);
    assert_eq!(*atoms::POPUP_KIND.state().read(), Some(PopupKind::Hitl));
    // 清理——避免全局 OnceLock atom 在测试间残留 POPUP_KIND 状态
    close_popup();
}

#[test]
#[serial]
fn test_close_popup_returns_previous() {
    setup_atoms();
    open_popup(PopupKind::AskUser);
    let closed = close_popup();
    assert_eq!(closed, Some(PopupKind::AskUser));
    assert_eq!(*atoms::POPUP_KIND.state().read(), None);
}

#[test]
#[serial]
fn test_close_popup_when_empty_returns_none() {
    setup_atoms();
    assert_eq!(close_popup(), None);
}

#[test]
#[serial]
fn test_is_popup_active() {
    setup_atoms();
    assert!(!is_popup_active());
    open_popup(PopupKind::Rewind);
    assert!(is_popup_active());
    close_popup();
    assert!(!is_popup_active());
}

/// 编译期断言：PopupKind 实现 Copy（atom 读取需要 Copy）。
#[test]
fn test_popup_kind_is_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<PopupKind>();
}

/// I21-C 回归保护：close_popup 必须根据关闭的 kind 清空对应 payload atom。
/// 防止未来重构破坏 atom 清空语义——用户打开 popup → 看到 payload → 关闭 →
/// 下次再打开时不应看到陈旧 payload（即使没有新事件）。
#[test]
#[serial]
fn test_close_popup_clears_payload_atoms() {
    use peri_acp_types::event_data::{AskUser, HitlPending, OauthNeeded, Question, RewindPreview};

    setup_atoms();

    // 构造 4 种 popup 的 payload 写入对应 atom
    *atoms::HITL_PENDING.state().write() = Some(crate::kit::acp_types::PendingInteraction {
        request_id_json: "\"hitl\"".into(),
        payload: HitlPending {
            tool_name: "rm".to_string(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    });
    *atoms::ASK_USER_PENDING.state().write() = Some(crate::kit::acp_types::PendingInteraction {
        request_id_json: "\"ask\"".into(),
        payload: AskUser {
            questions: vec![Question {
                id: "q1".to_string(),
                header: "h".to_string(),
                question: "q".to_string(),
                options: vec![],
                multi_select: false,
            }],
        },
    });
    *atoms::REWIND_PREVIEW.state().write() = Some(RewindPreview {
        files: vec![],
        messages: vec![],
    });
    *atoms::OAUTH_INFO.state().write() = Some(OauthNeeded {
        server_name: "test".to_string(),
        auth_url: "https://example.com".to_string(),
    });

    // 逐一关闭每种 popup，验证对应 atom 被清空
    open_popup(PopupKind::Hitl);
    close_popup();
    assert!(
        atoms::HITL_PENDING.state().read().is_none(),
        "HITL_PENDING should be cleared after close_popup"
    );

    open_popup(PopupKind::AskUser);
    close_popup();
    assert!(
        atoms::ASK_USER_PENDING.state().read().is_none(),
        "ASK_USER_PENDING should be cleared after close_popup"
    );

    open_popup(PopupKind::Rewind);
    // 预置预算/目标/错误（close_popup 应清空）
    *atoms::REWIND_TARGET_TEXT.state().write() = Some("t".to_string());
    *atoms::REWIND_BUDGET_STATE.state().write() = atoms::RewindBudgetState::Executing;
    *atoms::REWIND_QUERY_ERROR.state().write() = Some("e".to_string());
    close_popup();
    assert!(
        atoms::REWIND_PREVIEW.state().read().is_some(),
        "REWIND_PREVIEW should NOT be cleared after close_popup — 候选跟随会话生命周期"
    );
    assert!(
        atoms::REWIND_TARGET_TEXT.state().read().is_none()
            && *atoms::REWIND_BUDGET_STATE.state().read() == atoms::RewindBudgetState::Idle
            && atoms::REWIND_QUERY_ERROR.state().read().is_none(),
        "close_popup 应清空预算/目标/查询错误"
    );

    open_popup(PopupKind::OAuth);
    close_popup();
    assert!(
        atoms::OAUTH_INFO.state().read().is_none(),
        "OAUTH_INFO should be cleared after close_popup"
    );
}
