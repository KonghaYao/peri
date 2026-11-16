//! ratatui-kit ConfigPanel component.
//!
//! H1a（Iteration 14）：从 PERI_CONFIG_HANDLE 读取真实 PeriConfig，操作时
//! write + 调用 config::save_effective 持久化到当前生效层（全局或工作区，
//! 路径决策由 ConfigSource 加载时确定）。permission_mode
//! 经 session/set_mode 修改当前会话运行权限，不写宿主持久配置。
//!
//! 职责划分：本文件是组件外壳（hooks、事件注册、渲染窗口）；行模型与行显示在
//! [`rows`]，写入与保存边界在 [`actions`]。两边都以行模型的下标为身份。

mod actions;
mod rows;

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;

#[cfg(test)]
#[path = "config_betas_test.rs"]
mod beta_tests;

#[cfg(test)]
use crate::i18n;

mod config_panel;
pub use config_panel::*;

#[cfg(test)]
use self::actions::{activate_row, activate_row_with, toggle_beta_flag_with};
#[cfg(test)]
use self::rows::{
    CONFIG_ROWS, ConfigRow, PERMISSION_OPTS, ROW_1M_CONTEXT, ROW_CACHE_WARN, ROW_PERMISSION_MODE,
    beta_description, config_rows, parse_permission_mode, permission_mode_label, read_beta_toggle,
};
