//! ratatui-kit ThemePanel component.
//!
//! 列出可用主题（builtin + ~/.peri/themes/），顶部提供选中主题的 markdown
//! 预览，Enter 切换主题，Esc 关闭。布局仿 ThreadBrowser 面板。
//!
//! 交互模式：
//! - 打开面板 → 切到持久化主题色，记住原始主题（Esc 恢复用）
//! - j/k/↑/↓ 导航 → 实时切换全局颜色（实时预览）
//! - Enter → 持久化到 ~/.peri/settings.json
//! - Esc → 恢复原始主题色并关闭面板

mod download;

#[cfg(test)]
#[path = "theme_test.rs"]
mod theme_test;

#[cfg(test)]
#[path = "theme_download_test.rs"]
mod theme_download_test;

#[cfg(test)]
use download::{DownloadRequest, trigger_download_themes_with};

mod theme_panel;
pub use theme_panel::*;
