use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// 生命周期事件
///
/// 对齐 Claude Code hooks.json 中的 key 名（PascalCase）。
/// `Unknown` 变体用于兼容 settings.local.json 中尚未实现的事件。
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    UserPromptSubmit,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    /// 一批并行工具调用全部完成时触发（每 batch 一次）
    PostToolBatch,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    /// Agent 等待用户输入时触发（PermissionRequest / Stop 后）
    Notification,
    /// settings.local.json 中尚未实现的事件（如 Setup 等）
    Unknown(String),
}

impl Serialize for HookEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            HookEvent::PreToolUse => serializer.serialize_str("PreToolUse"),
            HookEvent::PostToolUse => serializer.serialize_str("PostToolUse"),
            HookEvent::PostToolUseFailure => serializer.serialize_str("PostToolUseFailure"),
            HookEvent::PermissionRequest => serializer.serialize_str("PermissionRequest"),
            HookEvent::UserPromptSubmit => serializer.serialize_str("UserPromptSubmit"),
            HookEvent::SessionStart => serializer.serialize_str("SessionStart"),
            HookEvent::SessionEnd => serializer.serialize_str("SessionEnd"),
            HookEvent::Stop => serializer.serialize_str("Stop"),
            HookEvent::StopFailure => serializer.serialize_str("StopFailure"),
            HookEvent::PostToolBatch => serializer.serialize_str("PostToolBatch"),
            HookEvent::SubagentStart => serializer.serialize_str("SubagentStart"),
            HookEvent::SubagentStop => serializer.serialize_str("SubagentStop"),
            HookEvent::PreCompact => serializer.serialize_str("PreCompact"),
            HookEvent::PostCompact => serializer.serialize_str("PostCompact"),
            HookEvent::Notification => serializer.serialize_str("Notification"),
            HookEvent::Unknown(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for HookEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "PreToolUse" => HookEvent::PreToolUse,
            "PostToolUse" => HookEvent::PostToolUse,
            "PostToolUseFailure" => HookEvent::PostToolUseFailure,
            "PermissionRequest" => HookEvent::PermissionRequest,
            "UserPromptSubmit" => HookEvent::UserPromptSubmit,
            "SessionStart" => HookEvent::SessionStart,
            "SessionEnd" => HookEvent::SessionEnd,
            "Stop" => HookEvent::Stop,
            "StopFailure" => HookEvent::StopFailure,
            "PostToolBatch" => HookEvent::PostToolBatch,
            "SubagentStart" => HookEvent::SubagentStart,
            "SubagentStop" => HookEvent::SubagentStop,
            "PreCompact" => HookEvent::PreCompact,
            "PostCompact" => HookEvent::PostCompact,
            "Notification" => HookEvent::Notification,
            other => HookEvent::Unknown(other.to_string()),
        })
    }
}

/// 4 种 hook 执行类型，对齐 Claude Code schemas/hooks.ts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HookType {
    /// Shell 命令执行 (bash/powershell)
    Command {
        command: String,
        #[serde(default)]
        shell: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
        #[serde(rename = "async", default)]
        async_run: bool,
        #[serde(rename = "asyncRewake", default)]
        async_rewake: bool,
        /// 粗粒度匹配器（字符串/正则），见"matcher vs if"章节
        #[serde(default)]
        matcher: Option<String>,
        /// 细粒度条件匹配（permission rule 语法），见"matcher vs if"章节
        #[serde(rename = "if", default)]
        condition: Option<String>,
    },
    /// LLM 提示词评估
    Prompt {
        prompt: String,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
        #[serde(default)]
        matcher: Option<String>,
        #[serde(rename = "if", default)]
        condition: Option<String>,
    },
    /// HTTP POST
    Http {
        url: String,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default)]
        allowed_env_vars: Vec<String>,
        #[serde(default)]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
        #[serde(default)]
        matcher: Option<String>,
        #[serde(rename = "if", default)]
        condition: Option<String>,
    },
    /// 子 Agent 执行（完整 agent 循环，最多 50 轮）
    Agent {
        prompt: String,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        status_message: Option<String>,
        #[serde(default)]
        once: bool,
        #[serde(default)]
        matcher: Option<String>,
        #[serde(rename = "if", default)]
        condition: Option<String>,
    },
}

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

/// hooks.json 中单个 hook 规则组
///
/// 对齐 Claude Code hooks schema：
/// - matcher: 粗粒度匹配器（工具名/正则），在进程启动前过滤
/// - hooks: 该规则组下的所有 hook 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatchRule {
    /// 粗粒度匹配器（见"matcher vs if"章节）
    #[serde(default)]
    pub matcher: Option<String>,
    pub hooks: Vec<HookType>,
}

/// 插件的完整 hooks 配置
pub type HooksConfig = HashMap<HookEvent, Vec<HookMatchRule>>;

/// 已注册到 HookMiddleware 的 hook（带插件上下文）
#[derive(Debug, Clone)]
pub struct RegisteredHook {
    pub hook: HookType,
    pub event: HookEvent,
    /// 粗粒度匹配器（来自 HookMatchRule.matcher 或 HookType 内 matcher 字段）
    pub matcher: Option<String>,
    pub plugin_name: String,
    pub plugin_id: String,
    pub plugin_root: PathBuf,
    pub plugin_data_dir: PathBuf,
    /// 插件选项（userConfig 值，用于 CLAUDE_PLUGIN_OPTION_* 环境变量）
    pub plugin_options: HashMap<String, serde_json::Value>,
}

// === HookType getter 辅助方法 ===

impl HookType {
    /// 返回各变体的 matcher 字段
    pub fn get_matcher(&self) -> Option<&String> {
        match self {
            HookType::Command { matcher, .. } => matcher.as_ref(),
            HookType::Prompt { matcher, .. } => matcher.as_ref(),
            HookType::Http { matcher, .. } => matcher.as_ref(),
            HookType::Agent { matcher, .. } => matcher.as_ref(),
        }
    }

    /// 返回各变体的 condition 字段
    pub fn get_condition(&self) -> Option<&String> {
        match self {
            HookType::Command { condition, .. } => condition.as_ref(),
            HookType::Prompt { condition, .. } => condition.as_ref(),
            HookType::Http { condition, .. } => condition.as_ref(),
            HookType::Agent { condition, .. } => condition.as_ref(),
        }
    }

    /// 返回 once 标志，Command 有 once 字段，其他类型默认 false
    pub fn is_once(&self) -> bool {
        match self {
            HookType::Command { once, .. } => *once,
            HookType::Prompt { once, .. } => *once,
            HookType::Http { once, .. } => *once,
            HookType::Agent { once, .. } => *once,
        }
    }

    /// 返回 async 标志，仅 Command 有 async_run 字段，其他类型默认 false
    pub fn is_async(&self) -> bool {
        match self {
            HookType::Command { async_run, .. } => *async_run,
            HookType::Prompt { .. } => false,
            HookType::Http { .. } => false,
            HookType::Agent { .. } => false,
        }
    }

    /// 返回 statusMessage 字段——hook 执行期间展示给用户的状态提示
    pub fn get_status_message(&self) -> Option<&String> {
        match self {
            HookType::Command { status_message, .. } => status_message.as_ref(),
            HookType::Prompt { status_message, .. } => status_message.as_ref(),
            HookType::Http { status_message, .. } => status_message.as_ref(),
            HookType::Agent { status_message, .. } => status_message.as_ref(),
        }
    }
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
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
