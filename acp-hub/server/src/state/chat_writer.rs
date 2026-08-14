//! doc 写入原语（§5.3 物理映射的执行层）。
//!
//! 所有函数在调用方事务内执行；**不做幂等/终态判定**（判定归聚合器），只保证
//! 「写出的结构合法」。每条原语幂等或由调用方保证幂等（§6.3）。
//!
//! 物理映射（§5.3/§5.4）：根对象/`entries`/`blocks`/`tool_calls` 用 `Y.Map`；
//! 顺序索引用 `Y.Array`（元素 `String`）；流式文本用 `Y.Text`；删除采用领域
//! tombstone，不由客户端物理删除权威记录。枚举值域按 schema 镜像的 serde
//! camelCase 字符串存储（`"completed"`/`"awaitingPermission"` 等）。

use yrs::{Array, Map, ReadTxn, Text, WriteTxn};

use acp_hub_proto::schema::{
    ActiveTurnProjection, BlockVisibility, ChatEntry, ContentBlock, EntryKind, EntryRole,
    EntryStatus, PublicError, ToolCallProjection, ToolCallStatus,
};

use crate::state::factory::ROOT;
use crate::state::view_store::TransactionCtx;

/// 内容块种类（`append_text_delta` 的目标块类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// 文本块（`ContentBlock::Text`，§5.3）。
    Text,
    /// 推理块（`ContentBlock::Reasoning`，§5.3）。
    Reasoning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEntryRegistration {
    Created,
    /// A legacy same-turn entry was safely linked to the Hub command that owns
    /// the persisted outbox turn.
    Correlated,
    Duplicate,
    SourceCommandConflict,
}

// ---------------------------------------------------------------------------
// 枚举值域（schema 镜像 serde camelCase 字符串）
// ---------------------------------------------------------------------------

pub(crate) fn entry_kind_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Message => "message",
        EntryKind::Tool => "tool",
        EntryKind::System => "system",
    }
}

pub(crate) fn entry_role_str(role: EntryRole) -> &'static str {
    match role {
        EntryRole::User => "user",
        EntryRole::Assistant => "assistant",
        EntryRole::System => "system",
    }
}

pub(crate) fn entry_status_str(status: EntryStatus) -> &'static str {
    match status {
        EntryStatus::Pending => "pending",
        EntryStatus::Streaming => "streaming",
        EntryStatus::Completed => "completed",
        EntryStatus::Cancelled => "cancelled",
        EntryStatus::Error => "error",
    }
}

pub(crate) fn tool_call_status_str(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::AwaitingPermission => "awaitingPermission",
        ToolCallStatus::Running => "running",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Error => "error",
        ToolCallStatus::Cancelled => "cancelled",
    }
}

pub(crate) fn visibility_str(v: BlockVisibility) -> &'static str {
    match v {
        BlockVisibility::Summary => "summary",
        BlockVisibility::Hidden => "hidden",
    }
}

// ---------------------------------------------------------------------------
// 读取辅助（聚合器判定用；只读，不写）
// ---------------------------------------------------------------------------

/// 根 MapRef（事务内获取；缺失时创建——root 恒存在，Factory 已补结构）。
pub fn root_map(txn: &mut TransactionCtx<'_>) -> yrs::MapRef {
    txn.get_or_insert_map(ROOT)
}

/// 只读根 MapRef（缺失返回 None；恢复/测试路径用）。
pub fn root_map_read<T: ReadTxn>(txn: &T) -> Option<yrs::MapRef> {
    txn.get_map(ROOT)
}

