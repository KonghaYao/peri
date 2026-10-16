use super::*;

// ─── 分页读取（archive 视图） ──────────────────────────────────────────────────

/// 浏览范围的边界事实（分页读取与初始 load 共用的**唯一**一条边界语句）：目标会话存在吗、
/// 浏览范围起点是谁、锚点行在不在。
///
/// 会话不存在时**没有行**（`NotFound`）；起点缺失为 NULL，锚点列只是**存在性判据**
/// （`NULL` = 锚点已不在）。刻意不写 `COALESCE(..., MAX(rowid) + 1)` 这类兜底：锚点消失
/// 必须失败关闭，不能悄悄把它变成「从最新一页开始」。
/// 计划证据要取的就是这条语句（见 `session_history_perf_test.rs`）：可见性提到 `crate::sessions`，
/// 让测试拿到的文本与生产执行的**同一条**，不复制 SQL。
pub(in crate::sessions) const HISTORY_PAGE_BOUNDS_SQL: &str = "SELECT
        (SELECT m.message_id FROM messages m WHERE m.thread_id = t.id ORDER BY m.rowid ASC LIMIT 1),
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.message_id = ?2)
    FROM threads t WHERE t.id = ?1";

/// 一页候选行（更早方向，含不可展示行）的**阶段一**读取：身份 + 存储序键 + 正文长度，
/// 最多 `limit + 1` 行，**不取正文**。
///
/// `?2` 为空 = 首屏，从最新一行开始——只有首屏允许这条路径：带游标时锚点行的 rowid 已由
/// [`HISTORY_PAGE_BOUNDS_SQL`] 证明存在，`None` 在那里已经变成失败。锚点用**子查询按
/// `message_id` 定位**（同一条语句内完成，两次语句之间不引入新的时间点），排序才用
/// `rowid`：删掉尾部行之后新插入的行会复用这些 rowid，rowid 不是稳定的条目身份。
///
/// 长度是**字节数**（`length(CAST(content AS BLOB))`；直接 `length(content)` 数的是字符，
/// 中文正文会偏小），与页预算和 `content.len()` 同一口径。`rowid` 只作为客户端可见的
/// 不透明位置返回（见 `HistoryPageItem::position`），不参与条目身份。
pub(in crate::sessions) const HISTORY_PAGE_ROWS_SQL: &str =
    "SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND (?2 IS NULL OR rowid < (
        SELECT m.rowid FROM messages m WHERE m.thread_id = ?1 AND m.message_id = ?2))
    ORDER BY rowid DESC LIMIT ?3";

/// 阶段二的正文读取语句原文（主键单行；计划证据与生产读取同一份文本）。
pub(in crate::sessions) const HISTORY_PAGE_BODY_SQL: &str =
    "SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2";

/// 自有有效条目位置读取的语句原文（计划证据与生产读取同一份文本）。
pub(in crate::sessions) const ACTIVE_ENTRY_POSITIONS_SQL: &str =
    "SELECT message_id, rowid FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid ASC";

// ─── 向前段读取（canonical 完整回放，REQ §5.3） ──────────────────────────────

/// 段读取的边界事实：会话存在吗 + 本次回放的固定上界（该线程自有行的最大 `rowid`，
/// 没有自有行时为 NULL）。
///
/// 与 [`HISTORY_PAGE_BOUNDS_SQL`] 同形（`threads` 单行 + 子查询）：会话不存在时**没有行**
/// （`NotFound`），存在性判定因此不需要额外一次读取。上界在这里读出来交给游标，行则由
/// [`HISTORY_SEGMENT_HEAD_SQL`] 在**同一条语句内**再算一次 `MAX(rowid)` 作谓词——两条语句
/// 落在同一个只读事务里，区间与行同源（不写 `COALESCE(MAX(rowid), …)` 这类兜底：没有自有行
/// 就是没有，回放本来就结束了）。
pub(in crate::sessions) const HISTORY_SEGMENT_TAIL_SQL: &str = "SELECT
        (SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = t.id)
    FROM threads t WHERE t.id = ?1";

