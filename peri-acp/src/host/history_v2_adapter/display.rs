//! opaque 正文的展示投影：canonical payload → 纯文本。
//!
//! 规则与页面投影同源：先过 [`dispatch::project_payload_updates`]，再按更新类型渲染成
//! 文本。这里**不**暴露 canonical JSON 信封，也不返回隐藏的 model-only reminder。
//! 单条工具输入有独立上限，避免一个参数把详情首屏撑爆。

use agent_client_protocol_schema::v1::{
    ContentBlock, EmbeddedResourceResource, SessionUpdate, ToolCall, ToolCallUpdate,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::PeriCaps;
use sha2::{Digest, Sha256};

use crate::dispatch::{self, ProjectedUpdate};

/// 单条工具调用输入在展示文本中的上限（设计 §8.3 的“有界语义头”扩展）。
pub(super) const TOOL_INPUT_LIMIT_BYTES: usize = 64 * 1024;

/// 把一条已解码 payload 投影成展示纯文本；没有可见内容时返回 `None`。
pub(super) fn display_text(payload: &PersistedPayload, caps: &PeriCaps) -> Option<String> {
    let mut segments = Vec::new();
    for update in dispatch::project_payload_updates(payload, caps, None) {
        if let Some(segment) = render_update(update) {
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments.join("\n\n"))
}

/// canonical payload 的稳定内容标识：SHA-256 前 16 字节的小写 hex。
///
/// 只依赖内容字节：同一内容跨请求一致；内容被改写/重建后自然变化。
pub(super) fn content_version(canonical: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(canonical);
    let mut version = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        // 写入 String 不会失败；显式忽略返回值以满足 `must_use`。
        let _ = write!(version, "{byte:02x}");
    }
    version
}

fn render_update(update: ProjectedUpdate) -> Option<String> {
    match update {
        ProjectedUpdate::Session(update) => match *update {
            SessionUpdate::UserMessageChunk(chunk) => render_content(&chunk.content),
            SessionUpdate::AgentMessageChunk(chunk) => render_content(&chunk.content),
            SessionUpdate::AgentThoughtChunk(chunk) => render_content(&chunk.content),
            SessionUpdate::ToolCall(tool_call) => Some(render_tool_call(&tool_call)),
            SessionUpdate::ToolCallUpdate(update) => render_tool_result(&update),
            _ => None,
        },
        ProjectedUpdate::SystemReminder(reminder) => {
            if !dispatch::reminder_visible_to_client(&reminder) {
                return None;
            }
            let title = reminder
                .summary
                .clone()
                .unwrap_or_else(|| reminder.kind.clone());
            Some(format!("提醒：{title}\n{}", reminder.body))
        }
    }
}

fn render_content(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::Image(image) => {
            Some(attachment("图片", &image.mime_type, image.uri.as_deref()))
        }
        ContentBlock::Audio(audio) => Some(attachment("音频", &audio.mime_type, None)),
        ContentBlock::ResourceLink(link) => {
            let label = link.title.clone().unwrap_or_else(|| link.name.clone());
            let media_type = link
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            Some(attachment(&label, &media_type, Some(&link.uri)))
        }
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => Some(text.text.clone()),
            EmbeddedResourceResource::BlobResourceContents(blob) => {
                let media_type = blob
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                Some(attachment(&blob.uri, &media_type, Some(&blob.uri)))
            }
            _ => None,
        },
        // 当前 projector 只产出文本块；未知块不作为展示正文。
        _ => None,
    }
}

fn attachment(label: &str, media_type: &str, uri: Option<&str>) -> String {
    let mut text =
        format!("附件：{label}\n类型：{media_type}\n（二进制正文按范围读取，不在历史文本中传输）");
    if let Some(uri) = uri {
        text.push_str(&format!("\n地址：{uri}"));
    }
    text
}

fn render_tool_call(tool_call: &ToolCall) -> String {
    let mut text = format!(
        "工具调用：{}\n状态：{}",
        tool_call.title,
        enum_name(&tool_call.status)
    );
    if let Some(input) = &tool_call.raw_input {
        let rendered = serde_json::to_string(input).unwrap_or_else(|_| "null".to_owned());
        text.push_str("\n输入：\n");
        text.push_str(&bounded(&rendered, TOOL_INPUT_LIMIT_BYTES));
    }
    text
}

fn render_tool_result(update: &ToolCallUpdate) -> Option<String> {
    let status = update.fields.status?;
    let mut text = format!("工具结果：{}", enum_name(&status));
    if let Some(output) = &update.fields.raw_output {
        let rendered = match output {
            serde_json::Value::String(value) => value.clone(),
            value => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
        };
        text.push_str("\n输出：\n");
        text.push_str(&rendered);
    }
    Some(text)
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "unknown".to_owned(),
    }
}

/// 截断到 `max` 字节以内（含截断说明），且不切断 UTF-8 字符。
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    const MARKER: &str = "\n…（输入已截断）";
    let budget = max.saturating_sub(MARKER.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end + MARKER.len());
    truncated.push_str(&text[..end]);
    truncated.push_str(MARKER);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::messages::BaseMessage;

    #[test]
    fn content_version_is_stable_short_hex() {
        let first = content_version(b"payload");
        let second = content_version(b"payload");
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
        assert!(first
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(first, content_version(b"payload-2"));
        assert_ne!(first, "1");
    }

    #[test]
    fn non_text_block_renders_attachment_note() {
        let block = ContentBlock::Image(agent_client_protocol_schema::v1::ImageContent::new(
            "aGVsbG8=",
            "image/png",
        ));
        let text = render_content(&block).expect("attachment");
        assert!(text.contains("图片"));
        assert!(text.contains("image/png"));
        assert!(text.contains("二进制正文"));
        assert!(!text.contains("aGVsbG8="), "media bytes must not leak");
    }

    #[test]
    fn raw_payload_has_no_display_text() {
        let payload = PersistedPayload::Message(BaseMessage::ai(
            peri_acp_types::messages::MessageContent::raw(vec![serde_json::json!({
                "type": "vendor_native",
                "blob": "x".repeat(1024)
            })]),
        ));
        assert_eq!(display_text(&payload, &PeriCaps::default()), None);
    }

    #[test]
    fn tool_input_is_bounded_without_splitting_utf8() {
        let text = "中".repeat(TOOL_INPUT_LIMIT_BYTES);
        let bounded = bounded(&text, TOOL_INPUT_LIMIT_BYTES);
        assert!(bounded.len() <= TOOL_INPUT_LIMIT_BYTES);
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
        assert!(bounded.ends_with("（输入已截断）"));
    }
}
