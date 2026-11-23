//! System event handlers — BudgetWarning, SystemNotification, Prediction,
//! FileSuggestions, Interaction events (HitlPending, AskUser, RewindPreview,
//! RewindCompleted, OauthNeeded), Background Tasks, Plugin events, WorkflowProgress,
//! Unknown.
//!
//! 模块结构：Interaction 事件（HitlPending / AskUser 弹窗与授权块）见 `interaction`，
//! Background Tasks（快照 / 增量与任务生命周期）见 `bg_task`；系统提示与提醒、
//! rewind / OAuth 结果、插件与 workflow 事件留在本模块。

mod bg_task;
mod interaction;

#[cfg(test)]
#[path = "system/child_reminder_test.rs"]
mod child_reminder_tests;

use super::BridgeState;

#[cfg(test)]
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use crate::kit::tui_render_unit::TuiSystemReminder;

mod system_projection;
pub(super) use system_projection::*;

#[cfg(test)]
pub(crate) use self::bg_task::apply_bg_task_snapshot;
pub(crate) use self::bg_task::request_bg_task_snapshot;
pub(super) use self::bg_task::{
    handle_bg_task_cancelled, handle_bg_task_completed, handle_bg_task_snapshot,
    handle_bg_task_started, handle_bg_task_updated, has_active_async_agent,
};
#[cfg(test)]
pub(crate) use self::interaction::hitl_input_summary;
pub(super) use self::interaction::{
    handle_ask_user, handle_hitl_pending, handle_interaction_terminal, handle_url_elicitation,
};
