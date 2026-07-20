//! TUI 渲染配置——从 AppConfig.extra 提取的 UI 专用字段
//!
//! 与 AppConfig 独立管理，通过 TUI_CONFIG_HANDLE 原子句柄共享。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Write/Edit 工具结果内联 diff 默认是否可见
    #[serde(default)]
    pub diff_enabled: bool,
    /// 流式渲染模式：streaming / block / none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_mode: Option<String>,
    /// 消息区滚动绘制帧率：60 | 30 | 20。None=默认 20fps（50ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_fps: Option<u32>,
    /// 主题名称（"peri-dark" | "peri-light" | 用户自定义主题名）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// 是否启用每日色彩自动切换（同 mode 内轮换）
    #[serde(default)]
    pub daily_color: bool,
    /// 上次执行每日色彩切换的日期（"YYYY-MM-DD"），用于启动时判断是否需要切换
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_color_date: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            diff_enabled: false,
            streaming_mode: None,
            scroll_fps: None,
            theme: None,
            daily_color: false,
            daily_color_date: None,
            extra: Map::new(),
        }
    }
}

impl TuiConfig {
    /// 从 PeriConfig.config.extra 提取旧 TUI 键（向后兼容启动迁移）
    pub fn from_extra(extra: &Map<String, Value>) -> Self {
        Self {
            diff_enabled: extra
                .get("diff_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            streaming_mode: extra
                .get("streaming_mode")
                .and_then(|v| v.as_str())
                .map(String::from),
            scroll_fps: extra
                .get("scroll_fps")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            theme: extra
                .get("theme")
                .and_then(|v| v.as_str())
                .map(String::from),
            daily_color: extra
                .get("daily_color")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            daily_color_date: extra
                .get("daily_color_date")
                .and_then(|v| v.as_str())
                .map(String::from),
            extra: Default::default(),
        }
    }

    /// 将 TuiConfig 同步到 PeriConfig.config.extra（用于持久化）。
    /// H1 fix: bool 字段始终写入（匹配当前 settings.json 行为）。
    pub fn sync_to_extra(&self, extra: &mut Map<String, Value>) {
        extra.insert("diff_enabled".into(), Value::Bool(self.diff_enabled));
        match &self.streaming_mode {
            Some(mode) => {
                extra.insert("streaming_mode".into(), Value::String(mode.clone()));
            }
            None => {
                extra.remove("streaming_mode");
            }
        }
        match self.scroll_fps {
            Some(fps) => {
                extra.insert("scroll_fps".into(), Value::Number(fps.into()));
            }
            None => {
                extra.remove("scroll_fps");
            }
        }
        match &self.theme {
            Some(theme) => {
                extra.insert("theme".into(), Value::String(theme.clone()));
            }
            None => {
                extra.remove("theme");
            }
        }
        extra.insert("daily_color".into(), Value::Bool(self.daily_color));
        match &self.daily_color_date {
            Some(date) => {
                extra.insert("daily_color_date".into(), Value::String(date.clone()));
            }
            None => {
                extra.remove("daily_color_date");
            }
        }
    }
}