/// 读取 entry 是否存在（幂等键判定）。
pub fn entry_exists<T: ReadTxn>(txn: &T, entry_id: &str) -> bool {
    root_map_read(txn)
        .and_then(|root| root.get(txn, "entries"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|entries| entries.get(txn, entry_id).is_some())
        .unwrap_or(false)
}

/// 读取 tool_call 是否存在（关联检查/幂等键判定）。
pub fn tool_call_exists<T: ReadTxn>(txn: &T, tool_call_id: &str) -> bool {
    root_map_read(txn)
        .and_then(|root| root.get(txn, "tool_calls"))
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
        .map(|calls| calls.get(txn, tool_call_id).is_some())
        .unwrap_or(false)
}

/// 读取 tool_call 投影（聚合器 upsert 前读取以保留未覆盖字段）。
pub fn tool_call_projection<T: ReadTxn>(txn: &T, tool_call_id: &str) -> Option<ToolCallProjection> {
    let root = root_map_read(txn)?;
    let calls = root.get(txn, "tool_calls")?.cast::<yrs::MapRef>().ok()?;
    let map = calls.get(txn, tool_call_id)?.cast::<yrs::MapRef>().ok()?;
    Some(proj_from_map(txn, &map))
}

fn proj_from_map<T: ReadTxn>(txn: &T, map: &yrs::MapRef) -> ToolCallProjection {
    let str_or =
        |key: &str| -> Option<String> { map.get(txn, key).and_then(|v| v.cast::<String>().ok()) };
    ToolCallProjection {
        tool_call_id: str_or("tool_call_id").unwrap_or_default(),
        turn_id: str_or("turn_id").unwrap_or_default(),
        name: str_or("name").unwrap_or_default(),
        status: str_or("status")
            .as_deref()
            .map(status_from_str)
            .unwrap_or(ToolCallStatus::Pending),
        arguments: map
            .get(txn, "arguments")
            .and_then(out_any)
            .and_then(non_null_json),
        result: map
            .get(txn, "result")
            .and_then(out_any)
            .and_then(non_null_json),
        result_omitted: map
            .get(txn, "result_omitted")
            .and_then(|value| value.cast::<bool>().ok()),
        result_bytes: map
            .get(txn, "result_bytes")
            .and_then(|value| value.cast::<f64>().ok())
            .and_then(|value| (value >= 0.0 && value <= u64::MAX as f64).then_some(value as u64)),
        public_error: map
            .get(txn, "public_error")
            .and_then(|v| v.cast::<yrs::MapRef>().ok())
            .map(|error| PublicError {
                code: error
                    .get(txn, "code")
                    .and_then(|v| v.cast::<String>().ok())
                    .unwrap_or_default(),
                message: error
                    .get(txn, "message")
                    .and_then(|v| v.cast::<String>().ok())
                    .unwrap_or_default(),
            }),
        permission_id: str_or("permission_id"),
        started_at: str_or("started_at"),
        completed_at: str_or("completed_at"),
    }
}

/// `Out::Any` 提取（`Any` 未实现 `TryFrom<Out>`，需模式匹配）。
fn out_any(v: yrs::Out) -> Option<yrs::Any> {
    match v {
        yrs::Out::Any(a) => Some(a),
        _ => None,
    }
}

fn non_null_json(value: yrs::Any) -> Option<serde_json::Value> {
    (!matches!(value, yrs::Any::Null)).then(|| any_to_json(value))
}

fn status_from_str(s: &str) -> ToolCallStatus {
    match s {
        "pending" => ToolCallStatus::Pending,
        "awaitingPermission" => ToolCallStatus::AwaitingPermission,
        "running" => ToolCallStatus::Running,
        "completed" => ToolCallStatus::Completed,
        "error" => ToolCallStatus::Error,
        "cancelled" => ToolCallStatus::Cancelled,
        _ => ToolCallStatus::Pending,
    }
}

fn any_to_json(a: yrs::Any) -> serde_json::Value {
    let mut s = String::new();
    a.to_json(&mut s);
    serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)
}

/// 读取 entries MapRef（写入遍历/读取用）。
pub fn entries_map(txn: &mut TransactionCtx<'_>) -> yrs::MapRef {
    root_map(txn).get_or_init::<_, yrs::MapRef>(txn, "entries")
}

/// 读取 tool_calls MapRef。
pub fn tool_calls_map(txn: &mut TransactionCtx<'_>) -> yrs::MapRef {
    root_map(txn).get_or_init::<_, yrs::MapRef>(txn, "tool_calls")
}

/// 读取 entry_order ArrayRef。
pub fn entry_order_array(txn: &mut TransactionCtx<'_>) -> yrs::ArrayRef {
    root_map(txn).get_or_init::<_, yrs::ArrayRef>(txn, "entry_order")
}

// ---------------------------------------------------------------------------
// 写入原语（§7）
// ---------------------------------------------------------------------------

