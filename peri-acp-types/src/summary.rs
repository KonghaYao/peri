//! Summary DTOs returned by `session/query` (ACP standard method).
//!
//! Contains two categories of types:
//! 1. **Migrated DTOs** — originally defined in `peri-acp/src/event/dto.rs`,
//!    re-exported from there for backward compatibility.
//! 2. **Summary types** — new DTOs for panel data queries (skills, cron, MCP,
//!    plugins, hooks, models, providers, workflows, system status, resource usage).

use serde::{Deserialize, Serialize};

// ─── Migrated DTOs (verbatim from peri-acp/src/event/dto.rs) ─────────────────

/// Compact 完成后保留的文件信息（DTO）
///
/// 替代 `peri_agent::agent::events::CompactFileInfo`，TUI/IDE 消费方应使用本类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactFileInfoDto {
    /// 文件路径
    pub path: String,
    /// 文件行数
    pub lines: usize,
}

/// Workflow 进度更新载荷（DTO）
///
/// 替代 `peri_agent::agent::events::WorkflowProgressPayload`，
/// TUI/IDE 消费方应使用本类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowProgressDto {
    /// Run ID (UUID v7)
    pub run_id: String,
    /// Workflow 名称
    pub workflow_name: String,
    /// 事件类型（run_started / phase_started / phase_done / agent_started / agent_progress / agent_done / run_done）
    pub event_type: String,
    /// Agent ID（仅 agent_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    /// Phase 名称（仅 phase_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Agent 标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Agent 状态（started/progress/done/dead/skipped）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    /// Token 计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    /// 工具调用计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    /// Run 状态（仅 run_done 有值：completed/failed/cancelled）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    /// 人类可读消息（错误描述 / 进度描述）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Token 使用量（DTO，替代 `peri_agent::llm::types::TokenUsage`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsageDto {
    /// 总输入 token（含缓存 token）
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 写入缓存的 token 数（仅 Anthropic 有意义，OpenAI 始终 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// 从缓存读取的 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// API 提供商返回的请求 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// LLM 响应停止原因（DTO，替代 `peri_agent::llm::types::StopReason`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReasonDto {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other { value: String },
}

/// Todo 项状态（DTO，替代 `peri_middlewares::tools::todo::TodoStatus`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusDto {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Todo 项（DTO，替代 `peri_middlewares::tools::todo::TodoItem`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TodoItemDto {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    #[serde(default)]
    pub status: TodoStatusDto,
}

// ─── New Summary types (session/query panel data) ──────────────────────────

/// Skill 摘要（对应 CronPanel / Skills 面板查询）
///
/// 字段对齐 `peri_middlewares::skills::loader::SkillMetadata`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    /// skill 来源（user / project / plugin / builtin 等）
    pub source: String,
    pub disabled: bool,
}

/// Cron 任务列表摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSummary {
    pub tasks: Vec<CronTaskDto>,
}

/// 单个 Cron 任务 DTO
///
/// 字段对齐 `peri_middlewares::cron::CronTask`，`expression` 重命名为 `schedule`
/// 以匹配面板 UI 语义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTaskDto {
    pub id: String,
    /// 标准 5 段 cron 表达式
    pub schedule: String,
    /// 触发时提交的用户输入
    pub prompt: String,
    /// 下次触发时间（ISO 8601 字符串）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_fire: Option<String>,
    pub enabled: bool,
}

/// MCP 服务器列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerListDto {
    pub servers: Vec<McpServerDto>,
}

/// 单个 MCP 服务器 DTO
///
/// 字段对齐 `peri_middlewares::mcp::client::ServerInfo` / `McpServerConfig`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDto {
    pub name: String,
    /// 连接状态（connected / failed / disconnected / disabled）
    pub status: String,
    /// 已注册工具数量
    pub tool_count: u32,
    pub tools: Vec<McpToolDto>,
    /// OAuth 状态（仅 HTTP 传输有意义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_status: Option<String>,
}

/// MCP 工具 DTO
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDto {
    pub name: String,
    pub description: String,
    /// 工具 inputSchema 的 JSON 字符串
    pub schema_json: String,
}

/// 插件列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginListDto {
    pub plugins: Vec<PluginDto>,
}

/// 插件 DTO
///
/// 字段对齐 `peri_middlewares::plugin::types::InstalledPlugin`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDto {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub description: String,
}

/// Hook 列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookListDto {
    pub hooks: Vec<HookDto>,
}

/// Hook DTO
///
/// 字段对齐 `peri_middlewares::hooks::types::HookType`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDto {
    /// Hook 唯一标识
    pub id: String,
    /// 事件名称（PreToolUse / PostToolUse / SessionStart 等）
    pub event: String,
    /// 执行命令或 prompt 摘要
    pub command: String,
    pub enabled: bool,
}

/// 模型别名 DTO（ModelPanel 查询）
///
/// 字段对齐 `peri_acp::provider::config::ProviderModels` 的 alias 映射。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAliasDto {
    pub alias: String,
    pub provider: String,
    pub model_id: String,
}

/// Provider 快照 DTO（StatusPanel / LoginPanel 查询）
///
/// 字段对齐 `peri_acp::provider::config::ProviderConfig`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSnapshotDto {
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub authenticated: bool,
}

/// Workflow 运行列表 DTO（WorkflowPanel 查询）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunListDto {
    pub runs: Vec<WorkflowRunDto>,
}

/// 单个 Workflow 运行 DTO
///
/// 字段对齐 `WorkflowProgressDto` 的 run 级别字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunDto {
    pub run_id: String,
    pub status: String,
    /// 启动时间（ISO 8601）
    pub started_at: String,
    pub agents_done: u32,
    pub agents_total: u32,
}

/// 系统状态 DTO（StatusPanel 查询）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatusDto {
    pub version: String,
    pub uptime_seconds: u64,
    pub sessions_active: u32,
    pub model: String,
    pub provider: String,
}

/// 资源使用量 DTO（MemoryPanel 查询）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsageDto {
    pub cpu_percent: f32,
    pub memory_mb: u64,
    pub disk_mb: u64,
}
