// Live 事件与持久历史共享工具的标准 ACP 投影；adapter 保留来源和兼容字段差异。
use agent_client_protocol_schema::v1::{
    Content, ContentBlock, ImageContent, TextContent, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use peri_acp_types::builtin_mcp;
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
    images: &[peri_acp_types::tools::ToolImage],
) -> ToolCallUpdate {
    let mut content = tool_result_content(output, is_error);
    // 图片走 ACP 标准 content 块（`ToolCallContent::Content`）：`rawOutput` 仍是
    // 纯文本，不改变机器消费方对字符串的既有解析。
    let (displayed, omitted) = display_images(images);
    content.extend(displayed.into_iter().map(|image| {
        ToolCallContent::Content(Content::new(ContentBlock::Image(ImageContent::new(
            image.data.clone(),
            image.mime_type.clone(),
        ))))
    }));
    let update = ToolCallUpdate::new(
        id.to_owned(),
        ToolCallUpdateFields::new()
            .status(if is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            })
            .content(content),
    );
    let mut peri_meta = serde_json::Map::new();
    if let Some(failure) = failure {
        peri_meta.insert("subagentFailure".to_string(), serde_json::json!(failure));
    }
    if omitted > 0 {
        // 客户端据此显示「图片过大未展示」meta 行；超限图片本身不进 content，
        // 模型可见文本里的 `[image: mime]` 占位不受影响。
        peri_meta.insert(
            peri_acp_types::tools::TOOL_IMAGES_OMITTED_META_KEY.to_string(),
            serde_json::json!(omitted),
        );
    }
    if peri_meta.is_empty() {
        update
    } else {
        update.meta(serde_json::Map::from_iter([(
            "peri".to_string(),
            Value::Object(peri_meta),
        )]))
    }
}

/// 单张工具图片进入展示面的 base64 字符上限（256 KiB）。
///
/// 展示面图片与模型面共享同一载荷，但模型面单条上限是 15 MiB（为 transcript
/// 预留）；实时投递缓冲只有 [`crate::host::live_delivery::MAX_BUFFERED_BYTES`]
/// （1 MiB），一条 15 MiB 的 update 会挤掉整个暂停期缓冲。展示图片是可选增强，
/// 超限时保留 `[image: mime]` 文本即可。
pub(crate) const TOOL_IMAGE_DISPLAY_MAX_CHARS: usize = 256 * 1024;

/// 单条工具 update 全部展示图片的 base64 合计上限（384 KiB）。
///
/// 与 [`TOOL_IMAGE_DISPLAY_MAX_CHARS`] 一起保证单条 update 序列化后明显小于
/// 1 MiB 的实时缓冲预算（留出 rawOutput 文本与 JSON 封装余量）。
pub(crate) const TOOL_UPDATE_IMAGE_MAX_CHARS: usize = 384 * 1024;

/// 按展示预算挑选图片，返回（可展示图片, 被省略张数）。
///
/// 单张或累计超限的图片被省略；调用方把省略数写进 `_meta.peri`，并把文本
/// 占位 `[image: mime]` 留给 `rawOutput`。
fn display_images(
    images: &[peri_acp_types::tools::ToolImage],
) -> (Vec<&peri_acp_types::tools::ToolImage>, usize) {
    let mut accepted = Vec::new();
    let mut accepted_chars = 0usize;
    let mut omitted = 0usize;
    for image in images {
        let chars = image.data.len();
        if chars > TOOL_IMAGE_DISPLAY_MAX_CHARS
            || chars > TOOL_UPDATE_IMAGE_MAX_CHARS.saturating_sub(accepted_chars)
        {
            omitted += 1;
            continue;
        }
        accepted_chars += chars;
        accepted.push(image);
    }
    (accepted, omitted)
}

/// 工具失败且无可展示文本时的稳定 fallback（非空、通用、不含内部细节）。
const TOOL_FAILED_FALLBACK: &str = "Tool execution failed";

/// 工具结果的标准展示 `content` 投影（单个 Text block）。
///
/// 失败且底层文本为空白时使用 `TOOL_FAILED_FALLBACK`，保证客户端
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
    let name = builtin_mcp::original_tool_name_of_effective(name).unwrap_or(name);
    match name {
        "Read" => ToolKind::Read,
        "Write" | "Edit" | "folder_operations" => ToolKind::Edit,
        "Bash" => ToolKind::Execute,
        "Grep" | "Glob" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}
