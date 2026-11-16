//! Shared composer side-effect reset fixture.

use crate::kit::atoms::{ACP_STATE, INPUT_BUFFER, VIEW_MODELS, ViewModelsSnapshot};

/// 测试夹具：重置提交/面板相关的全局 atom。
///
/// `input_area_test` 与 `submit_test` 共用这一份（`init_atoms()` 是 no-op，
/// 真正的重置在这里），避免两个测试模块各自复制后漂移。
#[cfg(test)]
pub(in super::super) fn reset_submit_side_effect_state() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    crate::kit::atoms::INPUT_HISTORY.state().write().clear();
    crate::kit::atoms::INPUT_HISTORY_INDEX
        .state()
        .write()
        .take();
    crate::kit::atoms::OPEN_PANELS.state().write().clear();
    crate::kit::atoms::ACTIVE_PANEL.state().write().take();
    *crate::kit::atoms::NOTIFICATION.state().write() = None;
    crate::kit::atoms::PENDING_ATTACHMENTS
        .state()
        .write()
        .clear();
    ACP_STATE.state().write().is_loading = false;
}