/// 首段候选行（**阶段一**：身份 + 存储序键 + 正文长度，记忆同 [`HISTORY_PAGE_ROWS_SQL`]），
/// 最多 `limit + 1` 行，**升序**（canonical 完整回放方向），**不取正文**。
///
/// 与非首段同形，只是去掉 `rowid > ?2`，并把上界交给同一条语句内的 `MAX(rowid)` 子查询：
/// 首段的上界就是这一刻的最大 `rowid`，没有可以预先绑定的值（它由本次读取确定）。
pub(in crate::sessions) const HISTORY_SEGMENT_HEAD_SQL: &str =
    "SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND rowid <= (
        SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = ?1)
    ORDER BY rowid ASC LIMIT ?2";

/// 锚点存在性 + 会话存在性（一次单行读取）：`rowid = ?2` 的那一行就是游标的 `after`。
///
/// `NULL` = 锚点已不在（游标失效）；一条行都没有 = 会话不存在（`NotFound`）。刻意不写
/// `COALESCE(..., 0)` 这类兜底：锚点消失必须失败关闭，不能悄悄退化成一个从头开始的区间。
pub(in crate::sessions) const HISTORY_SEGMENT_ANCHOR_SQL: &str = "SELECT
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.rowid = ?2)
    FROM threads t WHERE t.id = ?1";

/// 非首段候选行：`rowid > after` 且 `rowid <= tail`，升序，最多 `limit + 1` 行。
///
/// `tail` 是**数值**上界，不是一条要查回来的行：回放期间追加的行 `rowid > tail`，因此不进入
/// 本次遍历；`tail` 那一行自己被删掉也不影响区间。
pub(in crate::sessions) const HISTORY_SEGMENT_NEXT_SQL: &str =
    "SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND rowid > ?2 AND rowid <= ?3
    ORDER BY rowid ASC LIMIT ?4";

impl SqliteSessionData {
    /// 边界事实的**唯一**读取：目标会话存在吗、浏览范围起点是谁、锚点行在不在。
    ///
    /// 分页读取与 [`Self::read_history_page_bounds`] 都走这里：浏览范围的起点在代码里只有一份
    /// 定义，两条路径不会各自演化成两种「起点」。锚点列（`Option<i64>`）只是**存在性判据**：
    /// 它不参与任何身份判定，游标身份始终是 `message_id`。
    ///
    /// 调用方必须已开事务并确认会话存在（[`Self::require_session`]），使这条单行读取与
    /// 它服务的候选行/回答落在同一时刻。
    async fn read_page_bounds(
        connection: &mut SqliteConnection,
        id: &ThreadId,
        anchor: Option<&str>,
    ) -> SessionResourceResult<(Option<String>, bool)> {
        let bounds: Option<(Option<String>, Option<i64>)> = sqlx::query_as(HISTORY_PAGE_BOUNDS_SQL)
            .bind(id.as_str())
            .bind(anchor)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 会话不存在时边界语句一行都没有；起点缺失为 NULL。
        let (range_head, anchor_rowid) = bounds.ok_or_else(not_found)?;
        Ok((range_head, anchor_rowid.is_some()))
    }

