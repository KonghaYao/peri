//! 有界回退候选读取（本机 SQLite）。
//!
//! 两阶段、keyset 分批：
//!
//! 1. 只按 `thread_id` + `role = 'user'` 取身份与 `rowid`，`rowid DESC` 最新在前，
//!    每批最多 [`REWIND_CANDIDATE_SCAN_BATCH`] 行——Ai/Tool 行连身份都不返回，更不会
//!    进入正文解码。
//! 2. 只对命中的 user 行按 `message_id` 取正文；纯 `system-reminder` 行在累计前过滤，
//!    因此尾部注入不会挤掉有效候选（不足 `limit` 时继续下一批，直到累计够或耗尽）。
//!
//! `excluded` **不是**读取条件：被 Full Compact 归档的 user 消息仍是合法回退目标
//! （见 `requests_rewind_effective_view_test.rs`）。继承区 Human 条目在自有区不足时
//! 补在末尾，顺序与 [`super::SessionResources::load_session_history`] 的逆序口径一致。
//!
//! 阶段一/阶段二落在同一个只读事务里：候选身份与取回的正文来自同一快照。

use super::*;
use peri_acp_types::session_resources::{rewind_candidate, RewindCandidate};

/// 阶段一：user 行身份 + 存储序键，keyset 向更早方向，最多 `limit` 行。
///
/// 不取正文；`role = 'user'` 是领域常量（`canonical::role_of` 的稳定取值），不是用户输入。
pub(in crate::sessions) const REWIND_CANDIDATE_ROWS_SQL: &str = "SELECT message_id, rowid
    FROM messages
    WHERE thread_id = ?1 AND role = 'user' AND (?2 IS NULL OR rowid < ?2)
    ORDER BY rowid DESC LIMIT ?3";

/// 阶段二：按本批命中的 id 逐条取正文（主键单行）。
pub(in crate::sessions) const REWIND_CANDIDATE_BODY_SQL: &str =
    "SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2";

/// 阶段一每批读取的 user 行数：一次最多 128 条身份，不随历史总量增长。
const REWIND_CANDIDATE_SCAN_BATCH: i64 = 128;

impl SqliteSessionData {
    /// 有界读取回退候选；语义见 [`SessionResources::load_rewind_candidates`]。
    pub(super) async fn read_rewind_candidates(
        &self,
        id: &ThreadId,
        limit: usize,
    ) -> SessionResourceResult<Vec<RewindCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;

        let mut candidates = Vec::with_capacity(limit.min(REWIND_CANDIDATE_SCAN_BATCH as usize));
        let mut cursor: Option<i64> = None;
        'scan: loop {
            let rows: Vec<(String, i64)> = sqlx::query_as(REWIND_CANDIDATE_ROWS_SQL)
                .bind(id.as_str())
                .bind(cursor)
                .bind(REWIND_CANDIDATE_SCAN_BATCH)
                .fetch_all(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            if rows.is_empty() {
                break;
            }
            let exhausted = rows.len() < REWIND_CANDIDATE_SCAN_BATCH as usize;
            cursor = rows.last().map(|(_, rowid)| *rowid);

            for (message_id, _) in &rows {
                let content: Option<String> = sqlx::query_scalar(REWIND_CANDIDATE_BODY_SQL)
                    .bind(id.as_str())
                    .bind(message_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                let content = content.ok_or_else(|| corrupt("rewind candidate body is missing"))?;
                let payload = deserialize_persisted_payload(&content)
                    .map_err(|_| corrupt("rewind candidate body is not decodable"))?;
                // 行身份与正文身份必须一致：role 列只是读取条件，不能替代权威 id。
                if payload.id().as_uuid().to_string() != *message_id {
                    return Err(corrupt(
                        "rewind candidate id does not match its stored identity",
                    ));
                }
                if let Some(candidate) = rewind_candidate(&payload) {
                    candidates.push(candidate);
                    if candidates.len() == limit {
                        break 'scan;
                    }
                }
            }
            if exhausted {
                break;
            }
        }

        if candidates.len() < limit {
            let inherited = context::load_inherited_context_on(&mut tx, id)
                .await
                .map_err(read_failure)?;
            for payload in inherited.payloads.iter().rev() {
                if let Some(candidate) = rewind_candidate(payload) {
                    candidates.push(candidate);
                    if candidates.len() == limit {
                        break;
                    }
                }
            }
        }

        // 只读事务：没有写入效果可以证明，提交失败仍按原因分类。
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        Ok(candidates)
    }
}

#[cfg(test)]
#[path = "rewind_candidates_test.rs"]
mod tests;
