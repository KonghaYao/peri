//! Thread MQ 行的事实编解码、投影过滤与共用语句（本机 / 远端 adapter 同一份）。
//!
//! 两个 adapter 的差别只在**执行器**（事务 vs 托管批次），不在语义：行怎么解码、内容
//! 算不算相同、`(enqueued_at, id)` 怎么比较、投影过滤怎么应用，全部收在这里，避免
//! 「本机接受的内容远端判成冲突」这类分叉。
//!
//! 边界：本模块不写库、不开事务，只给 SQL 文本与纯函数。写入的顺序与事务边界由各 adapter
//! 自己保证（见 `docs/design/thread-mq.md` §5.4）。

use peri_acp_types::session::{
    MqEntry, MqFilter, MqReceiveItemOutcome, MqReceiveStatus, MqWatermark,
};
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::store::{deserialize_persisted_payload, serialize_persisted_payload};

use super::failure::corrupt;

/// 按 thread 读取全部 MQ 行：`ORDER BY (enqueued_at, id)` 就是消费顺序。
///
/// 投影过滤（来源 / 水位 / 上限）在 Rust 侧应用：两种执行器的 SQL 因此逐字相同，远端不需要
/// 为「来源集合」拼可变长度的 `IN (?, ?, ...)`。
pub(in crate::sessions) const SELECT_THREAD_MQ_SQL: &str =
    "SELECT id, enqueued_at, kind, source, policy_json, payload_json, original_draft
    FROM thread_mq WHERE thread_id = ?1 ORDER BY enqueued_at, id";

/// 按稳定 ID 读取单行（`UNIQUE(id)`，不带 thread 条件即可判定跨 thread 复用）。
pub(in crate::sessions) const SELECT_THREAD_MQ_BY_ID_SQL: &str =
    "SELECT thread_id, id, enqueued_at, kind, source, policy_json, payload_json, original_draft
    FROM thread_mq WHERE id = ?1";

/// 条件插入：历史或 MQ 已有同 ID、或目标 thread 不存在时不插入（影响 0 行）。
///
/// 与 Receive 竞争同一 ID 时，这条 `WHERE NOT EXISTS` 是唯一的裁决点——不能先查后插，
/// 两个语句之间会让重复接纳成立。
pub(in crate::sessions) const INSERT_THREAD_MQ_SQL: &str = "INSERT INTO thread_mq
    (thread_id, id, enqueued_at, kind, source, policy_json, payload_json, original_draft)
    SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
    WHERE NOT EXISTS (SELECT 1 FROM messages WHERE message_id = ?2)
      AND NOT EXISTS (SELECT 1 FROM thread_mq WHERE id = ?2)
      AND EXISTS (SELECT 1 FROM threads WHERE id = ?1)";

/// 按 `(thread_id, id, enqueued_at)` 删除：`enqueued_at` 参与条件，避免误删同 ID 重新入队的行。
pub(in crate::sessions) const DELETE_THREAD_MQ_ROW_SQL: &str =
    "DELETE FROM thread_mq WHERE thread_id = ?1 AND id = ?2 AND enqueued_at = ?3";

/// 消息历史里同 ID 的归属与正文（内容冲突判定用）。
pub(in crate::sessions) const SELECT_MESSAGE_ROW_SQL: &str =
    "SELECT thread_id, content FROM messages WHERE message_id = ?1";

/// 条件写入历史：同 ID 已存在时不插入（影响 0 行）。
pub(in crate::sessions) const INSERT_MESSAGE_SQL: &str =
    "INSERT INTO messages (message_id, thread_id, role, content)
    SELECT ?1, ?2, ?3, ?4
    WHERE NOT EXISTS (SELECT 1 FROM messages WHERE message_id = ?1)";

/// 远端移交用：正文直接取自 `thread_mq` 的同一行（远端批次读不回结果集）。
///
/// 条件与 [`INSERT_MESSAGE_SQL`] 相同（同 ID 已存在即不插入），额外要求 MQ 行的水位与选择
/// 一致——选择基于旧快照时不会把别的行写进历史。
pub(in crate::sessions) const INSERT_MESSAGE_FROM_MQ_SQL: &str =
    "INSERT INTO messages (message_id, thread_id, role, content)
    SELECT m.id, m.thread_id, ?4, m.payload_json FROM thread_mq m
    WHERE m.thread_id = ?1 AND m.id = ?2 AND m.enqueued_at = ?3
      AND NOT EXISTS (SELECT 1 FROM messages WHERE message_id = ?2)";

/// 派生计数与更新时间：只在历史条数真的变化时推进 `updated_at`。
///
/// 远端在批次里无法按「本批插入了几条」分支，因此把条件写成「计数是否变化」——它等价于
/// 「本次确实写了历史」，同时不会给 `AlreadyPresent`/`Conflict` 路径制造虚假的时间推进。
pub(in crate::sessions) const REFRESH_THREAD_COUNT_SQL: &str = "UPDATE threads SET updated_at = ?2,
        message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1)
    WHERE id = ?1
      AND message_count <> (SELECT COUNT(*) FROM messages WHERE thread_id = ?1)";

/// 首条用户消息派生标题：仅在标题为空时写入；`?2` 为 `NULL` 时是空操作。
pub(in crate::sessions) const DERIVE_THREAD_TITLE_SQL: &str =
    "UPDATE threads SET title = ?2 WHERE id = ?1 AND title IS NULL AND ?2 IS NOT NULL";

const ENQUEUED_AT_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6fZ";

