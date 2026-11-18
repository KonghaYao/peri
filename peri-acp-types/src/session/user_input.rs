//! 用户输入队列的 ACP 契约；执行与生命周期归 Agent mailbox。

use crate::messages::MessageContent;
use serde::{Deserialize, Serialize};

/// 用户在提交时选择的插件身份。
///
/// 只携带插件 ID：指引、URL、凭证与 Loader 名称由 Host 依据当前会话元数据生成，
/// 客户端不能借此扩大权限或替换绑定。wire 形态只接受对象
/// `{"pluginId":"alpha"}`；Serde 默认的位置形式会被拒绝。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSelection {
    pub plugin_id: String,
}

impl<'de> Deserialize<'de> for PluginSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SelectionVisitor;

        impl<'de> serde::de::Visitor<'de> for SelectionVisitor {
            type Value = PluginSelection;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a plugin selection object with pluginId")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut plugin_id = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key != "pluginId" {
                        return Err(serde::de::Error::unknown_field(&key, &["pluginId"]));
                    }
                    if plugin_id.is_some() {
                        return Err(serde::de::Error::duplicate_field("pluginId"));
                    }
                    plugin_id = Some(map.next_value()?);
                }
                plugin_id
                    .map(|plugin_id| PluginSelection { plugin_id })
                    .ok_or_else(|| serde::de::Error::missing_field("pluginId"))
            }
        }

        // `deserialize_map` 明确拒绝数组等非对象形状；默认 struct 路径会接受位置形式。
        deserializer.deserialize_map(SelectionVisitor)
    }
}

impl PluginSelection {
    #[must_use]
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInput {
    /// 与最终 Human 消息共用的稳定 UUID。
    pub input_id: String,
    pub content: MessageContent,
    /// 未经 trim 或输入准备改写的编辑器原文。
    pub original_draft: String,
    /// 随这条输入一起提交的插件选择；撤回重发必须保持同一集合。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_selections: Vec<PluginSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputState {
    Queued,
    Dispatching,
    Claimed,
    Delivered,
    Withdrawn,
    /// 请求的输入身份不在本会话队列记录中。
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueItem {
    pub input_id: String,
    pub content: MessageContent,
    pub original_draft: String,
    /// 与该输入绑定的插件选择；快照客户端据此恢复标签。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_selections: Vec<PluginSelection>,
    pub state: UserInputState,
}

/// 已终结输入的轻量记录；客户端用它补齐丢失的 `UserInputDelivered` 或撤回确认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueTerminalItem {
    pub input_id: String,
    pub state: UserInputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueSnapshot {
    pub session_id: String,
    /// 当前进程内队列身份；重建实例后更换。
    pub generation: String,
    pub revision: u64,
    /// 已绑定真实执行的队列 run；仅预留、尚未启动时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_request_id: Option<String>,
    pub items: Vec<UserInputQueueItem>,
    /// 有界保留的终态记录（Delivered/Withdrawn）；用于客户端恢复丢包确认。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_items: Vec<UserInputQueueTerminalItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnqueueUserInputRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    pub input_id: String,
    pub content: MessageContent,
    pub original_draft: String,
    /// 插件选择随 input_id 走，不读取 Host 的“当前全局选择”。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_selections: Vec<PluginSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchUserInputsRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    /// 点击时的明确集合；重试不得重新从队列中全选。
    pub input_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBackUserInputRequest {
    pub session_id: String,
    pub generation: String,
    pub command_id: String,
    pub input_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputQueueSnapshotRequest {
    pub session_id: String,
    #[serde(default)]
    pub generation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputItemResult {
    pub input_id: String,
    pub state: UserInputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQueueReceipt {
    pub snapshot: UserInputQueueSnapshot,
    /// 当前队列命令的逐条状态裁决。
    pub results: Vec<UserInputItemResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_back: Option<UserInput>,
}

#[cfg(test)]
#[path = "user_input_test.rs"]
mod tests;
