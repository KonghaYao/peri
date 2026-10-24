//! History V2 页面/条目准入校验（设计 §8.1、§8.2）。
//!
//! initial 与 `page`/`seek` 共用同一份校验：页面身份、预算、条目身份与子项上界只
//! 在这里定义一次。错误文本只描述字段位置，不回显 token、cursor 或正文。

use std::collections::BTreeSet;

use peri_acp_types::history_v2::{
    HISTORY_IDENTITY_MAX_BYTES, HISTORY_TOKEN_MAX_BYTES, HISTORY_V2_PROJECTION_VERSION,
    HISTORY_V2_VERSION, HistoryBodyDescriptor, HistoryEntry, HistoryLimits, HistorySource,
    HistorySubItem, HistoryWindowPage,
};

/// 页面准入失败：会话不匹配与其余非法形状分开，便于 initial 映射稳定 reason。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PageValidationError {
    SessionMismatch,
    Invalid(String),
}

impl PageValidationError {
    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid(detail.into())
    }
}

/// 校验一页是否可在当前会话/预算下接纳。
pub(crate) fn validate_window_page(
    page: &HistoryWindowPage,
    expected_session: &str,
    limits: &HistoryLimits,
) -> Result<(), PageValidationError> {
    if page.version != HISTORY_V2_VERSION {
        return Err(PageValidationError::invalid(
            "window page version must be 2",
        ));
    }
    if page.session_id != expected_session {
        return Err(PageValidationError::SessionMismatch);
    }
    if page.projection_version != HISTORY_V2_PROJECTION_VERSION {
        return Err(PageValidationError::invalid(
            "window page projectionVersion does not match the client contract",
        ));
    }
    require_identity("windowId", &page.window_id)?;
    require_identity("snapshotToken", &page.snapshot_token)?;
    require_token("readTicket", &page.read_ticket)?;
    require_optional_token("beforeCursor", page.before_cursor.as_deref())?;
    require_optional_token("afterCursor", page.after_cursor.as_deref())?;
    page.progress
        .validate()
        .map_err(|error| PageValidationError::invalid(error.to_string()))?;
    if page.items.len() > limits.max_items as usize {
        return Err(PageValidationError::invalid("window page exceeds maxItems"));
    }

    let mut entries = BTreeSet::new();
    for entry in &page.items {
        validate_entry(entry, limits)?;
        if !entries.insert(entry_identity_key(&entry.source, &entry.entry_id)) {
            return Err(PageValidationError::invalid(
                "window page repeats an entry identity",
            ));
        }
    }
    Ok(())
}

/// 稳定条目身份：source + entryId；不依赖文本 hash。
pub(crate) fn entry_identity_key(source: &HistorySource, entry_id: &str) -> String {
    match source {
        HistorySource::Own => format!("own|{}", length_prefixed(entry_id)),
        HistorySource::Inherited(origin) => format!(
            "inherited|{}|{}|{}",
            length_prefixed(&origin.snapshot_id),
            length_prefixed(&origin.ancestor_session_id),
            length_prefixed(entry_id)
        ),
    }
}

/// 长度前缀避免 opaque 身份中的分隔符造成键碰撞。
fn length_prefixed(value: &str) -> String {
    format!("{}:{value}", value.len())
}

pub(crate) fn entry_key(entry: &HistoryEntry) -> String {
    entry_identity_key(&entry.source, &entry.entry_id)
}

/// 子项稳定身份；V2 初版每行一个语义 subItem。
pub(crate) fn sub_item_id(item: &HistorySubItem) -> &str {
    match item {
        HistorySubItem::Text(item) | HistorySubItem::Reasoning(item) => &item.sub_item_id,
        HistorySubItem::ToolCall(item) => &item.sub_item_id,
        HistorySubItem::ToolResult(item) => &item.sub_item_id,
        HistorySubItem::Reminder(item) | HistorySubItem::Summary(item) => &item.sub_item_id,
        HistorySubItem::Attachment(item) => &item.sub_item_id,
        HistorySubItem::Opaque(item) => &item.sub_item_id,
    }
}

/// 子项外置正文描述符；内联子项返回 `None`。
pub(crate) fn sub_item_descriptor(item: &HistorySubItem) -> Option<&HistoryBodyDescriptor> {
    match item {
        HistorySubItem::Text(item) | HistorySubItem::Reasoning(item) => item.body.as_ref(),
        HistorySubItem::ToolCall(item) => item.body.as_ref(),
        HistorySubItem::ToolResult(item) => item.body.as_ref(),
        HistorySubItem::Reminder(item) | HistorySubItem::Summary(item) => item.body.as_ref(),
        HistorySubItem::Attachment(item) => Some(&item.body),
        HistorySubItem::Opaque(item) => Some(&item.body),
    }
}

fn validate_entry(entry: &HistoryEntry, limits: &HistoryLimits) -> Result<(), PageValidationError> {
    require_identity("entryId", &entry.entry_id)?;
    entry
        .source
        .validate()
        .map_err(|error| PageValidationError::invalid(error.to_string()))?;
    if entry.content_version.is_empty() {
        return Err(PageValidationError::invalid(
            "entry contentVersion must not be empty",
        ));
    }
    let mut sub_items = BTreeSet::new();
    for sub_item in &entry.sub_items {
        sub_item
            .validate_bounded(limits)
            .map_err(|error| PageValidationError::invalid(error.to_string()))?;
        if !sub_items.insert(sub_item_id(sub_item).to_owned()) {
            return Err(PageValidationError::invalid(
                "entry repeats a subItem identity",
            ));
        }
    }
    Ok(())
}

fn require_identity(field: &str, value: &str) -> Result<(), PageValidationError> {
    if value.is_empty() || value.len() > HISTORY_IDENTITY_MAX_BYTES {
        return Err(PageValidationError::invalid(format!(
            "{field} is empty or exceeds its byte bound"
        )));
    }
    Ok(())
}

fn require_token(field: &str, value: &str) -> Result<(), PageValidationError> {
    if value.is_empty() || value.len() > HISTORY_TOKEN_MAX_BYTES {
        return Err(PageValidationError::invalid(format!(
            "{field} is empty or exceeds its byte bound"
        )));
    }
    Ok(())
}

fn require_optional_token(field: &str, value: Option<&str>) -> Result<(), PageValidationError> {
    match value {
        Some(value) => require_token(field, value),
        None => Ok(()),
    }
}