    /// 一次有界单行读取：浏览范围边界（见 [`HistoryPageBounds`]）。
    ///
    /// 回答「更早方向还有没有内容」（`range_head = None` 即没有）并给出初始游标要携带的
    /// 范围身份；与分页读取同一条边界语句、同一份解码。锚点这一档固定为空：调用方还没有
    /// 游标，也不该在这里模拟一次带锚点的读取。
    pub(super) async fn read_history_page_bounds(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<HistoryPageBounds> {
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
        let (range_head, _) = Self::read_page_bounds(&mut tx, id, None).await?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        HistoryPageBounds::new(range_head.as_deref())
    }

    /// 自有**有效**条目的存储序键（`message_id → rowid`）：一次有界读取，**不读正文**。
    ///
    /// 只服务初始 load 的回放：客户端要按位置把后到的页并进稀疏首屏（REQ-H01-3），
    /// 而回放手里的 payload 顺序没有存储序信息。读取条件与有效视图一致（`excluded = 0`），
    /// 因此不会把归档正文带进内存，也不会扫整条线程。
    ///
    /// 会话不存在时是 `NotFound`（与其余读取同规）；继承区条目不在其中——它们在本线程
    /// 没有行，缺席就是「不是自有条目」。
    pub(super) async fn read_active_entry_positions(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<std::collections::HashMap<MessageId, u64>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(ACTIVE_ENTRY_POSITIONS_SQL)
            .bind(id.as_str())
            .fetch_all(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        rows.into_iter()
            .map(|(message_id, position)| {
                Ok((
                    message_id_of(&message_id)?,
                    u64::try_from(position)
                        .map_err(|_| corrupt("history row position is negative"))?,
                ))
            })
            .collect()
    }

    /// 有界读取一页历史（更早方向）：边界事实、阶段一的候选行与阶段二的正文取自**同一个**
    /// 只读事务（REQ-H02「固定范围 + 变更校验」，不引入长事务，也不允许两阶段跨快照）。
    ///
    /// 阶段二按 [`HistoryPagePlan::body_ids`] 逐 id 取正文：多读的那一条候选行的正文
    /// **永远不出库**（REQ-H03）。回复行数或身份对不上即损坏，见
    /// [`Self::read_page_bodies`]。
    pub(super) async fn read_history_page(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistoryPage> {
        if let Some(cursor) = cursor {
            if cursor.thread_id != *id {
                return Err(HistoryCursorError::CrossSession {
                    expected: id.clone(),
                    cursor: cursor.thread_id.clone(),
                }
                .into());
            }
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
        let anchor_target = cursor.map(|cursor| cursor.anchor.as_uuid().to_string());
        let (range_head, anchor_present) =
            Self::read_page_bounds(&mut tx, id, anchor_target.as_deref()).await?;
        // 锚点行在不在：rowid 只作存在性判据，游标身份始终是 message_id（游标身份规则见
        // [`resolve_browsing_range`]）。
        let range_token = resolve_browsing_range(cursor, range_head, anchor_present)?;
        // 阶段一：`limit + 1` 是「多出的那一条候选行」，只用来判断更早方向还有没有内容。
        let candidates: Vec<(String, i64, i64)> = sqlx::query_as(HISTORY_PAGE_ROWS_SQL)
            .bind(id.as_str())
            .bind(anchor_target.as_deref())
            .bind(limit.saturating_add(1) as i64)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let candidates = candidates
            .into_iter()
            .map(|(message_id, position, encoded_bytes)| {
                HistoryPageCandidate::new(&message_id, position, encoded_bytes)
            })
            .collect::<SessionResourceResult<Vec<_>>>()?;
        let plan = HistoryPagePlan::plan(id, &range_token, candidates, limit, byte_budget);
        // 阶段二：只取本页那些行，同一事务（同一快照）。
        let rows = Self::read_page_bodies(&mut tx, id, &plan.body_ids()).await?;
        // 只读事务：没有写入效果可以证明，提交失败仍按原因分类。
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        plan.materialize(rows)
    }

    /// 有界读取**一段** canonical 完整历史（向前方向）：首段的继承区、固定上界与候选行取自
    /// **同一个**只读事务（REQ §5.3 的「`tail` 与返回的行同源」），阶段二只取本段那些行的正文。
    ///
    /// 与 [`Self::read_history_page`] 的差别只有方向与过滤规则：这里读完整历史（不过滤
    /// `excluded`、不过滤 System），阶段二则复用同一份 [`Self::read_page_bodies`]（同一条
    /// 主键单行语句），多读的那一条候选行的正文永远不出库。
    ///
    /// 锚点行不在了即 `Expired`（与分页读取的游标过期同一处置）：不回退成首段，也不用空段
    /// 冒充回放结束。
    pub(super) async fn read_history_segment(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryForwardCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistorySegment> {
        // 游标形式先在这里校验：文本不能直接与 `rowid` 比较，坏游标也不能降级成首段。
        let bounds = cursor.map(HistoryForwardCursor::rowid_bounds).transpose()?;
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 会话存在性由边界语句自己回答（与分页读取的边界语句同形），不另发一次探询。
        let (inherited, tail, candidates) = match bounds {
            None => {
                let tail: Option<i64> = sqlx::query_scalar(HISTORY_SEGMENT_TAIL_SQL)
                    .bind(id.as_str())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?
                    .ok_or_else(not_found)?;
                let rows: Vec<(String, i64, i64)> = sqlx::query_as(HISTORY_SEGMENT_HEAD_SQL)
                    .bind(id.as_str())
                    .bind(limit.saturating_add(1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                let inherited = context::load_inherited_context_on(&mut tx, id)
                    .await
                    .map_err(read_failure)?
                    .payloads;
                (inherited, tail, rows)
            }
            Some((after, tail)) => {
                let anchor: Option<Option<i64>> = sqlx::query_scalar(HISTORY_SEGMENT_ANCHOR_SQL)
                    .bind(id.as_str())
                    .bind(after)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                if anchor.ok_or_else(not_found)?.is_none() {
                    return Err(HistoryCursorError::Expired {
                        thread_id: id.clone(),
                    }
                    .into());
                }
                let rows: Vec<(String, i64, i64)> = sqlx::query_as(HISTORY_SEGMENT_NEXT_SQL)
                    .bind(id.as_str())
                    .bind(after)
                    .bind(tail)
                    .bind(limit.saturating_add(1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                (Vec::new(), Some(tail), rows)
            }
        };
        let candidates = candidates
            .into_iter()
            .map(|(message_id, position, encoded_bytes)| {
                HistoryPageCandidate::new(&message_id, position, encoded_bytes)
            })
            .collect::<SessionResourceResult<Vec<_>>>()?;
        let plan = HistorySegmentPlan::plan(candidates, limit, byte_budget);
        // 阶段二：只取本段那些行，同一事务（同一快照）。
        let rows = Self::read_page_bodies(&mut tx, id, &plan.body_ids()).await?;
        // 只读事务：没有写入效果可以证明，提交失败仍按原因分类。
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        plan.materialize(
            inherited,
            tail.map(|tail| tail.to_string()).as_deref(),
            rows,
        )
    }

    /// 阶段二：**只**取计划里那些行的正文（按 id 集合，逐 id 一条语句）。
    ///
    /// 与 [`Self::read_history_page`] 的调用方同一事务：两阶段必须落在同一个快照上，否则
    /// 「按长度定的页」与「取回的正文」可能来自两个时刻。每个 id 恰好一行、行内 id 必须
    /// 与请求一致，否则是损坏——不完整读取绝不能降级成「这一行没有正文」。
    async fn read_page_bodies(
        connection: &mut SqliteConnection,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<Vec<HistoryPageRow>> {
        let mut rows = Vec::with_capacity(ids.len());
        for message_id in ids {
            let expected = message_id.as_uuid().to_string();
            let row: Option<String> = sqlx::query_scalar(HISTORY_PAGE_BODY_SQL)
                .bind(id.as_str())
                .bind(&expected)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let content = row.ok_or_else(|| corrupt("history page body is missing"))?;
            rows.push(HistoryPageRow::new(&expected, content)?);
        }
        Ok(rows)
    }
}

impl SqliteSessionData {
    pub(super) async fn append_history_rows(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if payloads.is_empty() {
            return Ok(());
        }
        // 相同 ID 的碰撞不能静默忽略：批次内重复与库存重复都必须失败，
        // 否则「已存在但内容不同」会被当成成功。
        let mut seen = HashSet::with_capacity(payloads.len());
        for payload in payloads {
            if !seen.insert(payload.id()) {
                return Err(invalid_input("history batch repeats a message id"));
            }
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            // 目标会话不存在与「外键拒绝」是两个不同的原因，前者更可诊断。
            return Err(not_found());
        }
        for payload in payloads {
            sqlx::query(
                "INSERT INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(payload.id().as_uuid().to_string())
            .bind(id.as_str())
            .bind(payload_role(payload))
            .bind(
                serialize_persisted_payload(payload)
                    .map_err(|_| corrupt("history entry is not serializable"))?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        }
        let now = peri_time::now_utc_rfc3339();
        let updated = sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        let messages = payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(title) = extract_title(&messages) {
            sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                .bind(&title)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}

impl SqliteSessionData {
    pub(super) async fn rewind_stored_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        self.writable()?;
        let target = boundary.message_id().as_uuid().to_string();
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let rowid: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
                .bind(id.as_str())
                .bind(&target)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        // 未知截止点保持无变更语义：找不到目标就不动历史，也不清零 flags
        // （调用方的清零集合锚在同一份历史的目标上，目标已不在即整次操作失效）。
        // 但「没生效」必须如实回报：调用方若把它当成已生效，热态会被改写成
        // 存储里不存在的那份（见 `RewindOutcome`）。
        let outcome = match rowid {
            Some((rowid,)) => {
                let sql = match boundary {
                    RewindBoundary::KeepThrough(_) => {
                        "DELETE FROM messages WHERE thread_id = ?1 AND rowid > ?2"
                    }
                    RewindBoundary::RemoveFrom(_) => {
                        "DELETE FROM messages WHERE thread_id = ?1 AND rowid >= ?2"
                    }
                };
                sqlx::query(sql)
                    .bind(id.as_str())
                    .bind(rowid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                // 边界删除与有效视图重建同属这一事务：任何一半生效都会让热态与冷恢复分叉。
                clear_rewind_excluded(&mut tx, id, clear_excluded).await?;
                refresh_history_derivations(&mut tx, id).await?;
                RewindOutcome::Applied
            }
            None => RewindOutcome::BoundaryMissing,
        };
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(outcome)
    }

    pub(super) async fn remove_stored_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if ids.is_empty() {
            return Ok(());
        }
        let unique: Vec<MessageId> = {
            let mut seen = HashSet::with_capacity(ids.len());
            ids.iter().copied().filter(|id| seen.insert(*id)).collect()
        };
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        for message_id in &unique {
            // 别会话的条目不允许被「精确移除」静默命中或静默跳过。
            let owner: Option<(String,)> =
                sqlx::query_as("SELECT thread_id FROM messages WHERE message_id = ?1")
                    .bind(message_id.as_uuid().to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            match owner {
                Some((owner,)) if owner == id.as_str() => {
                    sqlx::query("DELETE FROM messages WHERE message_id = ?1 AND thread_id = ?2")
                        .bind(message_id.as_uuid().to_string())
                        .bind(id.as_str())
                        .execute(&mut *tx)
                        .await
                        .map_err(|error| map_sqlx(&error))?;
                }
                Some(_) => return Err(invalid_input("history entry belongs to another session")),
                // 已经不存在的条目是幂等删除，不产生错误。
                None => {}
            }
        }
        refresh_history_derivations(&mut tx, id).await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}

/// rewind 保留前缀的有效视图重建：把给定 id 的 `excluded` 清零，并**把整条 `projection`
/// 载荷一并清掉**（置 NULL）；`truncated` 保持原样。
///
/// 载荷必须随清零一起消失：`projection` 列是 flags 的持久化载荷，其中的 `superseded_by`
/// （`MessageProjectionDirective::superseded_by`）只在 `excluded` 为真时被读取——一条正文已
/// 恢复的条目却留着「我被某摘要取代」的记录，下游读到它就会拿到一份与有效视图矛盾的投影
/// 指令。清掉它不丢任何仍然有效的渲染信息：生产里 `excluded: true` 的唯一写入点是
/// Full Compact（`peri-agent/src/agent/compact_v2/full.rs`），它写 flags 时用
/// `..Default::default()` **整体覆盖**，所以被排除的行本来就不带渲染指令。
///
/// 占位符个数随 id 数量生成，值全部走绑定参数（语句里不出现任何 id 文本）；语句在调用方
/// 的同一事务内执行。任一 id 不是本会话的历史条目即整体失败：清零集合来自同一次一致读取，
/// 命中不到行说明视图已与那次读取分叉，不能静默留下「一半恢复」的有效视图。
async fn clear_rewind_excluded(
    tx: &mut SqliteConnection,
    id: &ThreadId,
    clear_excluded: &[MessageId],
) -> SessionResourceResult<()> {
    if clear_excluded.is_empty() {
        return Ok(());
    }
    let unique: Vec<MessageId> = {
        let mut seen = HashSet::with_capacity(clear_excluded.len());
        clear_excluded
            .iter()
            .copied()
            .filter(|message| seen.insert(*message))
            .collect()
    };
    let placeholders = (2..=unique.len() + 1)
        .map(|position| format!("?{position}"))
        .collect::<Vec<_>>()
        .join(", ");
    // `projection = NULL`：整条载荷随清零一并消失，不留 `superseded_by` 这类只在 `excluded`
    // 为真时才有意义的记录（理由见函数文档）。`truncated` 不在 SET 里，保持原值。
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE messages SET excluded = 0, projection = NULL \
             WHERE thread_id = ?1 AND message_id IN ({placeholders})"
    )))
    .bind(id.as_str());
    for message in &unique {
        query = query.bind(message.as_uuid().to_string());
    }
    let updated = query
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
    if updated.rows_affected() != unique.len() as u64 {
        return Err(invalid_input(
            "rewind flag target is not a history entry of this session",
        ));
    }
    Ok(())
}