/// 确保 entry 存在（entry_id 幂等：已存在返回 false，不覆盖）。
///
/// `ChatEntry` 的 `blocks` 会以 `ContentBlock` 物理形态写入（Text/Reasoning
/// 块文本为 Y.Text）。
pub fn ensure_entry(txn: &mut TransactionCtx<'_>, root: &yrs::MapRef, entry: &ChatEntry) -> bool {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    if entries.get(txn, entry.entry_id.as_str()).is_some() {
        return false;
    }
    let entry_map = entries.insert(txn, entry.entry_id.as_str(), yrs::MapPrelim::default());
    entry_map.insert(txn, "entry_id", entry.entry_id.clone());
    match &entry.turn_id {
        Some(t) => entry_map.insert(txn, "turn_id", t.clone()),
        None => entry_map.insert(txn, "turn_id", yrs::Any::Null),
    };
    entry_map.insert(txn, "kind", entry_kind_str(entry.kind));
    entry_map.insert(txn, "role", entry_role_str(entry.role));
    entry_map.insert(txn, "status", entry_status_str(entry.status));
    match &entry.author_user_id {
        Some(u) => entry_map.insert(txn, "author_user_id", u.clone()),
        None => entry_map.insert(txn, "author_user_id", yrs::Any::Null),
    };
    match &entry.source_command_id {
        Some(command_id) => entry_map.insert(txn, "source_command_id", command_id.clone()),
        None => entry_map.insert(txn, "source_command_id", yrs::Any::Null),
    };
    entry_map.insert(txn, "created_at", entry.created_at.clone());
    match &entry.completed_at {
        Some(t) => entry_map.insert(txn, "completed_at", t.clone()),
        None => entry_map.insert(txn, "completed_at", yrs::Any::Null),
    };
    match &entry.error {
        Some(e) => write_public_error(txn, &entry_map, "error", e),
        None => {
            entry_map.insert(txn, "error", yrs::Any::Null);
        }
    };
    let block_order = entry_map.get_or_init::<_, yrs::ArrayRef>(txn, "block_order");
    for bid in &entry.block_order {
        block_order.push_back(txn, bid.clone());
    }
    let blocks = entry_map.get_or_init::<_, yrs::MapRef>(txn, "blocks");
    for (bid, block) in &entry.blocks {
        write_content_block(txn, &blocks, &block_order, bid, block);
    }
    let order = root.get_or_init::<_, yrs::ArrayRef>(txn, "entry_order");
    order.push_back(txn, entry.entry_id.clone());
    true
}

/// 创建 user entry（turn_id 幂等：同 turnId 已存在则跳过，§6.5「同 turnId 重放
/// 跳过」）。
#[allow(clippy::too_many_arguments)]
pub fn create_user_entry(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    turn_id: &str,
    entry_id: &str,
    text: &str,
    author_user_id: Option<&str>,
    source_command_id: Option<&str>,
    created_at: &str,
) -> UserEntryRegistration {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    // 同 turn_id 的 user entry 已存在。Hub command may backfill only an
    // absent identity; an existing different identity is never overwritten.
    if let Some(existing) = entries.iter(txn).find_map(|(_, v)| {
        v.cast::<yrs::MapRef>().ok().filter(|m| {
            m.get(txn, "role")
                .and_then(|r| r.cast::<String>().ok())
                .as_deref()
                == Some("user")
                && m.get(txn, "turn_id")
                    .and_then(|t| t.cast::<String>().ok())
                    .as_deref()
                    == Some(turn_id)
        })
    }) {
        return match source_command_id {
            None => UserEntryRegistration::Duplicate,
            Some(command_id) => match existing
                .get(txn, "source_command_id")
                .and_then(|value| value.cast::<String>().ok())
            {
                Some(current) if current == command_id => UserEntryRegistration::Duplicate,
                Some(_) => UserEntryRegistration::SourceCommandConflict,
                None => {
                    existing.insert(txn, "source_command_id", command_id.to_string());
                    UserEntryRegistration::Correlated
                }
            },
        };
    }
    let entry = ChatEntry {
        entry_id: entry_id.to_string(),
        turn_id: Some(turn_id.to_string()),
        kind: EntryKind::Message,
        role: EntryRole::User,
        status: EntryStatus::Completed,
        author_user_id: author_user_id.map(|s| s.to_string()),
        source_command_id: source_command_id.map(str::to_string),
        created_at: created_at.to_string(),
        completed_at: Some(created_at.to_string()),
        block_order: vec![format!("{entry_id}:text")],
        blocks: [(
            format!("{entry_id}:text"),
            ContentBlock::Text {
                block_id: format!("{entry_id}:text"),
                text: text.to_string(),
            },
        )]
        .into_iter()
        .collect(),
        error: None,
    };
    if ensure_entry(txn, root, &entry) {
        UserEntryRegistration::Created
    } else {
        UserEntryRegistration::Duplicate
    }
}

