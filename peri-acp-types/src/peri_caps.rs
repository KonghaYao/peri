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
    /// 控制 `peri/agent_event` 通知通道的发送（Category ③ 全部）
    #[serde(default)]
    pub agent_event: bool,
    /// 控制 `peri/agent_event_done`（TurnDone）通知的发送
    #[serde(default)]
    pub agent_event_done: bool,
    /// 控制 `peri/agent_activity` 隐私安全活动摘要通知。
    ///
    /// 与 TUI legacy `agent_event` 不同，本通道禁止携带消息、工具正文、
    /// 错误文本、路径或 URL，供 Hub/GUI 做持久活动投影。
    #[serde(default)]
    pub agent_activity: bool,
    /// 控制专用 `peri/oauth` MCP OAuth 生命周期通知。
    ///
    /// 该能力与 legacy `peri/agent_event` 独立：只允许版本化、有界 DTO，
    /// authorization URL 仅在需要用户交互的瞬时通知中出现，raw error 与
    /// callback code/state 永不进入该通道。
    #[serde(default)]
    pub oauth: bool,
    /// 控制 `peri/unstable-event` 通知通道的发送（Category ⑤ 全部）
    #[serde(default)]
    pub unstable_event: bool,
    /// 控制 `peri/prediction_ready` 预测输入的发送
    #[serde(default)]
    pub prediction: bool,
    /// 控制标准 ACP Plan 条目 `_meta.activeForm` 的附加。
    ///
    /// 未协商时仍发送标准 Plan content/status；只有显式声明的
    /// Peri 客户端才会收到经边界限制的“当前进行中”文案。
    #[serde(default)]
    pub plan_entry_active_form: bool,
    /// 控制 Peri 会话回退 RPC（candidates / preview / execute）。
    ///
    /// 回退同时修改 transcript 并可选恢复文件，因此默认必须为 false；
    /// 客户端未声明时三个非标准 RPC 都必须 fail closed。
    #[serde(default)]
    pub rewind: bool,
    /// 控制 `AvailableCommandsUpdate.availableCommands` 中界面性命令条目的广播
    /// （help / clear / mode / lang / exit / history 等，由 TUI 本地处理）。
    /// TUI（全 cap / mpsc 内部路径）声明后广播，外部客户端不声明则不收到。
    #[serde(default)]
    pub ui_commands: bool,
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
            agent_event: meta_bool(meta, "peri.agentEvent"),
            agent_event_done: meta_bool(meta, "peri.agentEventDone"),
            agent_activity: meta_bool(meta, "peri.agentActivity"),
            oauth: meta_bool(meta, "peri.oauth"),
            unstable_event: meta_bool(meta, "peri.unstableEvent"),
            prediction: meta_bool(meta, "peri.prediction"),
            plan_entry_active_form: meta_bool(meta, "peri.planEntryActiveForm"),
            rewind: meta_bool(meta, "peri.rewind"),
            ui_commands: meta_bool(meta, "peri.uiCommands"),
        }
    }

    /// 序列化到 `agentCapabilities._meta`（InitializeResponse 回显）。
    pub fn to_agent_meta(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("peri.tokenStats".into(), Value::Bool(self.token_stats));
        m.insert("peri.skillNames".into(), Value::Bool(self.skill_names));
        m.insert("peri.replay".into(), Value::Bool(self.replay));
        m.insert("peri.agentEvent".into(), Value::Bool(self.agent_event));
        m.insert(
            "peri.agentEventDone".into(),
            Value::Bool(self.agent_event_done),
        );
        m.insert(
            "peri.agentActivity".into(),
            Value::Bool(self.agent_activity),
        );
        m.insert("peri.oauth".into(), Value::Bool(self.oauth));
        m.insert(
            "peri.unstableEvent".into(),
            Value::Bool(self.unstable_event),
        );
        m.insert("peri.prediction".into(), Value::Bool(self.prediction));
        m.insert(
            "peri.planEntryActiveForm".into(),
            Value::Bool(self.plan_entry_active_form),
        );
        m.insert("peri.rewind".into(), Value::Bool(self.rewind));
        m.insert("peri.uiCommands".into(), Value::Bool(self.ui_commands));
        m
    }

    /// 返回全部 cap 启用的实例。
    /// 用于 MpscTransport 内部路径（TUI 默认想接收所有自定义事件）。
    pub fn all_enabled() -> Self {
        Self {
            token_stats: true,
            skill_names: true,
            replay: true,
            agent_event: true,
            agent_event_done: true,
            agent_activity: true,
            oauth: true,
            unstable_event: true,
            prediction: true,
            plan_entry_active_form: true,
            rewind: true,
            ui_commands: true,
        }
    }
}

#[cfg(test)]
#[path = "peri_caps_test.rs"]
mod tests;
