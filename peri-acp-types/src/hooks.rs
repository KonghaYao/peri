//! Hook 契约（事件 / 执行类型 / 规则组 / 已注册 hook）。
//!
//! 自 `peri-middlewares/src/hooks/types.rs` 迁入（3.0 批 2 波 1：协议类型
//! 归契约层；middlewares 保留 re-export 保兼容）。执行器（dispatcher /
//! executor / fire_*）留在 middlewares。

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

    // === P1-5: 新增 13 个 Claude Code hook 事件 ===
    /// 项目初始化/维护 hooks
    Setup,
    /// 实验性 agent teams（CC 实验性功能，peri 暂无 teams 系统）
    TeammateIdle,
    /// 任务创建通知
    TaskCreated,
    /// 任务完成通知
    TaskCompleted,
    /// 配置变化检测
    ConfigChange,
    /// Git worktree 创建
    WorktreeCreate,
    /// Git worktree 移除
    WorktreeRemove,
    /// 规则/指令加载
    InstructionsLoaded,
    /// MCP 交互请求
    Elicitation,
    /// MCP 交互结果
    ElicitationResult,
    /// 工作目录变更
    CwdChanged,
    /// 文件监控变更（依赖文件监控基础设施，暂无触发点）
    FileChanged,
    /// 权限拒绝后重试
    PermissionDenied,

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
            // === P1-5 新增事件序列化 ===
            HookEvent::Setup => serializer.serialize_str("Setup"),
            HookEvent::TeammateIdle => serializer.serialize_str("TeammateIdle"),
            HookEvent::TaskCreated => serializer.serialize_str("TaskCreated"),
            HookEvent::TaskCompleted => serializer.serialize_str("TaskCompleted"),
            HookEvent::ConfigChange => serializer.serialize_str("ConfigChange"),
            HookEvent::WorktreeCreate => serializer.serialize_str("WorktreeCreate"),
            HookEvent::WorktreeRemove => serializer.serialize_str("WorktreeRemove"),
            HookEvent::InstructionsLoaded => serializer.serialize_str("InstructionsLoaded"),
            HookEvent::Elicitation => serializer.serialize_str("Elicitation"),
            HookEvent::ElicitationResult => serializer.serialize_str("ElicitationResult"),
            HookEvent::CwdChanged => serializer.serialize_str("CwdChanged"),
            HookEvent::FileChanged => serializer.serialize_str("FileChanged"),
            HookEvent::PermissionDenied => serializer.serialize_str("PermissionDenied"),
            HookEvent::Unknown(s) => serializer.serialize_str(s),
        }
    }
}

impl HookEvent {
    /// 宽松解析 hook 事件名，仅返回已知事件。
    /// 未知事件返回 `None`（应在调用侧跳过并记录 warn 日志）。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
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
            // === P1-5 新增事件解析 ===
            "Setup" => HookEvent::Setup,
            "TeammateIdle" => HookEvent::TeammateIdle,
            "TaskCreated" => HookEvent::TaskCreated,
            "TaskCompleted" => HookEvent::TaskCompleted,
            "ConfigChange" => HookEvent::ConfigChange,
            "WorktreeCreate" => HookEvent::WorktreeCreate,
            "WorktreeRemove" => HookEvent::WorktreeRemove,
            "InstructionsLoaded" => HookEvent::InstructionsLoaded,
            "Elicitation" => HookEvent::Elicitation,
            "ElicitationResult" => HookEvent::ElicitationResult,
            "CwdChanged" => HookEvent::CwdChanged,
            "FileChanged" => HookEvent::FileChanged,
            "PermissionDenied" => HookEvent::PermissionDenied,
            _ => return None,
        })
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
            // === P1-5 新增事件反序列化 ===
            "Setup" => HookEvent::Setup,
            "TeammateIdle" => HookEvent::TeammateIdle,
            "TaskCreated" => HookEvent::TaskCreated,
            "TaskCompleted" => HookEvent::TaskCompleted,
            "ConfigChange" => HookEvent::ConfigChange,
            "WorktreeCreate" => HookEvent::WorktreeCreate,
            "WorktreeRemove" => HookEvent::WorktreeRemove,
            "InstructionsLoaded" => HookEvent::InstructionsLoaded,
            "Elicitation" => HookEvent::Elicitation,
            "ElicitationResult" => HookEvent::ElicitationResult,
            "CwdChanged" => HookEvent::CwdChanged,
            "FileChanged" => HookEvent::FileChanged,
            "PermissionDenied" => HookEvent::PermissionDenied,
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

/// Settings hooks 加载端口（3.0 批 2 波 2 装配注入）。
///
/// global/project/local 三级 settings hooks 的磁盘加载留在
/// `peri-middlewares`（hooks/loader）；ACP 装配面（`host/assemble.rs`）只做
/// 组序组合（plugin → global → project → local，ARC-MIDDLEWARE-001 不重排）。
/// 宿主装配点构造实现后注入。
pub trait SettingsHooksPort: Send + Sync {
    /// `~/.claude/settings.json` 级 hooks。
    fn global(&self) -> Vec<RegisteredHook>;
    /// `{cwd}/.claude/settings.json` 级 hooks。
    fn project(&self, cwd: &str) -> Vec<RegisteredHook>;
    /// `{cwd}/.claude/settings.local.json` 级 hooks。
    fn local(&self, cwd: &str) -> Vec<RegisteredHook>;
}
