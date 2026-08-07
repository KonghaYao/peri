use serde::{Deserialize, Serialize};

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::hooks`）。
// `HookEvent` / `HookType` / `HookMatchRule` / `HooksConfig` / `RegisteredHook`
// 自本文件迁出；本模块保留 re-export 保兼容。
pub use peri_acp_types::hooks::{HookEvent, HookMatchRule, HookType, HooksConfig, RegisteredHook};

/// Hook 执行输入——通过 stdin JSON 传递给 command hook，或作为 HTTP body
///
/// 对齐 Claude Code coreSchemas.ts:
/// - BaseHookInputSchema: session_id, transcript_path, cwd, permission_mode, agent_id, agent_type
/// - 每个事件通过 hook_event_name 判别字段区分
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    // === BaseHookInputSchema 基础字段 ===
    /// 会话 ID
    pub session_id: String,
    /// 会话 transcript 文件路径
    pub transcript_path: String,
    /// 当前工作目录
    pub cwd: String,
    /// 当前权限模式（"yolo" / "hitl" 等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// 子 agent ID（仅子 agent 内触发时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Agent 类型（如 "general-purpose" / "code-reviewer"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,

    // === 事件判别字段 ===
    /// 事件名称（如 "PreToolUse"、"SessionStart"）
    pub hook_event_name: HookEvent,

    // === 工具事件字段（PreToolUse / PostToolUse / PostToolUseFailure / PermissionRequest）===
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<serde_json::Value>,

    // === UserPromptSubmit 事件字段 ===
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    // === SessionStart 事件字段 ===
    /// 来源：startup / resume / clear / compact
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 当前模型
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    // === Subagent 事件字段 ===
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_result: Option<String>,

    // === Compact 事件字段 ===
    /// 压缩前的消息数量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,

    // === 通用扩展字段（P1-5 新增）===
    /// 事件特定数据，避免 struct 字段膨胀。
    /// 不同事件通过此字段携带额外上下文（如 CwdChanged 的 old_cwd/new_cwd）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_data: Option<serde_json::Value>,
}

/// Hook 执行结果——对齐 Claude Code src/types/hooks.ts syncHookResponseSchema
///
/// Claude Code 的 hook 输出是扁平 JSON（非 enum），包含多个可选字段。
/// Peri 解析为结构体后转换为内部 Action 枚举。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncHookResponse {
    /// 是否继续（默认 true）。false 时阻止 agent 继续执行
    #[serde(default, rename = "continue")]
    pub continue_run: Option<bool>,
    /// 是否在 transcript 中隐藏 stdout（默认 false）
    #[serde(default)]
    pub suppress_output: Option<bool>,
    /// continue=false 时显示的停止原因
    #[serde(default, rename = "stopReason")]
    pub stop_reason: Option<String>,
    /// 权限决策：approve=允许, block=阻止
    #[serde(default)]
    pub decision: Option<HookDecision>,
    /// 决策原因说明
    #[serde(default)]
    pub reason: Option<String>,
    /// 系统警告消息（展示给用户）
    #[serde(default, rename = "systemMessage")]
    pub system_message: Option<String>,
    /// 事件特定输出
    #[serde(default)]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

/// 权限决策：approve=允许, block=阻止
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    Approve,
    Block,
}

/// 事件特定的 hook 输出——对齐 Claude Code hookSpecificOutput discriminated union
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hookEventName")]
pub enum HookSpecificOutput {
    #[serde(rename = "PreToolUse")]
    PreToolUse {
        /// 权限决策：ask / deny / allow / passthrough
        #[serde(default, rename = "permissionDecision")]
        permission_decision: Option<PermissionDecision>,
        #[serde(default, rename = "permissionDecisionReason")]
        permission_decision_reason: Option<String>,
        /// 修改后的工具输入（PreToolUse hook 改写参数）
        #[serde(default, rename = "updatedInput")]
        updated_input: Option<serde_json::Value>,
        /// 附加上下文信息
        #[serde(default, rename = "additionalContext")]
        additional_context: Option<String>,
    },
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit {
        #[serde(default, rename = "additionalContext")]
        additional_context: Option<String>,
    },
    #[serde(rename = "SessionStart")]
    SessionStart {
        #[serde(default, rename = "additionalContext")]
        additional_context: Option<String>,
        /// 追加的初始用户消息
        #[serde(default, rename = "initialUserMessage")]
        initial_user_message: Option<String>,
        /// 监视路径列表（用于 FileChanged 事件，Phase 2）
        #[serde(default, rename = "watchPaths")]
        watch_paths: Option<Vec<String>>,
    },
}

/// 权限决策枚举（用于 PreToolUse hook 的 permissionDecision）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision {
    Ask,
    Deny,
    Allow,
    Passthrough,
}

/// 内部处理后的 hook 动作
#[derive(Debug, Clone)]
pub enum HookAction {
    /// 允许继续（默认行为）
    Allow,
    /// 阻止操作（decision=block / exit code 2 / continue=false）
    Block { reason: String },
    /// 修改工具输入（PreToolUse hook 的 updatedInput）
    ModifyInput { new_input: serde_json::Value },
    /// 修改权限行为（permissionDecision）
    PermissionOverride {
        decision: PermissionDecision,
        reason: Option<String>,
    },
    /// 阻止 agent 继续执行（continue=false + stopReason）
    PreventContinuation { stop_reason: Option<String> },
    /// 向 agent 注入系统消息（systemMessage）
    SystemMessage { message: String },
    /// 追加上下文（additionalContext）
    AdditionalContext { context: String },
    /// SessionStart 追加初始消息
    InitialUserMessage { message: String },
}

// === HookInput 构造函数（按事件类型）===

impl HookInput {
    pub fn session_start(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        source: &str,
        model: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::SessionStart,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: Some(source.to_string()),
            model: Some(model.to_string()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
            additional_data: None,
        }
    }

    pub fn tool_call(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        permission_mode: &str,
        tool_name: &str,
        tool_input: &serde_json::Value,
        tool_use_id: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: Some(permission_mode.to_string()),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::PreToolUse,
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(tool_input.clone()),
            tool_use_id: Some(tool_use_id.to_string()),
            tool_output: None,
            prompt: None,
            source: None,
            model: None,
            subagent_name: None,
            subagent_result: None,
            message_count: None,
            additional_data: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_result(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        permission_mode: &str,
        tool_name: &str,
        tool_input: &serde_json::Value,
        tool_output: &serde_json::Value,
        is_error: bool,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: Some(permission_mode.to_string()),
            agent_id: None,
            agent_type: None,
            hook_event_name: if is_error {
                HookEvent::PostToolUseFailure
            } else {
                HookEvent::PostToolUse
            },
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(tool_input.clone()),
            tool_use_id: None,
            tool_output: Some(tool_output.clone()),
            prompt: None,
            source: None,
            model: None,
            subagent_name: None,
            subagent_result: None,
            message_count: None,
            additional_data: None,
        }
    }

    pub fn user_prompt_submit(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        prompt: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::UserPromptSubmit,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: Some(prompt.to_string()),
            source: None,
            model: None,
            subagent_name: None,
            subagent_result: None,
            message_count: None,
            additional_data: None,
        }
    }

    pub fn subagent_start(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        subagent_name: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::SubagentStart,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: None,
            subagent_name: Some(subagent_name.to_string()),
            subagent_result: None,
            message_count: None,
            additional_data: None,
        }
    }

    pub fn subagent_stop(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        subagent_name: &str,
        result: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::SubagentStop,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: None,
            subagent_name: Some(subagent_name.to_string()),
            subagent_result: Some(result.to_string()),
            message_count: None,
            additional_data: None,
        }
    }

    pub fn compact(
        session_id: &str,
        transcript_path: &str,
        cwd: &str,
        event: HookEvent,
        message_count: usize,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: event,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: None,
            subagent_name: None,
            subagent_result: None,
            message_count: Some(message_count),
            additional_data: None,
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
