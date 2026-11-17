//! ratatui-kit ModelPanel component.
//!
//! 左右分栏 Profile 编辑器：
//! - 左侧：4 个固定档位卡片；选择可用档位时同时激活，未完成档位只选中并提示；
//! - 右侧：选中 profile 的 K/V 编辑行（Provider / Model / Effort / Max tokens / Context window / 1m enable）。
//!
//! 右侧 `→`/`←` 切换字段值并立即写入内存 + 持久化 + 推送 ACP（无 Enter/Save 步骤）。
//! 所有 provider（含 Responses / ChatGPT plan）共用同一套行与动作，没有按 provider
//! 类型分叉的编辑面板或字段。右栏 K/V 行点击 = 聚焦并执行与键盘相同的动作；底部
//! “管理 Provider”按钮与 `Ctrl+L` 等价：经 panel_registry 打开 Login 管理当前 provider。

mod edit;
mod editor_view;
mod panel_input;
mod panel_render;

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;

#[cfg(test)]
use crate::i18n;

mod model_panel;
pub use model_panel::*;

pub(crate) use edit::switch_active_alias;

#[cfg(test)]
use crate::kit::atoms::SERVICE_SNAPSHOT;

#[cfg(test)]
use self::model_panel::effective_context_window;
#[cfg(test)]
use self::panel_input::active_profile_idx;
