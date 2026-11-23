//! 监控 WebSocket 的线协议。写消息绑定到发送时的活动页和租约代次。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::input::{KeyEventKind, MouseButton, MouseEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    Driving,
    Idle,
    WaitingForHuman,
}

impl AgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Driving => "driving",
            Self::Idle => "idle",
            Self::WaitingForHuman => "waiting_for_human",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ViewStamp {
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub page_id: Option<String>,
    pub view_epoch: String,
}

#[derive(Serialize)]
pub(crate) struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Command {
    pub id: u64,
    pub view: ViewStamp,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub(crate) enum Action {
    Switch {
        page_id: String,
    },
    NewTab {
        url: String,
    },
    CloseTab {
        page_id: String,
    },
    Takeover,
    Release,
    Devtools,
    Answer {
        request_id: String,
        accepted: bool,
    },
    Done {
        request_id: String,
    },
    Mouse {
        #[serde(rename = "type")]
        event: MouseEventKind,
        x: f64,
        y: f64,
        button: Option<MouseButton>,
        click_count: Option<u32>,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Key {
        #[serde(rename = "type")]
        event: KeyEventKind,
        key: String,
        code: String,
        text: Option<String>,
        #[serde(default)]
        modifiers: u32,
    },
    Text {
        text: String,
    },
    Navigate {
        url: String,
    },
    History {
        delta: i64,
    },
}

impl Action {
    pub fn needs_human(&self) -> bool {
        matches!(
            self,
            Self::Release
                | Self::Done { .. }
                | Self::CloseTab { .. }
                | Self::Mouse { .. }
                | Self::Wheel { .. }
                | Self::Key { .. }
                | Self::Text { .. }
                | Self::Navigate { .. }
                | Self::History { .. }
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub(crate) enum Signal {
    Visible { visible: bool },
    Ack,
}

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub(crate) enum ServerMessage<'a> {
    Result {
        id: u64,
        ok: bool,
        value: Value,
        code: Option<&'static str>,
    },
    Meta {
        #[serde(flatten)]
        stamp: &'a ViewStamp,
        viewport_width: u32,
        viewport_height: u32,
        device_width: u32,
        device_height: u32,
        page_scale_factor: f64,
        scroll_offset_x: f64,
        scroll_offset_y: f64,
    },
    Thumb {
        page_id: &'a str,
        data: &'a str,
    },
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
