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
    /// 控制 `peri/agent_event` 通知通道的发送（Category ③ 全部）
    #[serde(default)]
    pub agent_event: bool,
    /// 控制 `peri/agent_event_done`（TurnDone）通知的发送
    #[serde(default)]
    pub agent_event_done: bool,
    /// 控制 `peri/unstable-event` 通知通道的发送（Category ⑤ 全部）
    #[serde(default)]
    pub unstable_event: bool,
    /// 控制 `peri/prediction_ready` 预测输入的发送
    #[serde(default)]
    pub prediction: bool,
    /// 控制 `peri/hitl_pending` HITL 审批通知的发送
    #[serde(default)]
    pub hitl_pending: bool,
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
            agent_event: meta_bool(meta, "peri.agentEvent"),
            agent_event_done: meta_bool(meta, "peri.agentEventDone"),
            unstable_event: meta_bool(meta, "peri.unstableEvent"),
            prediction: meta_bool(meta, "peri.prediction"),
            hitl_pending: meta_bool(meta, "peri.hitlPending"),
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
        m.insert("peri.agentEvent".into(), Value::Bool(self.agent_event));
        m.insert(
            "peri.agentEventDone".into(),
            Value::Bool(self.agent_event_done),
        );
        m.insert(
            "peri.unstableEvent".into(),
            Value::Bool(self.unstable_event),
        );
        m.insert("peri.prediction".into(), Value::Bool(self.prediction));
        m.insert("peri.hitlPending".into(), Value::Bool(self.hitl_pending));
        m
    }

    /// 返回全部 cap 启用的实例。
    /// 用于 MpscTransport 内部路径（TUI 默认想接收所有自定义事件）。
    pub fn all_enabled() -> Self {
        Self {
            token_stats: true,
            skill_names: true,
            replay: true,
            source_agent_id: true,
            context_usage: true,
            agent_event: true,
            agent_event_done: true,
            unstable_event: true,
            prediction: true,
            hitl_pending: true,
        }
    }
}

#[cfg(test)]
#[path = "peri_caps_test.rs"]
mod tests;
