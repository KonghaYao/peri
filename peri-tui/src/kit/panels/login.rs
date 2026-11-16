//! ratatui-kit LoginPanel component.
//!
//! Login 是宿主配置的 Provider 编辑器：Browse 列表来自 `PROVIDER_LIST` 投影，
//! Edit 使用 typed 草稿（三态类型、API key / 既有 ChatGPT plan 引用、provider 四档
//! 默认模型）。档位绑定、模型与 contextWindow 由 Model 面板负责；新建 provider
//! 不写任何 profile。保存捕获进入时的 revision，只发布 authority 接纳后的快照；
//! 失败保持草稿与字段错误。
//!
//! Browse：只读列表 + Enter/鼠标点击进入编辑。
//! Edit：动态字段导航、Ctrl+S/Confirm 保存、Esc 放弃；已有 provider 的类型
//! 只有显式确认子视图才能更改（方向键不会误改）。Ctrl+R 重读权威配置并重新
//! 基线化草稿，是 revision 冲突后唯一可用的面板内恢复动作。

mod auth_policy;
mod auth_render;
mod auth_ui;
mod browse_render;
mod config_store;
mod edit_errors;
mod edit_handler;
mod edit_state;
mod edit_types;
mod edit_view;
mod plan;

#[cfg(test)]
#[path = "login/plan_test.rs"]
mod plan_tests;
mod render;

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;

#[cfg(test)]
use self::edit_state::LoginEditState;
#[cfg(test)]
use self::edit_state::LoginField;
use crate::chatgpt_auth as auth;
#[cfg(test)]
use ratatui_kit::crossterm::event::KeyCode;
#[cfg(test)]
use ratatui_kit::crossterm::event::KeyModifiers;

mod login_panel;
pub use login_panel::*;

#[cfg(test)]
use crate::kit::atoms::NOTIFICATION;

#[cfg(test)]
use crate::kit::atoms::Notification;

#[cfg(test)]
use crate::kit::atoms::ProviderSummary;

#[cfg(test)]
use crate::kit::panels::shared::list::content_rows;
