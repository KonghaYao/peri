// Live 事件与持久历史共享工具的标准 ACP 投影；adapter 保留来源和兼容字段差异。
use agent_client_protocol_schema::v1::{
    Content, ContentBlock, TextContent, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use peri_acp_types::builtin_mcp::original_tool_name_of_effective;
use peri_acp_types::error::SafeSubagentFailure;
use serde_json::Value;

pub(crate) fn project_tool_start(id: &str, name: &str, input: &Value) -> ToolCall {
    ToolCall::new(id.to_owned(), name.to_owned())
        .kind(infer_tool_kind(name))
        .status(ToolCallStatus::InProgress)
        .raw_input(Some(input.clone()))
}

pub(crate) fn project_tool_completion(
    id: &str,
    output: &str,
    is_error: bool,
    failure: Option<&SafeSubagentFailure>,
) -> ToolCallUpdate {
    let update = ToolCallUpdate::new(
        id.to_owned(),
        ToolCallUpdateFields::new()
            .status(if is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            })
            .content(tool_result_content(output, is_error)),
    );
    if let Some(failure) = failure {
        update.meta(serde_json::Map::from_iter([(
            "peri".to_string(),
            serde_json::json!({ "subagentFailure": failure }),
        )]))
    } else {
        update
    }
}

/// 工具失败且无可展示文本时的稳定 fallback（非空、通用、不含内部细节）。
const TOOL_FAILED_FALLBACK: &str = "Tool execution failed";

/// 工具结果的标准展示 `content` 投影（单个 Text block）。
///
/// 失败且底层文本为空白时使用 [`TOOL_FAILED_FALLBACK`]，保证客户端
/// 不会因空串静默丢弃失败；成功路径保持底层文本原样（可能为空）。
/// live mapper 与 session replay 共用此规则，避免两种路径的协议形态漂移。
pub fn tool_result_content(output: &str, is_error: bool) -> Vec<ToolCallContent> {
    let text = if is_error && output.trim().is_empty() {
        TOOL_FAILED_FALLBACK
    } else {
        output
    };
    vec![ToolCallContent::Content(Content::new(ContentBlock::Text(
        TextContent::new(text),
    )))]
}

/// 工具名 → `ToolKind` 投影。
///
/// **A4 匹配型归一（IF-D7 B 节）**：builtin 一等工具的模型面名字是 effective name
/// （如 `mcp__web__WebFetch`），入口先经 [`original_tool_name_of_effective`]
/// （IF-D15 唯一归一入口）换回原始工具名后走既有分支——因此今天裸名命中的
/// `Fetch` 分类对迁移后的 `mcp__web__*` 仍然成立，且**不新增** effective name 字面量
/// 分支（`mcp__artifact__artifact` → `artifact` → 仍落 `Other`）。
///
/// 归一未命中（未知 / 外部 `mcp__*`，例如 `mcp__foo__bar`、大小写不匹配的
/// `mcp__web__fetch`）时保持原样 ⇒ 分类结果与迁移前逐位一致（仍为 `Other`）。
/// 投影真值（`ToolCall` 载荷上的 name）**不**被归一改写：仍是 effective name。
fn infer_tool_kind(name: &str) -> ToolKind {
    let name = original_tool_name_of_effective(name).unwrap_or(name);
    match name {
        "Read" => ToolKind::Read,
        "Write" | "Edit" | "folder_operations" => ToolKind::Edit,
        "Bash" => ToolKind::Execute,
        "Grep" | "Glob" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}