/// Create the prompt-delivery-v2 user entry before ACP dispatch. The body is
/// durable in the chat projection while the outbox retains only the matching
/// fingerprint. A replay is idempotent only when command, turn and fingerprint
/// all match; any mismatch fails closed.
#[allow(clippy::too_many_arguments)]
pub fn create_pending_prompt_entry(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    turn_id: &str,
    entry_id: &str,
    text: &str,
    author_user_id: Option<&str>,
    source_command_id: &str,
    payload_fingerprint: &str,
    created_at: &str,
) -> UserEntryRegistration {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    if let Some(existing) = entries.iter(txn).find_map(|(_, value)| {
        value.cast::<yrs::MapRef>().ok().filter(|map| {
            map.get(txn, "role")
                .and_then(|role| role.cast::<String>().ok())
                .as_deref()
                == Some("user")
                && map
                    .get(txn, "turn_id")
                    .and_then(|turn| turn.cast::<String>().ok())
                    .as_deref()
                    == Some(turn_id)
        })
    }) {
        let same_command = existing
            .get(txn, "source_command_id")
            .and_then(|value| value.cast::<String>().ok())
            .as_deref()
            == Some(source_command_id);
        let same_fingerprint = existing
            .get(txn, "payload_fingerprint")
            .and_then(|value| value.cast::<String>().ok())
            .as_deref()
            == Some(payload_fingerprint);
        return if same_command && same_fingerprint {
            UserEntryRegistration::Duplicate
        } else {
            UserEntryRegistration::SourceCommandConflict
        };
    }

    let entry = ChatEntry {
        entry_id: entry_id.to_string(),
        turn_id: Some(turn_id.to_string()),
        kind: EntryKind::Message,
        role: EntryRole::User,
        status: EntryStatus::Pending,
        author_user_id: author_user_id.map(str::to_string),
        source_command_id: Some(source_command_id.to_string()),
        created_at: created_at.to_string(),
        completed_at: None,
        block_order: vec![format!("{entry_id}:text")],
        blocks: [(
            format!("{entry_id}:text"),
            ContentBlock::Text {
                block_id: format!("{entry_id}:text"),
                text: text.to_string(),
            },
        )]
        .into_iter()
        .collect(),
        error: None,
    };
    if !ensure_entry(txn, root, &entry) {
        return UserEntryRegistration::Duplicate;
    }
    if let Some(entry_map) = entries
        .get(txn, entry_id)
        .and_then(|value| value.cast::<yrs::MapRef>().ok())
    {
        entry_map.insert(txn, "delivery_schema_version", 2_i64);
        entry_map.insert(txn, "delivery_state", "pending");
        entry_map.insert(txn, "delivery_error_code", yrs::Any::Null);
        entry_map.insert(txn, "payload_fingerprint", payload_fingerprint.to_string());
    }
    UserEntryRegistration::Created
}

/// Transition Hub-owned prompt delivery evidence on the existing user entry.
/// Returns false for duplicate state or non-v2/unknown entries.
pub fn set_prompt_entry_delivery(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    entry_id: &str,
    delivery_state: &str,
    delivery_error_code: Option<&str>,
    completed_at: Option<&str>,
) -> bool {
    if !matches!(
        delivery_state,
        "pending" | "completed" | "failed_not_delivered" | "delivery_unknown"
    ) {
        return false;
    }
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    let Some(entry_map) = entries
        .get(txn, entry_id)
        .and_then(|value| value.cast::<yrs::MapRef>().ok())
    else {
        return false;
    };
    let is_v2 = entry_map
        .get(txn, "delivery_schema_version")
        .and_then(|value| value.cast::<i64>().ok())
        == Some(2);
    if !is_v2 {
        return false;
    }
    let previous = entry_map
        .get(txn, "delivery_state")
        .and_then(|value| value.cast::<String>().ok());
    let previous_error = entry_map
        .get(txn, "delivery_error_code")
        .and_then(|value| value.cast::<String>().ok());
    if previous.as_deref() == Some(delivery_state)
        && previous_error.as_deref() == delivery_error_code
    {
        return false;
    }
    entry_map.insert(txn, "delivery_state", delivery_state.to_string());
    match delivery_error_code {
        Some(code) => entry_map.insert(txn, "delivery_error_code", code.to_string()),
        None => entry_map.insert(txn, "delivery_error_code", yrs::Any::Null),
    };
    if delivery_state == "completed" {
        entry_map.insert(txn, "status", entry_status_str(EntryStatus::Completed));
        match completed_at {
            Some(at) => entry_map.insert(txn, "completed_at", at.to_string()),
            None => entry_map.insert(txn, "completed_at", yrs::Any::Null),
        };
    } else if delivery_state == "failed_not_delivered" {
        entry_map.insert(txn, "status", entry_status_str(EntryStatus::Error));
        match completed_at {
            Some(at) => entry_map.insert(txn, "completed_at", at.to_string()),
            None => entry_map.insert(txn, "completed_at", yrs::Any::Null),
        };
    }
    true
}

