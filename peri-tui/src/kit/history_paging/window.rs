//! S4 `historyWindowV1` 窗口请求/响应 DTO 与解码。
//!
//! 与 `peri-acp-types/src/history_window.rs` 的 wire 形状一一对应；本模块只做编码、
//! 解码和代次校验，不驱动请求，也不修改 legacy 分页路径。
//!
//! 这些类型在 S5.4 接入 `open/page/seek` 前暂无生产调用方；本阶段由单元测试覆盖，
//! 因此允许本模块存在未使用项，避免把“已定义待接入”误报成死代码。
#![allow(dead_code)]

use serde_json::{Value, json};

use crate::kit::history_window::{
    HistoryEntryView, HistorySource, PersistentEntryKey, PersistentSubItemKey, WindowIdentity,
};

// ---------------------------------------------------------------------------
// S4 窗口请求/响应（与 `peri-acp-types/src/history_window.rs` 对齐）
// ---------------------------------------------------------------------------

/// 服务端默认单页行数 / 上限（与 S4 常量一致）。
pub(crate) const WINDOW_PAGE_LIMIT: u16 = 50;
pub(crate) const WINDOW_MAX_LIMIT: u16 = 100;
pub(crate) const WINDOW_DEFAULT_BYTE_BUDGET: u32 = 512 * 1024;
pub(crate) const WINDOW_MAX_BYTE_BUDGET: u32 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WindowRequestError {
    #[error("history limit is out of range")]
    InvalidLimit,
    #[error("history request token must not be empty")]
    EmptyToken,
    #[error("history byteBudget is out of range")]
    InvalidByteBudget,
    #[error("history seek anchor entryId must not be empty")]
    EmptyAnchor,
    #[error("history body maxBytes is out of range")]
    InvalidChunkBytes,
}

fn validate_limit(limit: u16) -> Result<(), WindowRequestError> {
    if (1..=WINDOW_MAX_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(WindowRequestError::InvalidLimit)
    }
}

