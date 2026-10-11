use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Write/Edit 工具结果内联 diff 默认是否可见。
    #[serde(default)]
    pub diff_enabled: bool,
    /// 流式渲染模式：streaming / block / none。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_mode: Option<String>,
    /// 消息区滚动绘制帧率：60 | 30 | 20。None 使用默认 20fps。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_fps: Option<u32>,
    /// 主题名称或用户自定义主题名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// 是否启用每日色彩自动切换。
    #[serde(default)]
    pub daily_color: bool,
    /// 上次执行每日色彩切换的日期，格式为 YYYY-MM-DD。
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
    pub fn from_extra(extra: &Map<String, Value>) -> Self {
        Self {
            diff_enabled: extra
                .get("diff_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            streaming_mode: extra
                .get("streaming_mode")
                .and_then(Value::as_str)
                .map(String::from),
            scroll_fps: extra
                .get("scroll_fps")
                .and_then(Value::as_u64)
                .map(|value| value as u32),
            theme: extra.get("theme").and_then(Value::as_str).map(String::from),
            daily_color: extra
                .get("daily_color")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            daily_color_date: extra
                .get("daily_color_date")
                .and_then(Value::as_str)
                .map(String::from),
            extra: Map::new(),
        }
    }

    pub fn sync_to_extra(&self, extra: &mut Map<String, Value>) {
        extra.insert("diff_enabled".into(), Value::Bool(self.diff_enabled));
        set_optional(
            extra,
            "streaming_mode",
            self.streaming_mode
                .as_ref()
                .map(|value| Value::String(value.clone())),
        );
        set_optional(extra, "scroll_fps", self.scroll_fps.map(Value::from));
        set_optional(
            extra,
            "theme",
            self.theme
                .as_ref()
                .map(|value| Value::String(value.clone())),
        );
        extra.insert("daily_color".into(), Value::Bool(self.daily_color));
        set_optional(
            extra,
            "daily_color_date",
            self.daily_color_date
                .as_ref()
                .map(|value| Value::String(value.clone())),
        );
    }
}

fn set_optional(extra: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        extra.insert(key.to_string(), value);
    } else {
        extra.remove(key);
    }
}

#[cfg(test)]
#[path = "ui_test.rs"]
mod tests;