/// 创建 assistant/system entry 骨架 + 首块（message_delta 的 entry 未知时由
/// 聚合器先建）。
pub fn ensure_entry_with_blocks(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    entry_id: &str,
    kind: EntryKind,
    role: EntryRole,
    turn_id: Option<&str>,
    created_at: &str,
) -> bool {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    if entries.get(txn, entry_id).is_some() {
        return false;
    }
    let entry_map = entries.insert(txn, entry_id, yrs::MapPrelim::default());
    entry_map.insert(txn, "entry_id", entry_id.to_string());
    match turn_id {
        Some(t) => entry_map.insert(txn, "turn_id", t.to_string()),
        None => entry_map.insert(txn, "turn_id", yrs::Any::Null),
    };
    entry_map.insert(txn, "kind", entry_kind_str(kind));
    entry_map.insert(txn, "role", entry_role_str(role));
    entry_map.insert(txn, "status", entry_status_str(EntryStatus::Pending));
    entry_map.insert(txn, "created_at", created_at.to_string());
    entry_map.insert(txn, "author_user_id", yrs::Any::Null);
    entry_map.insert(txn, "completed_at", yrs::Any::Null);
    entry_map.insert(txn, "error", yrs::Any::Null);
    entry_map.get_or_init::<_, yrs::ArrayRef>(txn, "block_order");
    entry_map.get_or_init::<_, yrs::MapRef>(txn, "blocks");
    let order = root.get_or_init::<_, yrs::ArrayRef>(txn, "entry_order");
    order.push_back(txn, entry_id.to_string());
    true
}

/// 追加内容块（block_id 幂等），返回块引用。
pub fn append_block(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    entry_id: &str,
    block: ContentBlock,
) -> bool {
    let Some(block_id) = block_id_of(&block) else {
        return false;
    };
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    let Some(entry_map) = entries
        .get(txn, entry_id)
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
    else {
        return false;
    };
    let blocks = entry_map.get_or_init::<_, yrs::MapRef>(txn, "blocks");
    if blocks.get(txn, &block_id).is_some() {
        return false;
    }
    let block_order = entry_map.get_or_init::<_, yrs::ArrayRef>(txn, "block_order");
    write_content_block(txn, &blocks, &block_order, &block_id, &block);
    true
}

fn block_id_of(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text { block_id, .. }
        | ContentBlock::Reasoning { block_id, .. }
        | ContentBlock::ToolCall { block_id, .. }
        | ContentBlock::Resource { block_id, .. } => Some(block_id.clone()),
    }
}

fn write_content_block(
    txn: &mut TransactionCtx<'_>,
    blocks: &yrs::MapRef,
    block_order: &yrs::ArrayRef,
    block_id: &str,
    block: &ContentBlock,
) {
    let bm = blocks.insert(txn, block_id, yrs::MapPrelim::default());
    match block {
        ContentBlock::Text { block_id, text } => {
            bm.insert(txn, "block_id", block_id.clone());
            bm.insert(txn, "kind", "text");
            bm.insert(txn, "text", yrs::TextPrelim::new(text.clone()));
        }
        ContentBlock::Reasoning {
            block_id,
            text,
            visibility,
        } => {
            bm.insert(txn, "block_id", block_id.clone());
            bm.insert(txn, "kind", "reasoning");
            bm.insert(txn, "text", yrs::TextPrelim::new(text.clone()));
            bm.insert(txn, "visibility", visibility_str(*visibility));
        }
        ContentBlock::ToolCall {
            block_id,
            tool_call_id,
        } => {
            bm.insert(txn, "block_id", block_id.clone());
            bm.insert(txn, "kind", "tool_call");
            bm.insert(txn, "tool_call_id", tool_call_id.clone());
        }
        ContentBlock::Resource {
            block_id,
            resource_id,
            media_type,
            name,
        } => {
            bm.insert(txn, "block_id", block_id.clone());
            bm.insert(txn, "kind", "resource");
            bm.insert(txn, "resource_id", resource_id.clone());
            bm.insert(txn, "media_type", media_type.clone());
            bm.insert(txn, "name", name.clone());
        }
    }
    block_order.push_back(txn, block_id.to_string());
}

