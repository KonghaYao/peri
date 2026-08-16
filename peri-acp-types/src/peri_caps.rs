use crate::command::command_route::UiCommandSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// TUI 自定义消费能力声明。
///
/// TUI 在 `InitializeRequest.clientCapabilities._meta` 中以 `peri.xxx` keys 声明消费能力。
/// 每个 flag 默认为 false —— 其他 TUI 程序不需要 peri 自定义数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
    /// `peri.uiCommands`：TUI 上送的 ui 域命令明细（空 = 不广播 ui 条目）。
    /// 门控语义反转：TUI 声明明细 → ACP 注册为 `ui:*` 条目，而非 ACP 附加
    /// 硬编码列表；旧客户端 bool `true` 由 [`PeriCaps::from_client_meta`]
    /// 退化为 [`default_ui_commands`] 明细（注册冲突按注册表裁决，见该函数
    /// 注释）。
    #[serde(default)]
    pub ui_commands: Vec<UiCommandSpec>,
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
            ui_commands: Self::meta_ui_commands(meta),
        }
    }

    /// `peri.uiCommands` 兼容两态解析：
    /// - 数组 → `Vec<UiCommandSpec>`（serde 解析失败按空处理 + warn）；
    /// - 旧客户端 bool `true` → 退化为 [`default_ui_commands`] 明细（明细与旧
    ///   ACP 硬编码列表一致；注册时同名冲突按注册表裁决，见该函数注释）；
    /// - 未声明 / `false` / 其他类型 → 空（外部客户端默认不接收界面性命令）。
    fn meta_ui_commands(meta: &serde_json::Map<String, Value>) -> Vec<UiCommandSpec> {
        match meta.get("peri.uiCommands") {
            None | Some(Value::Bool(false)) => Vec::new(),
            Some(Value::Bool(true)) => default_ui_commands(),
            Some(Value::Array(_)) => serde_json::from_value(meta["peri.uiCommands"].clone())
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "peri.uiCommands 数组解析失败，按空处理");
                    Vec::new()
                }),
            Some(other) => {
                tracing::warn!(
                    value = %other,
                    "peri.uiCommands 类型不受支持（期望数组或 bool），按空处理"
                );
                Vec::new()
            }
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
        m.insert(
            "peri.uiCommands".into(),
            serde_json::to_value(&self.ui_commands).expect("Vec<UiCommandSpec> 序列化不应失败"),
        );
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
            ui_commands: default_ui_commands(),
        }
    }
}

/// 默认 ui 域命令明细（11 条，旧客户端 bool `true` 退化与 [`PeriCaps::all_enabled`]
/// 内部路径（MpscTransport，TUI 默认全 cap）填写的兜底集合，数据迁移自
/// `peri-acp/src/dispatch/commands.rs` 的 `UI_COMMANDS` 列表）。Phase 4 TUI
/// 显式上送明细后由 TUI 明细替换。
///
/// 注意：明细注册进注册表时按「第一等级裸名跨域互斥」裁决（设计 §63/§64，
/// 纯拒绝不覆盖）——`ui:clear` 与内置 `core:clear` 同名冲突被拒，实际广播
/// 10 条 ui 条目（clear 职能由 `core:clear` 承担）；将来 core 域新增同名内置
/// （如 `core:help`）会静默丢对应 ui 条目。Phase 4 TUI 显式上送明细时需评估
/// 同名冲突策略（ui 面板名与 core 内置同名是常态，可能需要冲突裁决优先序
/// 或 TUI 侧改名）。
pub(crate) fn default_ui_commands() -> Vec<UiCommandSpec> {
    vec![
        UiCommandSpec {
            name: "help".into(),
            description: "Show available commands and their descriptions".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "clear".into(),
            description: "Clear the current conversation".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "context".into(),
            description: "Display context usage / token statistics".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "cost".into(),
            description: "Show token usage and estimated cost".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "mode".into(),
            description: "Switch the current permission mode".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "effort".into(),
            description: "Configure LLM reasoning/thinking effort".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "history".into(),
            description: "View and resume previous conversations".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "agents".into(),
            description: "Manage sub-agent definitions".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "rename".into(),
            description: "Rename the current session".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "lang".into(),
            description: "Switch display language / locale".into(),
            ..Default::default()
        },
        UiCommandSpec {
            name: "exit".into(),
            description: "Exit the application".into(),
            ..Default::default()
        },
    ]
}

#[cfg(test)]
#[path = "peri_caps_test.rs"]
mod tests;
