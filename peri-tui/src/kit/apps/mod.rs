//! MCP Apps 实时视图：发现 → attach → 读 UI HTML → 网关帧 → 面板；
//! 阶段 2 增加画布左键点击与 App `tools/call` 中继。
//!
//! 模块划分：
//! - [`config`]：开关 + `PERI_BROWSER_VIEW_URL` / `PERI_BROWSER_VIEW_TOKEN` 解析；
//! - [`view_client`] / [`sse`]：网关 view-session HTTP 面与 SSE 解码；
//! - [`state`]：发现结果、多视图状态机与全局 atom；
//! - [`runtime`]：轮询、attach/read、建会话、SSE 重连与清理；
//! - [`image_cache`]：面板帧的终端协议编码（后台线程）；
//! - [`mapping`]：终端单元格 → CSS 视口坐标（面板点击使用）。
//! - [`request_queue`]：`app-request` wire 类型与有界队列契约。
//!
//! 鼠标坐标只来自真实 `MouseEvent`；App 回调只经 `apps/request`，不产生 LLM turn。

pub mod config;
pub mod image_cache;
pub mod mapping;
pub mod request_queue;
pub mod runtime;
pub mod sse;
pub mod state;
pub mod view_client;

#[cfg(test)]
pub(crate) mod test_support;

pub use config::{AppsAvailability, AppsConfig, ViewToken};
pub use request_queue::PendingAppRequest;
pub use runtime::request_view_click;
pub use state::{AppEntry, AppView, AppsRuntimeState, AppsState, FrameImage, ViewPhase};
pub use view_client::{FrameEvent, ViewEvent};

use crate::app::panel_types::PanelKind;

/// 工具卡 → App 的匹配（A1）：同一 ACP 会话 + 模型 tool-call ID。
#[must_use]
pub fn app_for_tool_card(session_id: &str, tool_call_id: &str) -> Option<AppEntry> {
    state::app_for_tool_call(session_id, tool_call_id)
}

/// 正文空档折叠组：唯一成员是当前会话的 App 工具卡时返回该 App。
///
/// 组头默认折叠，工具卡的「打开应用」动作不可见；组头因此复用同一条动作
/// （点击组头 = 展开组 + 打开应用）。多个 App 成员返回 `None`，避免把一次
/// 点击变成「随机打开其中一个」。
#[must_use]
pub fn app_for_collapsed_group(
    group: &crate::kit::tui_render_unit::TuiCollapsedGroup,
) -> Option<AppEntry> {
    let session_id = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut found = None;
    for vm in &group.view_models {
        let crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card) = vm else {
            continue;
        };
        if card.is_running || card.is_error {
            continue;
        }
        let Some(entry) = app_for_tool_card(&session_id, &card.tool_id) else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some(entry);
    }
    found
}

/// 工具卡点击入口：命中已发现的 App 时打开面板并请求 attach。
///
/// 返回 `false` 表示无匹配或运行时未启动——调用方保留原有折叠行为。
pub fn open_from_tool_card(session_id: &str, tool_call_id: &str) -> bool {
    let Some(entry) = app_for_tool_card(session_id, tool_call_id) else {
        return false;
    };
    open_app(&entry.app_id)
}

/// 面板/命令入口：按 app_id 打开（运行时未启动时返回 `false`）。
pub fn open_app(app_id: &str) -> bool {
    if !runtime::request_open_app(app_id) {
        state::set_open_error(Some("apps_runtime_inactive".to_string()));
        return false;
    }
    crate::kit::panel_registry::open_panel(PanelKind::McpApps);
    true
}

/// 面板关闭：关闭所有视图（运行时对每个视图 DELETE 网关会话）。
pub fn close_all_views() {
    runtime::request_close_all();
}

/// 关闭单个视图（面板 `x`）。
pub fn close_view(view_id: &str) {
    let _ = runtime::request_close_view(view_id);
}

/// 运行时是否已激活（面板据此区分「未启用」与「暂无 App」）。
#[must_use]
pub fn runtime_active() -> bool {
    matches!(
        state::APPS_STATE.state().read().runtime,
        AppsRuntimeState::Active
    )
}

/// 工具卡渲染缓存用的版本号（descriptor 变化时 +1）。
#[must_use]
pub fn registry_revision() -> u64 {
    state::registry_revision()
}