/// 文本增量追加：block 不存在则先建（block_id 幂等），`Y.Text` insert（块
/// 尾部）。
///
/// 返回：新建块返回 `true`（首帧），追加已有块返回 `false`。
pub fn append_text_delta(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    entry_id: &str,
    block_id: &str,
    delta: &str,
    kind: ContentKind,
) -> bool {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    let Some(entry_map) = entries
        .get(txn, entry_id)
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
    else {
        return false;
    };
    let blocks = entry_map.get_or_init::<_, yrs::MapRef>(txn, "blocks");
    let block_order = entry_map.get_or_init::<_, yrs::ArrayRef>(txn, "block_order");
    let created = match blocks.get(txn, block_id) {
        Some(v) => {
            // 已有块：定位 Y.Text 追加。
            if let Some(text) = v.cast::<yrs::MapRef>().ok().and_then(|m| {
                m.get(txn, "text")
                    .and_then(|t| t.cast::<yrs::TextRef>().ok())
            }) {
                text.push(txn, delta);
            }
            false
        }
        None => {
            let bm = blocks.insert(txn, block_id, yrs::MapPrelim::default());
            bm.insert(txn, "block_id", block_id.to_string());
            match kind {
                ContentKind::Text => {
                    bm.insert(txn, "kind", "text");
                    bm.insert(txn, "text", yrs::TextPrelim::new(delta.to_string()));
                }
                ContentKind::Reasoning => {
                    bm.insert(txn, "kind", "reasoning");
                    bm.insert(txn, "text", yrs::TextPrelim::new(delta.to_string()));
                    // 默认可见性 summary；ReasoningDelta 后续经
                    // set_reasoning_visibility 覆盖。
                    bm.insert(txn, "visibility", "summary");
                }
            }
            block_order.push_back(txn, block_id.to_string());
            true
        }
    };
    // 首帧后 entry 置 streaming（有内容产出）。
    if created {
        entry_map.insert(txn, "status", entry_status_str(EntryStatus::Streaming));
    }
    created
}

/// reasoning 可见性设置（summary/hidden；hidden 绝不发给无权客户端，§5.3）。
/// 返回是否找到并更新。
pub fn set_reasoning_visibility(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    block_id: &str,
    visibility: BlockVisibility,
) -> bool {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    // 收集块所在 entry（先遍历收集再写入，避免 iter 借用与写借用冲突）。
    let target_entry = entries.iter(txn).find_map(|(_, v)| {
        v.cast::<yrs::MapRef>().ok().filter(|m| {
            m.get(txn, "blocks")
                .and_then(|b| b.cast::<yrs::MapRef>().ok())
                .map(|blocks| blocks.get(txn, block_id).is_some())
                .unwrap_or(false)
        })
    });
    match target_entry {
        Some(entry_map) => {
            let blocks = entry_map.get_or_init::<_, yrs::MapRef>(txn, "blocks");
            if let Some(bm) = blocks
                .get(txn, block_id)
                .and_then(|b| b.cast::<yrs::MapRef>().ok())
            {
                bm.insert(txn, "visibility", visibility_str(visibility));
                return true;
            }
            false
        }
        None => false,
    }
}

/// tool_call upsert（tool_call_id 幂等：存在则更新字段，不存在则创建）。
/// 返回是否新建。
pub fn upsert_tool_call(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    tc: &ToolCallProjection,
) -> bool {
    let calls = root.get_or_init::<_, yrs::MapRef>(txn, "tool_calls");
    let created = calls.get(txn, &tc.tool_call_id).is_none();
    let cm = calls.get_or_init::<_, yrs::MapRef>(txn, tc.tool_call_id.as_str());
    cm.insert(txn, "tool_call_id", tc.tool_call_id.clone());
    cm.insert(txn, "turn_id", tc.turn_id.clone());
    cm.insert(txn, "name", tc.name.clone());
    cm.insert(txn, "status", tool_call_status_str(tc.status));
    insert_opt_json(txn, &cm, "arguments", tc.arguments.as_ref());
    insert_opt_json(txn, &cm, "result", tc.result.as_ref());
    match tc.result_omitted {
        Some(value) => cm.insert(txn, "result_omitted", value),
        None => cm.insert(txn, "result_omitted", yrs::Any::Null),
    };
    match tc.result_bytes {
        Some(value) => cm.insert(txn, "result_bytes", value as f64),
        None => cm.insert(txn, "result_bytes", yrs::Any::Null),
    };
    match &tc.public_error {
        Some(e) => write_public_error(txn, &cm, "public_error", e),
        None => {
            cm.insert(txn, "public_error", yrs::Any::Null);
        }
    };
    match &tc.permission_id {
        Some(p) => cm.insert(txn, "permission_id", p.clone()),
        None => cm.insert(txn, "permission_id", yrs::Any::Null),
    };
    match &tc.started_at {
        Some(value) => cm.insert(txn, "started_at", value.clone()),
        None => cm.insert(txn, "started_at", yrs::Any::Null),
    };
    match &tc.completed_at {
        Some(value) => cm.insert(txn, "completed_at", value.clone()),
        None => cm.insert(txn, "completed_at", yrs::Any::Null),
    };
    created
}

