//! Login 流程回归：冲突提示分类，以及 Model → Login 的一次性导航请求。

use super::*;
use crate::kit::panels::login::edit_types::LoginError;

/// 冲突提示只属于 revision/authority 冲突；字段校验错误不得显示“重新载入”。
#[test]
#[serial]
fn conflict_reload_hint_is_reserved_for_conflict_errors() {
    let cfg = login_config();
    let auth = crate::kit::panels::login::auth::AuthController::new();
    let colors = crate::kit::panels::login::render::LoginRenderColors {
        cursor: ratatui_kit::ratatui::style::Color::Reset,
        dim: ratatui_kit::ratatui::style::Color::Reset,
        text: ratatui_kit::ratatui::style::Color::Reset,
        focus: ratatui_kit::ratatui::style::Color::Reset,
    };
    let render_text = |state: &LoginEditState| {
        crate::kit::panels::login::render::build_edit_lines(
            state,
            &auth,
            LoginField::ProfileWindow,
            0,
            &colors,
        )
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>()
    };

    let mut field_error = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    field_error.inline_error = Some(LoginError::field(
        LoginField::ProfileWindow,
        crate::i18n::tr("login-error-context-window"),
    ));
    let text = render_text(&field_error);
    assert!(
        text.contains(&crate::i18n::tr("login-error-context-window"))
            && !text.contains(&crate::i18n::tr("login-conflict-reload-hint")),
        "字段错误只显示字段提示，不得显示冲突重载提示：\n{text}"
    );

    let mut conflict = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    conflict.inline_error = Some(LoginError::conflict(crate::i18n::tr(
        "config-draft-conflict",
    )));
    let text = render_text(&conflict);
    assert!(
        text.contains(&crate::i18n::tr("config-draft-conflict"))
            && text.contains(&crate::i18n::tr("login-conflict-reload-hint")),
        "revision 冲突必须显示可操作的重载提示：\n{text}"
    );
}

/// Model 的“管理 Provider”一次性请求：Login 打开后直接编辑目标 provider，
/// 请求消费后清空；不复制任何草稿状态。
#[test]
#[serial]
fn provider_management_request_opens_target_provider_edit_once() {
    use crate::config::PeriConfig;
    use crate::kit::atoms::{LOGIN_PANEL_REQUEST, LoginPanelRequest, PERI_CONFIG_HANDLE};
    use std::sync::Arc;

    let _list = super::ProviderListGuard::install(vec![]);
    let _ = PERI_CONFIG_HANDLE.set(Arc::new(parking_lot::RwLock::new(PeriConfig::default())));
    let handle = Arc::clone(PERI_CONFIG_HANDLE.get().expect("配置句柄必须存在"));
    let previous = handle.read().clone();
    *handle.write() = login_config();
    *LOGIN_PANEL_REQUEST.state().write() = Some(LoginPanelRequest {
        provider_id: Some("resp".to_string()),
    });

    let (tx, rx) = std::sync::mpsc::channel();
    super::mount_panel_at(2, move |panel| {
        if *panel.mode.read() == LoginPanelMode::Edit {
            tx.send((
                *panel.mode.read(),
                *panel.edit_focus.read(),
                panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(|state| state.original_provider_id.clone()),
                *panel.edit_cursor.read(),
            ))
            .expect("测试结果通道仍打开");
        }
    });

    let (mode, focus, provider, cursor) = rx.try_recv().expect("请求必须进入编辑视图");
    assert_eq!(mode, LoginPanelMode::Edit);
    assert_eq!(focus, LoginField::ProviderId);
    assert_eq!(provider.as_deref(), Some("resp"));
    assert_eq!(cursor, "resp".chars().count());
    assert!(
        LOGIN_PANEL_REQUEST.state().read().is_none(),
        "导航请求是一次性的"
    );
    *handle.write() = previous;
}
