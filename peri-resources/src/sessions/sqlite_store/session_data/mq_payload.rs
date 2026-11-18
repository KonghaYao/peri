//! MQ 行投影：把 `thread_mq` 行解码成领域条目，并按 [`MqFilter`] 构造读查询。
//!
//! 只做纯映射与 SQL 文本拼装，不开事务、不写库；写入顺序与事务边界见 `mq.rs`。

use super::SessionResourceResult;
use crate::sessions::mq_rows::RawMqRow;
use peri_acp_types::session::{MessageSource, MqEntry, MqFilter};

/// 单行读取的列（`thread_id` 在最前，用于跨 thread 判定）。
pub(super) type MqRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

/// 列表读取的列（不带 `thread_id`：查询本身就按它过滤）。
pub(super) type MqListRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

/// 控制面读取的列：没有 `payload_json`。
pub(super) type MqHeaderRow = (String, String, String, String, String);

/// 把 `MqFilter` 翻译成 SQL：过滤与上限都在数据库侧完成，正文查询不会先取全表。
///
/// 顺序键与 [`peri_acp_types::session::MqWatermark`] 一致：`(enqueued_at, id)` 字典序
/// 对固定格式时间与小写 UUID 就等于消费顺序。
pub(super) fn filtered_mq_select(
    columns: &str,
    filter: &MqFilter,
) -> SessionResourceResult<(String, Vec<String>)> {
    let mut sql = format!("SELECT {columns} FROM thread_mq WHERE thread_id = ?1");
    let mut binds: Vec<String> = Vec::new();
    if let Some(after) = &filter.after {
        sql.push_str(" AND (enqueued_at > ?2 OR (enqueued_at = ?2 AND id > ?3))");
        binds.push(after.enqueued_at.clone());
        binds.push(after.id.as_uuid().to_string());
    }
    if filter.user_input_only {
        let placeholder = binds.len() + 2;
        sql.push_str(&format!(" AND source = ?{placeholder}"));
        binds.push(crate::sessions::mq_rows::source_token(
            &MessageSource::UserInput,
        )?);
    }
    if !filter.sources.is_empty() {
        let placeholders = (0..filter.sources.len())
            .map(|offset| format!("?{}", binds.len() + offset + 2))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND source IN ({placeholders})"));
        for source in &filter.sources {
            binds.push(crate::sessions::mq_rows::source_token(source)?);
        }
    }
    if !filter.kinds.is_empty() {
        let placeholders = (0..filter.kinds.len())
            .map(|offset| format!("?{}", binds.len() + offset + 2))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" AND kind IN ({placeholders})"));
        for kind in &filter.kinds {
            binds.push(crate::sessions::mq_rows::kind_token(*kind)?);
        }
    }
    sql.push_str(" ORDER BY enqueued_at, id");
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    Ok((sql, binds))
}

pub(super) fn entry_of(row: &MqRow) -> SessionResourceResult<MqEntry> {
    crate::sessions::mq_rows::decode_entry(RawMqRow {
        id: &row.1,
        enqueued_at: &row.2,
        kind: &row.3,
        source: &row.4,
        policy_json: &row.5,
        payload_json: &row.6,
        original_draft: row.7.as_deref(),
    })
}

pub(super) fn listed_entry_of(row: &MqListRow) -> SessionResourceResult<MqEntry> {
    crate::sessions::mq_rows::decode_entry(RawMqRow {
        id: &row.0,
        enqueued_at: &row.1,
        kind: &row.2,
        source: &row.3,
        policy_json: &row.4,
        payload_json: &row.5,
        original_draft: row.6.as_deref(),
    })
}