fn validate_token(name: &str, value: &str) -> Result<(), WindowRequestError> {
    if value.is_empty() {
        tracing::debug!(name, "history window request token is empty");
        Err(WindowRequestError::EmptyToken)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowDirection {
    Around,
    Before,
    After,
}

impl WindowDirection {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Around => "around",
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowRequestAnchor {
    Latest,
    Entry(PersistentEntryKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowOpenRequest {
    pub anchor: WindowRequestAnchor,
    pub direction: WindowDirection,
    pub limit: u16,
    /// 显式方向预算优先于 `limit`；None 表示服务端按 `limit` 取默认。
    pub before_limit: Option<u16>,
    pub after_limit: Option<u16>,
    pub byte_budget: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowPageCursor {
    Before(String),
    After(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowPageRequest {
    pub snapshot_token: String,
    pub read_ticket: String,
    pub cursor: WindowPageCursor,
    pub limit: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowSeekRequest {
    pub snapshot_token: String,
    pub read_ticket: String,
    pub anchor: PersistentEntryKey,
    pub before_limit: u16,
    pub after_limit: u16,
}

/// `session/load` 的 `history` 判别联合。与旧顶层 `cursor/limit` 互斥。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowRequest {
    Open(WindowOpenRequest),
    Page(WindowPageRequest),
    Seek(WindowSeekRequest),
}

impl WindowRequest {
    /// 请求形状校验（限值来自 S4；不合法不发送）。
    pub(crate) fn validate(&self) -> Result<(), WindowRequestError> {
        match self {
            Self::Open(request) => {
                validate_limit(request.limit)?;
                if let Some(limit) = request.before_limit {
                    validate_limit(limit)?;
                }
                if let Some(limit) = request.after_limit {
                    validate_limit(limit)?;
                }
                if request.byte_budget == 0 || request.byte_budget > WINDOW_MAX_BYTE_BUDGET {
                    return Err(WindowRequestError::InvalidByteBudget);
                }
            }
            Self::Page(request) => {
                validate_token("snapshotToken", &request.snapshot_token)?;
                validate_token("readTicket", &request.read_ticket)?;
                validate_limit(request.limit)?;
            }
            Self::Seek(request) => {
                validate_token("snapshotToken", &request.snapshot_token)?;
                validate_token("readTicket", &request.read_ticket)?;
                if request.anchor.entry_id.is_empty() {
                    return Err(WindowRequestError::EmptyAnchor);
                }
                validate_limit(request.before_limit)?;
                validate_limit(request.after_limit)?;
            }
        }
        Ok(())
    }

    /// 编码为 S4 wire 形状（camelCase + `kind` 判别）。
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Open(request) => {
                let mut value = json!({
                    "kind": "open",
                    "anchor": anchor_value(&request.anchor),
                    "direction": request.direction.as_wire(),
                    "limit": request.limit,
                    "byteBudget": request.byte_budget,
                });
                if let Some(limit) = request.before_limit {
                    value["beforeLimit"] = json!(limit);
                }
                if let Some(limit) = request.after_limit {
                    value["afterLimit"] = json!(limit);
                }
                value
            }
            Self::Page(request) => {
                let mut value = json!({
                    "kind": "page",
                    "snapshotToken": request.snapshot_token,
                    "readTicket": request.read_ticket,
                    "limit": request.limit,
                });
                match &request.cursor {
                    WindowPageCursor::Before(cursor) => {
                        value["beforeCursor"] = json!(cursor);
                    }
                    WindowPageCursor::After(cursor) => {
                        value["afterCursor"] = json!(cursor);
                    }
                }
                value
            }
            Self::Seek(request) => json!({
                "kind": "seek",
                "snapshotToken": request.snapshot_token,
                "readTicket": request.read_ticket,
                "anchor": seek_anchor_value(&request.anchor),
                "beforeLimit": request.before_limit,
                "afterLimit": request.after_limit,
            }),
        }
    }
}

fn anchor_value(anchor: &WindowRequestAnchor) -> Value {
    match anchor {
        WindowRequestAnchor::Latest => json!({ "kind": "latest" }),
        WindowRequestAnchor::Entry(entry) => json!({
            "kind": "entry",
            "entryId": entry.entry_id,
            "source": source_value(&entry.source),
        }),
    }
}

fn seek_anchor_value(entry: &PersistentEntryKey) -> Value {
    json!({
        "entryId": entry.entry_id,
        "source": source_value(&entry.source),
    })
}

fn source_value(source: &HistorySource) -> Value {
    match source {
        HistorySource::Own => json!("own"),
        HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } => {
            let mut inherited = json!({ "frozenSnapshotId": frozen_snapshot_id });
            if let Some(ancestor) = ancestor_thread_id {
                inherited["ancestorThreadId"] = json!(ancestor);
            }
            json!({ "inherited": inherited })
        }
    }
}

fn decode_source(value: &Value) -> Option<HistorySource> {
    if value.as_str() == Some("own") {
        return Some(HistorySource::Own);
    }
    let inherited = value.get("inherited")?;
    let frozen_snapshot_id = inherited
        .get("frozenSnapshotId")
        .or_else(|| inherited.get("frozen_snapshot_id"))?
        .as_str()?;
    let ancestor_thread_id = inherited
        .get("ancestorThreadId")
        .or_else(|| inherited.get("ancestor_thread_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(HistorySource::inherited_with_ancestor(
        frozen_snapshot_id,
        ancestor_thread_id,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowBodyDescriptor {
    /// descriptor 自身不带来源；匹配到 items 时用 item 的来源补齐。
    pub entry_id: String,
    pub source: Option<HistorySource>,
    pub total_bytes: u64,
    pub chunk_size: u32,
    pub read_ticket: String,
    /// 详情读取绑定的快照；内联 body 优先，缺失时回退响应 snapshotToken。
    pub snapshot_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSubItem {
    pub position: u32,
    pub update: Value,
    pub system_reminder: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowPageItem {
    pub source: HistorySource,
    pub entry_id: String,
    /// 仅 own 来源有；继承来源没有（S4 wire 不发出该字段）。
    pub position: Option<u64>,
    pub oversize: bool,
    pub sub_items: Vec<WindowSubItem>,
    pub descriptor: Option<WindowBodyDescriptor>,
    /// 由顶层 descriptor 合成、items 中没有对应条目的超限占位。
    pub descriptor_only: bool,
}

impl WindowPageItem {
    pub(crate) fn persistent_key(&self) -> PersistentEntryKey {
        PersistentEntryKey::new(self.source.clone(), self.entry_id.clone())
    }

    /// 构造窗口条目身份。`body_bytes`/`reserved_bytes` 由装配方按实际展示单元计量。
    pub(crate) fn to_entry_view(
        &self,
        unit_count: usize,
        body_bytes: usize,
        reserved_bytes: usize,
    ) -> HistoryEntryView {
        let key = self.persistent_key();
        let sub_items = self
            .sub_items
            .iter()
            .map(|sub| PersistentSubItemKey::new(key.clone(), sub.position))
            .collect();
        HistoryEntryView::new(
            key,
            self.position,
            unit_count,
            body_bytes,
            reserved_bytes,
            sub_items,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInheritedSnapshot {
    pub snapshot_id: String,
    pub ancestor_thread_id: Option<String>,
    pub first_entry_id: Option<String>,
    pub last_entry_id: Option<String>,
    pub entry_count: usize,
    pub supported: bool,
}

/// 一次 open/page/seek 的客户端视图。任一必需字段缺失即整页丢弃。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowPageResponse {
    pub session_id: String,
    pub identity: WindowIdentity,
    pub committed_tail: Option<String>,
    pub checkpoint_id: Option<String>,
    pub read_ticket: String,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub items: Vec<WindowPageItem>,
    /// 顶层 `bodyDescriptors` 解码结果（保留原始列表供诊断/测试）。
    pub body_descriptors: Vec<WindowBodyDescriptor>,
    /// items 中不存在、由 descriptor 合成的超限占位数量。
    pub descriptor_only_items: usize,
    pub inherited: Option<WindowInheritedSnapshot>,
    pub inherited_unsupported: bool,
    pub reopen_required: bool,
}

pub(crate) fn decode_window_page_response(value: &Value) -> Option<WindowPageResponse> {
    let session_id = value.get("sessionId")?.as_str()?.to_string();
    let session_generation = value.get("sessionGeneration")?.as_str()?.to_string();
    let structure_version = value.get("structureVersion")?.as_u64()?;
    let snapshot_token = value.get("snapshotToken")?.as_str()?.to_string();
    let read_ticket = value.get("readTicket")?.as_str()?.to_string();
    let committed_tail = value
        .get("committedTail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let checkpoint_id = value
        .get("checkpointId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let before_cursor = value
        .get("beforeCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let after_cursor = value
        .get("afterCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let items = value
        .get("items")?
        .as_array()?
        .iter()
        .map(|item| decode_window_item(item, &snapshot_token))
        .collect::<Option<Vec<_>>>()?;
    let body_descriptors = decode_body_descriptors(value, &snapshot_token)?;
    let mut items = items;
    let descriptor_only_items = attach_body_descriptors(&mut items, &body_descriptors)?;
    let inherited = value.get("inherited").and_then(decode_inherited_snapshot);
    let inherited_unsupported = value
        .get("inheritedUnsupported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reopen_required = value
        .get("rewindInvalidation")
        .and_then(|invalidation| invalidation.get("reopenRequired"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(WindowPageResponse {
        session_id: session_id.clone(),
        identity: WindowIdentity::new(
            session_id,
            session_generation,
            structure_version,
            snapshot_token,
        ),
        committed_tail,
        checkpoint_id,
        read_ticket,
        before_cursor,
        after_cursor,
        items,
        body_descriptors,
        descriptor_only_items,
        inherited,
        inherited_unsupported,
        reopen_required,
    })
}

fn decode_inherited_snapshot(value: &Value) -> Option<WindowInheritedSnapshot> {
    if value.is_null() {
        return None;
    }
    Some(WindowInheritedSnapshot {
        snapshot_id: value.get("snapshotId")?.as_str()?.to_string(),
        ancestor_thread_id: value
            .get("ancestorThreadId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        first_entry_id: value
            .get("firstEntryId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        last_entry_id: value
            .get("lastEntryId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        entry_count: usize::try_from(value.get("entryCount")?.as_u64()?).ok()?,
        supported: value
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn decode_window_item(value: &Value, snapshot_token: &str) -> Option<WindowPageItem> {
    let source = decode_source(value.get("source")?)?;
    let entry_id = value.get("entryId")?.as_str()?.to_string();
    let position = value.get("position").and_then(Value::as_u64);
    match (&source, position) {
        (HistorySource::Own, None) => return None,
        (HistorySource::Inherited { .. }, Some(_)) => return None,
        _ => {}
    }
    let oversize = value
        .get("oversize")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sub_items = value
        .get("subItems")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(decode_window_sub_item)
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_else(|| Some(Vec::new()))?;
    // 权威形状：超限条目内联 `body`；顶层 `bodyDescriptors` 仅作兼容输入。
    let descriptor = value.get("body").and_then(|body| {
        decode_body_descriptor(body, snapshot_token, &entry_id, Some(source.clone()))
    });
    Some(WindowPageItem {
        source,
        entry_id,
        position,
        oversize,
        sub_items,
        descriptor,
        descriptor_only: false,
    })
}

fn decode_window_sub_item(value: &Value) -> Option<WindowSubItem> {
    let position = u32::try_from(value.get("position")?.as_u64()?).ok()?;
    let (update, system_reminder) = match value.get("update") {
        Some(update) => (update.clone(), false),
        None => (value.get("systemReminder")?.clone(), true),
    };
    Some(WindowSubItem {
        position,
        update,
        system_reminder,
    })
}

fn decode_body_descriptors(
    value: &Value,
    snapshot_token: &str,
) -> Option<Vec<WindowBodyDescriptor>> {
    let Some(raw) = value
        .get("bodyDescriptors")
        .or_else(|| value.get("body_descriptors"))
    else {
        return Some(Vec::new());
    };
    raw.as_array()?
        .iter()
        .map(|descriptor| {
            let entry_id = descriptor.get("entryId")?.as_str()?;
            if entry_id.is_empty() {
                return None;
            }
            let source = descriptor.get("source").and_then(decode_source);
            decode_body_descriptor(descriptor, snapshot_token, entry_id, source)
        })
        .collect::<Option<Vec<_>>>()
}

fn decode_body_descriptor(
    value: &Value,
    default_snapshot_token: &str,
    entry_id: &str,
    source: Option<HistorySource>,
) -> Option<WindowBodyDescriptor> {
    if entry_id.is_empty() {
        return None;
    }
    let total_bytes = value.get("totalBytes")?.as_u64()?;
    if total_bytes == 0 {
        return None;
    }
    let chunk_size = u32::try_from(value.get("chunkSize")?.as_u64()?).ok()?;
    if chunk_size == 0 {
        return None;
    }
    let snapshot_token = value
        .get("snapshotToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .unwrap_or(default_snapshot_token)
        .to_string();
    Some(WindowBodyDescriptor {
        entry_id: entry_id.to_string(),
        source,
        total_bytes,
        chunk_size,
        read_ticket: value.get("readTicket")?.as_str()?.to_string(),
        snapshot_token: Some(snapshot_token),
    })
}

/// 顶层 `bodyDescriptors` 兼容输入。内联 `item.body` 已存在时以 item 为准。
///
/// - 匹配唯一且 item 无内联 descriptor：挂到 item 上；
/// - 没有对应 item：合成 `descriptor_only` 占位（兼容 S4 类型字段形状）；
/// - 同一顶层 descriptor 重复或匹配歧义：整页丢弃，不猜；
/// - 返回合成的占位数量。
fn attach_body_descriptors(
    items: &mut Vec<WindowPageItem>,
    descriptors: &[WindowBodyDescriptor],
) -> Option<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut synthesized = 0usize;
    for descriptor in descriptors {
        if !seen.insert((descriptor.entry_id.clone(), descriptor.source.clone())) {
            return None;
        }
        let matches: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.entry_id == descriptor.entry_id
                    && descriptor
                        .source
                        .as_ref()
                        .is_none_or(|source| source == &item.source)
            })
            .map(|(index, _)| index)
            .collect();
        match matches.as_slice() {
            [] => {
                let source = descriptor.source.clone().unwrap_or(HistorySource::Own);
                items.push(WindowPageItem {
                    source,
                    entry_id: descriptor.entry_id.clone(),
                    position: None,
                    oversize: true,
                    sub_items: Vec::new(),
                    descriptor: Some(descriptor.clone()),
                    descriptor_only: true,
                });
                synthesized += 1;
            }
            [index] => {
                let item = items.get_mut(*index)?;
                if item.descriptor.is_some() {
                    // 内联 body 是权威；顶层同身份条目只作兼容输入，冲突时忽略顶层。
                    continue;
                }
                item.descriptor = Some(descriptor.clone());
                item.oversize = true;
            }
            _ => return None,
        }
    }
    Some(synthesized)
}

/// 超大条目范围正文请求（服务端 `session/history/body` 实际字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryBodyRequest {
    pub entry_id: String,
    pub source: HistorySource,
    pub snapshot_token: String,
    pub read_ticket: String,
    pub offset: u64,
    pub max_bytes: u32,
}

impl HistoryBodyRequest {
    pub(crate) fn validate(&self) -> Result<(), WindowRequestError> {
        validate_token("entryId", &self.entry_id)?;
        validate_token("snapshotToken", &self.snapshot_token)?;
        validate_token("readTicket", &self.read_ticket)?;
        if !(1..=crate::kit::history_paging::window::WINDOW_MAX_CHUNK_BYTES)
            .contains(&self.max_bytes)
        {
            return Err(WindowRequestError::InvalidChunkBytes);
        }
        Ok(())
    }

    pub(crate) fn to_value(&self) -> Value {
        json!({
            "entryId": self.entry_id,
            "source": source_value(&self.source),
            "snapshotToken": self.snapshot_token,
            "readTicket": self.read_ticket,
            "offset": self.offset,
            "maxBytes": self.max_bytes,
        })
    }
}

pub const WINDOW_MAX_CHUNK_BYTES: u32 = 256 * 1024;

/// 页请求/响应身份：`sessionGeneration + structureVersion + snapshotToken +
/// windowGeneration + requestId`。任一项不匹配整页丢弃（I09）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowGeneration {
    pub session_id: String,
    pub session_generation: String,
    pub structure_version: u64,
    pub snapshot_token: String,
    pub window_generation: u64,
    pub request_id: String,
}

impl WindowGeneration {
    /// 同时校验 wire 身份与本地窗口代次。`requestId` 不在响应中，由调用方把待结算
    /// 请求与该身份一起保存并单独比对。
    pub(crate) fn accepts(
        &self,
        response: &WindowPageResponse,
        current_window_generation: u64,
    ) -> bool {
        self.window_generation == current_window_generation
            && self.session_id == response.session_id
            && self.session_generation == response.identity.session_generation
            && self.structure_version == response.identity.structure_version
            && self.snapshot_token == response.identity.snapshot_token
    }
}
