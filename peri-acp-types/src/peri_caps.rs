use serde::{Deserialize, Serialize};
use serde_json::Value;

/// TUI 自定义消费能力声明。
///
/// TUI 在 `InitializeRequest.clientCapabilities._meta` 中以 `peri.xxx` keys 声明消费能力。
/// 每个 flag 默认为 false —— 其他 TUI 程序不需要 peri 自定义数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeriCaps {
    /// 控制 `UsageUpdate._meta.{inputTokens, outputTokens, cacheReadTokens, requestId, model, stopReason}`
    #[serde(default)]
    pub token_stats: bool,
    /// 控制 `AvailableCommandsUpdate._meta.skillNames`
    #[serde(default)]
    pub skill_names: bool,
    /// 控制 `ContentChunk._meta.periReplay` / `ToolCall._meta.periReplay` / `ToolCallUpdate._meta.periReplay`
    #[serde(default)]
    pub replay: bool,
    /// 控制 `params._peri.sourceAgentId`
    #[serde(default)]
    pub source_agent_id: bool,
    /// 控制 `peri/agent_event` 通道中 `AcpEvent::StateSnapshotMeta` 的发送
    #[serde(default)]
    pub context_usage: bool,
}

impl PeriCaps {
    /// 从 `clientCapabilities._meta` JSON map 解析。
    pub fn from_client_meta(meta: &serde_json::Map<String, Value>) -> Self {
        fn meta_bool(meta: &serde_json::Map<String, Value>, key: &str) -> bool {
            meta.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
        }
        Self {
            token_stats: meta_bool(meta, "peri.tokenStats"),
            skill_names: meta_bool(meta, "peri.skillNames"),
            replay: meta_bool(meta, "peri.replay"),
            source_agent_id: meta_bool(meta, "peri.sourceAgentId"),
            context_usage: meta_bool(meta, "peri.contextUsage"),
        }
    }

    /// 序列化到 `agentCapabilities._meta`（InitializeResponse 回显）。
    pub fn to_agent_meta(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("peri.tokenStats".into(), Value::Bool(self.token_stats));
        m.insert("peri.skillNames".into(), Value::Bool(self.skill_names));
        m.insert("peri.replay".into(), Value::Bool(self.replay));
        m.insert(
            "peri.sourceAgentId".into(),
            Value::Bool(self.source_agent_id),
        );
        m.insert("peri.contextUsage".into(), Value::Bool(self.context_usage));
        m
    }
}

#[cfg(test)]
#[path = "peri_caps_test.rs"]
mod tests;