/// Cancel every non-terminal tool belonging to a turn. Used only when the turn itself reaches
/// a cancellation/interruption terminal so cards cannot remain visually running forever.
pub fn cancel_nonterminal_tools_for_turn(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    turn_id: &str,
) -> usize {
    let calls = root.get_or_init::<_, yrs::MapRef>(txn, "tool_calls");
    let ids: Vec<String> = calls.iter(txn).map(|(id, _)| id.to_string()).collect();
    let mut migrated = 0;
    for id in ids {
        let Some(tool) = calls
            .get(txn, id.as_str())
            .and_then(|value| value.cast::<yrs::MapRef>().ok())
        else {
            continue;
        };
        let linked_turn = tool
            .get(txn, "turn_id")
            .and_then(|value| value.cast::<String>().ok());
        let status = tool
            .get(txn, "status")
            .and_then(|value| value.cast::<String>().ok())
            .unwrap_or_default();
        if linked_turn.as_deref() == Some(turn_id)
            && !matches!(status.as_str(), "completed" | "error" | "cancelled")
        {
            tool.insert(
                txn,
                "status",
                tool_call_status_str(ToolCallStatus::Cancelled),
            );
            migrated += 1;
        }
    }
    migrated
}

fn insert_opt_json(
    txn: &mut TransactionCtx<'_>,
    map: &yrs::MapRef,
    key: &str,
    v: Option<&serde_json::Value>,
) {
    match v {
        Some(value) => {
            let s = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
            let any = yrs::Any::from_json(&s).unwrap_or(yrs::Any::Null);
            map.insert(txn, key, yrs::In::from(any));
        }
        None => {
            map.insert(txn, key, yrs::Any::Null);
        }
    };
}

fn write_public_error(txn: &mut TransactionCtx<'_>, map: &yrs::MapRef, key: &str, e: &PublicError) {
    let em = map.insert(txn, key, yrs::MapPrelim::default());
    em.insert(txn, "code", e.code.clone());
    em.insert(txn, "message", e.message.clone());
}

/// entry 终态迁移（status/completed_at/error；Chat Doc 侧）。
/// 返回是否找到 entry 并迁移。
pub fn migrate_entry_terminal(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    entry_id: &str,
    status: EntryStatus,
    completed_at: &str,
    error: Option<&PublicError>,
) -> bool {
    let entries = root.get_or_init::<_, yrs::MapRef>(txn, "entries");
    let Some(entry_map) = entries
        .get(txn, entry_id)
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
    else {
        return false;
    };
    entry_map.insert(txn, "status", entry_status_str(status));
    entry_map.insert(txn, "completed_at", completed_at.to_string());
    match error {
        Some(e) => write_public_error(txn, &entry_map, "error", e),
        None => {
            entry_map.insert(txn, "error", yrs::Any::Null);
        }
    };
    true
}

