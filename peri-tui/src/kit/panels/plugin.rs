//! ratatui-kit PluginPanel v2 component.
//!
//! v2 Phase 2: 4-tab multi-view with dual-mode (list/detail) state machine.
//! ←/→ 切换视图，↑/↓ 导航，Enter 进详情/执行操作，Esc 返回列表。
//! 无 ScrollView——避免其内置 handler 与自定义 ↑/↓ 冲突。

mod data;
mod discover_state;
use self::discover_state as discover;
mod discover_handler;
mod identity;
mod operation;
mod panel_handler;
mod render;
mod search_handler;
mod search_request;

#[cfg(test)]
#[path = "plugin/guard_test.rs"]
mod guard_tests;

#[cfg(test)]
#[path = "plugin/hint_test.rs"]
mod hint_tests;

#[cfg(test)]
#[path = "plugin/search_request_test.rs"]
mod search_request_tests;

#[cfg(test)]
use crate::application::operations::OperationScope;
#[cfg(test)]
use crate::i18n;

use identity::{InstalledIdentity, MarketplaceIdentity};

mod plugin_panel;
pub use plugin_panel::*;

#[cfg(test)]
use crate::kit::atoms::ACTIVE_SESSION_ID;

#[cfg(test)]
use crate::kit::atoms::BRIDGE_RESET_COUNTER;

#[cfg(test)]
use crate::kit::atoms::PLUGIN_LIST;

use crate::kit::atoms::PluginSummary;

#[cfg(test)]
use crate::kit::atoms::PluginViewTab;
