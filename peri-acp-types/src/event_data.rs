//! Event data structures for the peri-acp protocol.
//!
//! Every custom event pushed through `peri/unstable-event` carries one of
//! these structs as its `data` payload. The event name (kebab-case string)
//! selects which struct to deserialize into.
//!
//! Reference: `docs/design/peri-acp-protocol.md` section 4 "Event Directory".

use serde::{Deserialize, Serialize};

// ===========================================================================
// §4.3 Status events (update status bar, no message-area changes)
// ===========================================================================

/// `"tool-count"` — number of tool calls in the current turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCount {
    pub count: u64,
}

/// `"progress"` — progress percentage with a human-readable label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Progress {
    pub percent: u32,
    pub label: String,
}

/// `"budget-warning"` — context budget threshold crossed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BudgetWarning {
    pub used: u64,
    pub limit: u64,
    pub threshold: String,
}

/// `"system-notification"` — system-level notification text with severity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemNotification {
    pub text: String,
    pub level: String,
}

// ===========================================================================
// §4.4 Input assist events
// ===========================================================================

/// `"prediction"` — input prediction suggestion shown as a grey placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Prediction {
    /// 占位文本（兼容既有消费方；无结构化动作时的回落值）
    pub text: String,
    /// 结构化动作列表（新通道；旧消费方忽略此字段）
    #[serde(default)]
    pub actions: Vec<PredictionAction>,
}

/// Prediction 结构化动作
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PredictionAction {
    /// 输入区灰色占位文本（= 原 Prediction.text 语义）
    Placeholder { text: String },
    /// 改会话标题（仅模型判断话题显著转变时输出）
    SetTitle { title: String },
    /// 给会话加标签（持久化到 session 元数据，不展示）
    AddTag { tag: String },
    /// 会话摘要（展示在 loading spinner 名言位）
    Summary { text: String },
}

/// `"file-suggestions"` — @-mention file completion candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileSuggestions {
    pub files: Vec<String>,
}

// ===========================================================================
// §4.5 Interaction request events (require user decision)
// ===========================================================================

/// `"hitl-pending"` — HITL tool approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HitlPending {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// Additional tools in the same approval batch, or `null` if standalone.
    pub batch: Option<Vec<ToolApproval>>,
}

/// A single tool entry within an HITL approval batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolApproval {
    pub tool_id: String,
    pub tool_name: String,
    pub input_summary: String,
}

/// `"ask-user"` — multi-question form initiated by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AskUser {
    pub questions: Vec<Question>,
}

/// A single question in an `AskUser` form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// A selectable option within a `Question`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// `"rewind-preview"` — preview of changes that will be undone by a rewind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewindPreview {
    pub files: Vec<FileChange>,
    pub messages: Vec<RewindMessage>,
}

/// A single file change in a rewind preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileChange {
    pub path: String,
    pub change_type: String,
    /// Unified diff preview for the change, if available.
    pub diff: Option<String>,
}

/// A single message in a rewind preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewindMessage {
    pub id: String,
    pub role: String,
    pub preview: String,
}

/// `"oauth-needed"` — MCP server authorization required.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OauthNeeded {
    pub server_name: String,
    pub auth_url: String,
}

// ===========================================================================
// §4.9 Plugin events
// ===========================================================================

/// `"plugin-snapshot"` — 插件列表全量快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PluginSnapshot {
    pub plugins: Vec<PluginSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PluginSnapshotEntry {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub root: String,
    pub description: String,
    pub marketplace: String,
    pub author: Option<String>,
    pub skills_count: usize,
    pub commands_count: usize,
    pub agents_count: usize,
    pub mcp_count: usize,
    pub install_scope: String,
    pub load_error: Option<String>,
}

/// `"plugin-action-result"` — 操作结果通知。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PluginActionResult {
    pub action: String,
    pub plugin_name: String,
    pub success: bool,
    pub error: Option<String>,
}

/// `"plugin-search-result"` — Discover 搜索返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PluginSearchResult {
    pub query: String,
    pub results: Vec<PluginSnapshotEntry>,
    pub from_cache: bool,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "event_data_test.rs"]
mod tests;