/// active_turn 更新（Session Doc `session` map 内嵌字段；§7.2 权威投影）。
///
/// 对齐 Chat/Session 双 Doc：active turn 不是独立根键，而是 `session` map 的
/// `active_turn_id`/`active_turn_status`/`active_turn_updated_at` 三字段
/// （参考实现 Session Doc 形态）。`None` 清空三字段（终态后归位）。
/// 返回是否写入（值与既有不同或新增）。
pub fn set_active_turn(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    active: Option<&ActiveTurnProjection>,
) -> bool {
    let sm = root.get_or_init::<_, yrs::MapRef>(txn, "session");
    match active {
        Some(a) => {
            let changed = sm
                .get(txn, "active_turn_id")
                .and_then(|t| t.cast::<String>().ok())
                .as_deref()
                != Some(a.turn_id.as_str())
                || sm
                    .get(txn, "active_turn_status")
                    .and_then(|t| t.cast::<String>().ok())
                    != Some(turn_status_str(a.turn_status).to_string());
            if changed {
                sm.insert(txn, "active_turn_id", a.turn_id.clone());
                sm.insert(txn, "active_turn_status", turn_status_str(a.turn_status));
                sm.insert(txn, "active_turn_updated_at", a.updated_at.clone());
            }
            changed
        }
        None => {
            let had = sm.get(txn, "active_turn_id").is_some()
                || sm.get(txn, "active_turn_status").is_some();
            sm.remove(txn, "active_turn_id");
            sm.remove(txn, "active_turn_status");
            sm.remove(txn, "active_turn_updated_at");
            had
        }
    }
}

/// 条件更新 active_turn_status（Session Doc `session` map 内嵌字段）：
/// 现值 == `expect` 时写 `new`（§7.2 状态推进——awaitingPermission →
/// running/cancelled 只在状态仍为 awaitingPermission 时成立，避免
/// 覆盖后续终态）。返回是否写入。
pub fn set_active_turn_status_if(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    expect: &str,
    new: &str,
) -> bool {
    let sm = root.get_or_init::<_, yrs::MapRef>(txn, "session");
    let cur = sm
        .get(txn, "active_turn_status")
        .and_then(|s| s.cast::<String>().ok())
        .unwrap_or_default();
    if cur == expect {
        sm.insert(txn, "active_turn_status", new.to_string());
        true
    } else {
        false
    }
}

/// turn 状态字符串（§7.2 值域；camelCase 存储）。
pub(crate) fn turn_status_str(status: acp_hub_proto::schema::TurnStatus) -> &'static str {
    use acp_hub_proto::schema::TurnStatus;
    match status {
        TurnStatus::Accepting => "accepting",
        TurnStatus::Running => "running",
        TurnStatus::AwaitingPermission => "awaitingPermission",
        TurnStatus::Cancelling => "cancelling",
        TurnStatus::Completed => "completed",
        TurnStatus::Cancelled => "cancelled",
        TurnStatus::Interrupted => "interrupted",
        TurnStatus::Failed => "failed",
    }
}

/// projection_version += 1（每次成功投影 +1，§5.3/§5.6）。返回新版本。
pub fn bump_projection_version(txn: &mut TransactionCtx<'_>, root: &yrs::MapRef) -> u32 {
    let current = root
        .get(txn, "projection_version")
        .and_then(|v| v.cast::<u32>().ok())
        .unwrap_or(0);
    let next = current + 1;
    root.insert(txn, "projection_version", next);
    next
}

/// 读取当前 projection_version（只读）。
pub fn projection_version<T: ReadTxn>(txn: &T) -> u32 {
    root_map_read(txn)
        .and_then(|root| root.get(txn, "projection_version"))
        .and_then(|v| v.cast::<u32>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod source_command_tests {
    use super::*;
    use yrs::{Map, Transact};

    #[test]
    fn user_entry_source_command_backfill_is_exact_and_conflict_safe() {
        let doc = yrs::Doc::new();
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        assert_eq!(
            create_user_entry(&mut txn, &root, "t", "t:user", "same", None, None, "now"),
            UserEntryRegistration::Created
        );
        assert_eq!(
            create_user_entry(
                &mut txn,
                &root,
                "t",
                "t:user",
                "same",
                None,
                Some("cmd-1"),
                "now"
            ),
            UserEntryRegistration::Correlated
        );
        assert_eq!(
            create_user_entry(
                &mut txn,
                &root,
                "t",
                "t:user",
                "same",
                None,
                Some("cmd-1"),
                "now"
            ),
            UserEntryRegistration::Duplicate
        );
        assert_eq!(
            create_user_entry(
                &mut txn,
                &root,
                "t",
                "t:user",
                "same",
                None,
                Some("cmd-2"),
                "now"
            ),
            UserEntryRegistration::SourceCommandConflict
        );
        let entries = root
            .get(&txn, "entries")
            .unwrap()
            .cast::<yrs::MapRef>()
            .unwrap();
        let entry = entries
            .get(&txn, "t:user")
            .unwrap()
            .cast::<yrs::MapRef>()
            .unwrap();
        assert_eq!(
            entry
                .get(&txn, "source_command_id")
                .unwrap()
                .cast::<String>()
                .unwrap(),
            "cmd-1"
        );
    }
}