/// 存储端生成入队时间：固定六位小数 + `Z`，重试不改写。
pub(in crate::sessions) fn enqueued_at_now() -> String {
    chrono::Utc::now().format(ENQUEUED_AT_FORMAT).to_string()
}

pub(in crate::sessions) fn policy_json(policy: &peri_acp_types::session::MessagePolicy) -> String {
    // `MessagePolicy` 全部字段都是可序列化的值类型，这里的失败只可能是构造错误。
    serde_json::to_string(policy).unwrap_or_else(|_| "{}".to_owned())
}

/// `MessageKind` 的序列化 token（`snake_case`，列里存的就是它）。
pub(in crate::sessions) fn kind_token(
    kind: peri_acp_types::session::MessageKind,
) -> SessionResourceResult<String> {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| corrupt("message kind is not representable"))
}

/// `MessageSource` 的序列化 token。
pub(in crate::sessions) fn source_token(
    source: &peri_acp_types::session::MessageSource,
) -> SessionResourceResult<String> {
    serde_json::to_value(source)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| corrupt("message source is not representable"))
}

pub(in crate::sessions) fn payload_json(
    payload: &peri_acp_types::store::PersistedPayload,
) -> SessionResourceResult<String> {
    serialize_persisted_payload(payload).map_err(|_| corrupt("MQ payload is not serializable"))
}

/// 一行 MQ 事实的原样文本形态（两种执行器读回后共用的形状）。
pub(in crate::sessions) struct RawMqRow<'a> {
    pub id: &'a str,
    pub enqueued_at: &'a str,
    pub kind: &'a str,
    pub source: &'a str,
    pub policy_json: &'a str,
    pub payload_json: &'a str,
    pub original_draft: Option<&'a str>,
}

/// 文本行 → 领域事实。任何一块解释不了都按损坏失败关闭：MQ 是待发事实的唯一来源，
/// 读不懂的行不能当成空队列或空正文。
pub(in crate::sessions) fn decode_entry(row: RawMqRow<'_>) -> SessionResourceResult<MqEntry> {
    let kind = serde_json::from_value(serde_json::Value::String(row.kind.to_owned()))
        .map_err(|_| corrupt("MQ row has an unknown message kind"))?;
    let source = serde_json::from_value(serde_json::Value::String(row.source.to_owned()))
        .map_err(|_| corrupt("MQ row has an unknown message source"))?;
    let policy = serde_json::from_str(row.policy_json)
        .map_err(|_| corrupt("MQ row has an unreadable message policy"))?;
    let payload = deserialize_persisted_payload(row.payload_json)
        .map_err(|_| corrupt("MQ row has an unreadable payload"))?;
    let id = row
        .id
        .parse::<uuid::Uuid>()
        .map(peri_acp_types::messages::MessageId::from)
        .map_err(|_| corrupt("MQ row has an invalid message id"))?;
    Ok(MqEntry {
        id,
        enqueued_at: row.enqueued_at.to_owned(),
        kind,
        source,
        payload,
        policy,
        original_draft: row.original_draft.map(str::to_owned),
    })
}

/// 投影过滤：输入必须已按 `(enqueued_at, id)` 升序（SQL 负责），这里只做收敛。
pub(in crate::sessions) fn filter_entries(
    entries: impl IntoIterator<Item = MqEntry>,
    filter: &MqFilter,
) -> Vec<MqEntry> {
    let limit = filter.limit.unwrap_or(usize::MAX);
    let mut selected = Vec::new();
    for entry in entries {
        if filter.user_input_only
            && entry.source != peri_acp_types::session::MessageSource::UserInput
        {
            continue;
        }
        if !filter.sources.is_empty() && !filter.sources.contains(&entry.source) {
            continue;
        }
        if let Some(after) = &filter.after {
            if entry.watermark() <= *after {
                continue;
            }
        }
        selected.push(entry);
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

/// 两条 payload 是否表示同一条消息（按规范化信封比较）。
///
/// 不逐字比字符串：legacy 行可能存的是未包裹的裸 `BaseMessage`，逐字比较会把「同一内容」
/// 判成冲突，从而删掉一条其实已经移交过的待发项。解不开的一侧一律视为不同内容——冲突路径
/// 删除 MQ 行且不写历史，宁可失败关闭也不重复追加。
pub(in crate::sessions) fn same_payload(left: &str, right: &str) -> bool {
    let normalize = |text: &str| {
        deserialize_persisted_payload(text)
            .ok()
            .and_then(|payload| serialize_persisted_payload(&payload).ok())
    };
    match (normalize(left), normalize(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// 把逐项判定收敛成结果；顺序与选择一致。
pub(in crate::sessions) fn item_outcome(
    id: peri_acp_types::messages::MessageId,
    status: MqReceiveStatus,
    entry: Option<MqEntry>,
) -> MqReceiveItemOutcome {
    MqReceiveItemOutcome { id, status, entry }
}

/// 水位比较：`(enqueued_at, id)` 字典序。
pub(in crate::sessions) fn is_same_watermark(entry: &MqEntry, watermark: &MqWatermark) -> bool {
    entry.enqueued_at == watermark.enqueued_at && entry.id == watermark.id
}

/// 首条 Human 消息的标题（与历史追加同一份派生规则）。
pub(in crate::sessions) fn title_of(entries: &[MqEntry]) -> Option<String> {
    let messages = entries
        .iter()
        .filter_map(|entry| entry.payload.as_message())
        .cloned()
        .collect::<Vec<_>>();
    super::canonical::extract_title(&messages)
}
