//! 远程会话读取行为：一致快照、绑定分类、历史、metadata、分页与树。
//!
//! 读取不写、不改写、不发 DDL；`load_snapshot` 这类需要多段事实来自同一时刻的读取走
//! [`super::mutation::RemoteStore::read_batch`]（同一请求内的 `BEGIN DEFERRED` 只读事务），
//! 不把多次独立查询的结果拼成「一致快照」。回复形状不符（结果集或行数不对）在这里是
//! **错误**：不完整的读取绝不能降级成「这个会话没有历史」或「没有这一行」。
//!
//! 分类规则与本机 adapter 同一套（绑定行在/不在、子会话无绑定、无绑定无 legacy 证据），
//! 差别只有一处：远端没有本机 workspace 登记，因此 `LegacyConfirmed` 不在数据端冒充——
//! 它由门面按本机来源证据联合判定；远端只回答「绑定事实是否完整存在」。

use std::collections::HashMap;
use std::path::PathBuf;

use peri_acp_types::history_page::{
    message_id_of, resolve_browsing_range, HistoryCursor, HistoryCursorError, HistoryForwardCursor,
    HistoryPage, HistoryPageBounds, HistoryPageCandidate, HistoryPagePlan, HistoryPageRow,
    HistorySegment, HistorySegmentPlan,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, BindingState, FrozenSnapshotBytes, FrozenState, SessionResourceResult,
    SessionSnapshot,
};
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{ThreadId, ThreadListEntry, ThreadMeta};
use peri_acp_types::workspace::{
    ScopedThreadEntry, ScopedThreadPage, ScopedThreadQuery, SessionBinding, ThreadListCursor,
};
use turso_serverless::Value;

use super::mutation::{incomplete_reply, sole_row};
use super::session_codec as codec;
use super::session_data::{not_found, RemoteSessionData};
use super::session_sql::{self, PAGE_BINDING_VERSION};
use super::sql::{int_at, text_at};

impl RemoteSessionData {
    /// 一致读取：会话事实行与自有 payload 在同一只读事务里取出。
    pub(super) async fn read_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<SessionSnapshot> {
        let store = self.store().await?;
        let (facts, messages) = store
            .read_pair(
                session_sql::select_session_statement(id),
                session_sql::select_messages_statement(id),
            )
            .await?;
        let facts = sole_row(facts)?.ok_or_else(not_found)?;
        let facts = decode_session_facts(&facts)?;
        let (payloads, flags) = decode_history(&messages)?;
        Ok(SessionSnapshot {
            meta: facts.meta,
            binding: facts.binding,
            frozen: facts.frozen,
            payloads,
            flags,
            inherited: facts.inherited,
        })
    }

    /// 一致读取有效视图：与 [`Self::read_snapshot`] 同一次读取，但自有 payload 的读取
    /// 条件带 `excluded = 0`——已排除正文不出库，也不在这里二次过滤。
    pub(super) async fn read_active_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ActiveSessionSnapshot> {
        let store = self.store().await?;
        let (facts, messages) = store
            .read_pair(
                session_sql::select_session_statement(id),
                session_sql::select_active_messages_statement(id),
            )
            .await?;
        let facts = sole_row(facts)?.ok_or_else(not_found)?;
        let facts = decode_session_facts(&facts)?;
        let (payloads, flags) = decode_history(&messages)?;
        Ok(ActiveSessionSnapshot {
            meta: facts.meta,
            binding: facts.binding,
            frozen: facts.frozen,
            inherited: facts.inherited,
            payloads,
            flags,
        })
    }

    /// 轻量 frozen 读取：只问一列，不拉绑定、继承区或正文。
    pub(super) async fn read_frozen_state(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<FrozenState> {
        let store = self.store().await?;
        let mut batches = store
            .read_batch(vec![session_sql::select_frozen_statement(id.as_str())])
            .await?
            .into_iter();
        let rows = batches
            .next()
            .ok_or_else(|| incomplete_reply("expected 1 result set, got 0"))?;
        let row = sole_row(rows)?.ok_or_else(not_found)?;
        Ok(match text_at(&row, 0) {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            None => FrozenState::LegacyAbsent,
        })
    }

    /// 有界 flags 读取：只问给定的 id（一条语句一个 id，同一批次读出），
    /// 不做全历史扫描。回复形状不完整在 [`super::mutation::RemoteStore::read_batch`]
    /// 处已经是错误；行内 ID 与请求 ID 不一致同样按损坏失败。
    pub(super) async fn read_message_flags(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<HashMap<MessageId, MessageFlags>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.store().await?;
        let statements = ids
            .iter()
            .map(|message_id| {
                session_sql::select_message_flags_statement(
                    id.as_str(),
                    &message_id.as_uuid().to_string(),
                )
            })
            .collect();
        let results = store.read_batch(statements).await?;
        let mut flags = HashMap::new();
        for (requested, rows) in ids.iter().zip(results) {
            for row in &rows {
                let (row_id, row_flags) = codec::decode_flag_row(row)?;
                if row_id != *requested {
                    return Err(codec::corrupt("message flags row does not match its id"));
                }
                flags.insert(row_id, row_flags);
            }
        }
        Ok(flags)
    }

    /// 轻量绑定分类（只读会话事实行，不加载历史）。
    pub(super) async fn read_binding_state(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<BindingState> {
        let store = self.store().await?;
        let facts = store
            .fetch_row(&session_sql::select_session_statement(id))
            .await?
            .ok_or_else(not_found)?;
        let binding = codec::decode_binding(&facts, session_sql::FACT_BINDING_VERSION)?;
        Ok(classify_binding(binding, &facts))
    }

    /// 完整逻辑上下文：继承区在前、自有 payload 在后（继承快照是权威值，父会话之后的
    /// compact/rewind 不改变它）。
    pub(super) async fn read_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        let store = self.store().await?;
        let (facts, messages) = store
            .read_pair(
                session_sql::select_session_statement(id),
                session_sql::select_messages_statement(id),
            )
            .await?;
        let facts = sole_row(facts)?.ok_or_else(not_found)?;
        let inherited = codec::decode_inherited(text_at(&facts, session_sql::FACT_INHERITED))?;
        let (own, _) = decode_history(&messages)?;
        let mut payloads = inherited.payloads;
        payloads.extend(own);
        Ok(payloads)
    }

    /// 有界读取一页历史（更早方向），分两阶段：
    ///
    /// 1. 边界事实 + 候选行（身份 + 存储序键 + 正文长度，最多 `limit + 1` 行）走**同一次**
    ///    一致读取（`read_batch` 已保证结果集数量与请求一致，回复不完整即错误）。
    /// 2. 正文只按 [`HistoryPagePlan::body_ids`] 取：一条语句一个 id，同一批次读出，
    ///    因此多读的那一条候选行的正文**永不出库**（REQ-H03）。
    ///
    /// 阶段二单独一批是执行器的形状决定的：批次内的语句必须在发出请求**之前**就定好，
    /// 拿不到阶段一的输出。两阶段的固定范围由游标锚点（keyset）与**按 id 取值**保证：
    /// 阶段二只问「本页那些 id」，追加上来的行不影响它，被并发删掉/换掉的行在
    /// [`decode_page_bodies`] 处失败关闭，不会悄悄变成另一页。
    ///
    /// 与 SQLite adapter 共用同一份页规则（[`HistoryPagePlan`]），差别只有执行器。
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
        let anchor = cursor.map(|cursor| cursor.anchor.as_uuid().to_string());
        let store = self.store().await?;
        let mut batches = store
            .read_batch(vec![
                session_sql::history_page_bounds_statement(id.as_str(), anchor.as_deref()),
                session_sql::history_page_rows_statement(
                    id.as_str(),
                    anchor.as_deref(),
                    limit.saturating_add(1) as i64,
                ),
            ])
            .await?
            .into_iter();
        let bounds = batches
            .next()
            .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
        let rows = batches
            .next()
            .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
        // 会话不存在：边界语句一行都没有（与只读快照同一分类）。
        let bounds = sole_row(bounds)?.ok_or_else(not_found)?;
        // 锚点行在不在：rowid 只作存在性判据，游标身份始终是 message_id（游标身份规则见
        // [`resolve_browsing_range`]）。
        let (range_head, anchor_present) = decode_page_bounds(&bounds);
        let range_token = resolve_browsing_range(cursor, range_head, anchor_present)?;
        let candidates = rows
            .iter()
            .map(|row| decode_page_row(row))
            .collect::<SessionResourceResult<Vec<_>>>()?;
        let plan = HistoryPagePlan::plan(id, &range_token, candidates, limit, byte_budget);
        let body_ids = plan.body_ids();
        if body_ids.is_empty() {
            // 本页没有要推进的行（空会话或 limit 为 0）：阶段二没有可问的 id，
            // 不发空批次（空批次会被执行器当作形状不符）。
            return plan.materialize(Vec::new());
        }
        let statements = body_ids
            .iter()
            .map(|message_id| {
                session_sql::history_page_body_statement(
                    id.as_str(),
                    &message_id.as_uuid().to_string(),
                )
            })
            .collect();
        let bodies = store.read_batch(statements).await?;
        let rows = decode_page_bodies(&body_ids, bodies)?;
        plan.materialize(rows)
    }

    /// 有界读取**一段** canonical 完整历史（向前方向），分两阶段：
    ///
    /// 1. 边界事实 + 候选行（身份 + 存储序键 + 正文长度，最多 `limit + 1` 行，**升序**）
    ///    走**同一次**一致读取（`read_batch` 已保证结果集数量与请求一致，回复不完整即错误）；
    ///    首段还要多读一列继承区快照——它与上界、候选行同批，因此是同一个快照。
    /// 2. 正文只按 [`HistorySegmentPlan::body_ids`] 取：一条语句一个 id，同一批次读出，
    ///    因此多读的那一条候选行的正文**永不出库**。
    ///
    /// 游标（`after`/`tail`）是**数值**存储序键：两个后端绑定同一份语句文本，坏游标在发请求
    /// 之前就失败（形式校验见 [`HistoryForwardCursor::rowid_bounds`]），不会把文本形式的键
    /// 送进 SQL 变成一次没有意义的比较。
    ///
    /// 锚点行不在了即 `Expired`（锚点列只有存在性判据的用途）：不回退成首段，也不用空段
    /// 冒充回放结束。与 SQLite adapter 共用同一份段规则（[`HistorySegmentPlan`]），差别只有
    /// 执行器。
    pub(super) async fn read_history_segment(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryForwardCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistorySegment> {
        let bounds = cursor.map(HistoryForwardCursor::rowid_bounds).transpose()?;
        let store = self.store().await?;
        let (inherited, tail, candidates) = match bounds {
            None => {
                let mut batches = store
                    .read_batch(vec![
                        session_sql::history_segment_tail_statement(id.as_str()),
                        session_sql::history_segment_head_statement(
                            id.as_str(),
                            limit.saturating_add(1) as i64,
                        ),
                        session_sql::inherited_context_statement(id.as_str()),
                    ])
                    .await?
                    .into_iter();
                let tail = batches
                    .next()
                    .ok_or_else(|| incomplete_reply("expected 3 result sets, got 2"))?;
                let rows = batches
                    .next()
                    .ok_or_else(|| incomplete_reply("expected 3 result sets, got 2"))?;
                let inherited = batches
                    .next()
                    .ok_or_else(|| incomplete_reply("expected 3 result sets, got 2"))?;
                // 会话不存在：边界语句一行都没有（与只读快照同一分类）。
                let tail = sole_row(tail)?.ok_or_else(not_found)?;
                let tail = int_at(&tail, 0);
                let inherited = sole_row(inherited)?.ok_or_else(not_found)?;
                let inherited = codec::decode_inherited(text_at(&inherited, 0))?.payloads;
                let candidates = rows
                    .iter()
                    .map(|row| decode_page_row(row))
                    .collect::<SessionResourceResult<Vec<_>>>()?;
                (inherited, tail, candidates)
            }
            Some((after, tail)) => {
                let mut batches = store
                    .read_batch(vec![
                        session_sql::history_segment_anchor_statement(id.as_str(), after),
                        session_sql::history_segment_next_statement(
                            id.as_str(),
                            after,
                            tail,
                            limit.saturating_add(1) as i64,
                        ),
                    ])
                    .await?
                    .into_iter();
                let anchor = batches
                    .next()
                    .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
                let rows = batches
                    .next()
                    .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
                let anchor = sole_row(anchor)?.ok_or_else(not_found)?;
                if int_at(&anchor, 0).is_none() {
                    return Err(HistoryCursorError::Expired {
                        thread_id: id.clone(),
                    }
                    .into());
                }
                let candidates = rows
                    .iter()
                    .map(|row| decode_page_row(row))
                    .collect::<SessionResourceResult<Vec<_>>>()?;
                (Vec::new(), Some(tail), candidates)
            }
        };
        let plan = HistorySegmentPlan::plan(candidates, limit, byte_budget);
        let body_ids = plan.body_ids();
        if body_ids.is_empty() {
            // 本段没有要推进的行（空会话或 limit 为 0）：阶段二没有可问的 id，
            // 不发空批次（空批次会被执行器当作形状不符）。
            return plan.materialize(
                inherited,
                tail.map(|tail| tail.to_string()).as_deref(),
                Vec::new(),
            );
        }
        let statements = body_ids
            .iter()
            .map(|message_id| {
                session_sql::history_page_body_statement(
                    id.as_str(),
                    &message_id.as_uuid().to_string(),
                )
            })
            .collect();
        let bodies = store.read_batch(statements).await?;
        let rows = decode_page_bodies(&body_ids, bodies)?;
        plan.materialize(
            inherited,
            tail.map(|tail| tail.to_string()).as_deref(),
            rows,
        )
    }

    /// 一次有界单行读取：浏览范围边界（见 [`HistoryPageBounds`]）。
    ///
    /// 与 [`Self::read_history_page`] 共用同一条边界语句与同一份解码
    /// （[`decode_page_bounds`]）：「有没有更早内容」与页读取的起点在远端只有一份口径。
    /// 会话不存在时边界语句没有行（`NotFound`）；回复出现多行是**不完整回复**，不是「取第一行」。
    pub(super) async fn read_history_page_bounds(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<HistoryPageBounds> {
        let store = self.store().await?;
        // 锚点这一档固定为空：调用方还没有游标，也不在这里模拟一次带锚点的读取。
        let bounds = store
            .fetch_rows(&session_sql::history_page_bounds_statement(
                id.as_str(),
                None,
            ))
            .await?;
        let bounds = sole_row(bounds)?.ok_or_else(not_found)?;
        let (range_head, _) = decode_page_bounds(&bounds);
        HistoryPageBounds::new(range_head.as_deref())
    }

    /// 自有**有效**条目的存储序键（`message_id → rowid`）：一次有界读取，**不读正文**。
    ///
    /// 只服务初始 load 回放的位置戳（REQ-H01-3）：读取条件与有效视图一致（`excluded = 0`），
    /// 归档正文不会进入内存。会话不存在（没有行）与「会话没有自有条目」在存储上同形，
    /// 这里不区分——调用方（回放）只按 id 查表，两种情形都表现为「没有位置」。
    pub(super) async fn read_active_entry_positions(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<HashMap<MessageId, u64>> {
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::active_entry_positions_statement(id.as_str()))
            .await?;
        let mut positions = HashMap::with_capacity(rows.len());
        for row in &rows {
            let (message_id, position) = decode_entry_position(row)?;
            positions.insert(message_id, position);
        }
        Ok(positions)
    }

    /// 轻量 metadata 投影（不含 frozen/继承区正文）。
    pub(super) async fn read_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&session_sql::select_meta_statement(id))
            .await?
            .ok_or_else(not_found)?;
        codec::decode_meta(&row)
    }

    /// 分页列举：过滤（scope、hidden、消息数）与游标都在数据端完成。
    ///
    /// `effective_cwd` 给的是记录下来的创建目录，`workspace_root` 是 `None`：远端没有本机
    /// 登记，给不出已解析的根目录，也不拿别名顶替。要执行加载的调用方必须在本机重新校验。
    pub(super) async fn read_page(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.read_page_by_archive(query, false).await
    }

    pub(super) async fn read_page_by_archive(
        &self,
        query: &ScopedThreadQuery,
        archived: bool,
    ) -> SessionResourceResult<ScopedThreadPage> {
        // 压缩前的形状没有 `archived` 列：归档面在它上面恒为空，分页也走老形状的语句。
        if self.legacy_shape && archived {
            return Ok(ScopedThreadPage {
                entries: Vec::new(),
                next_cursor: None,
            });
        }
        let limit = query.limit.clamp(1, 200) as usize;
        let store = self.store().await?;
        let statement = if self.legacy_shape {
            session_sql::page_statement_v11(query)?
        } else {
            session_sql::page_statement_for_archive(query, archived)?
        };
        let rows = store.fetch_rows(&statement).await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            let meta = codec::decode_meta(row)?;
            let binding = codec::decode_binding(row, PAGE_BINDING_VERSION)?;
            let effective_cwd = PathBuf::from(&meta.cwd);
            entries.push(ScopedThreadEntry {
                thread: ThreadListEntry {
                    id: meta.id,
                    title: meta.title,
                    cwd: meta.cwd,
                    message_count: meta.message_count,
                    updated_at: meta.updated_at,
                },
                binding,
                effective_cwd,
                workspace_root: None,
            });
        }
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = if has_more {
            entries.last().map(|entry| ThreadListCursor {
                updated_at: entry.thread.updated_at,
                thread_id: entry.thread.id.clone(),
            })
        } else {
            None
        };
        Ok(ScopedThreadPage {
            entries,
            next_cursor,
        })
    }

    /// 直接子会话 metadata（按创建时间定序）。
    pub(super) async fn read_children(
        &self,
        parent: &ThreadId,
    ) -> SessionResourceResult<Vec<ThreadMeta>> {
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::select_children_statement(parent))
            .await?;
        rows.iter().map(|row| codec::decode_meta(row)).collect()
    }

    /// 以 `root` 为根的整棵树 metadata（含自身）。
    pub(super) async fn read_tree(
        &self,
        root: &ThreadId,
    ) -> SessionResourceResult<Vec<ThreadMeta>> {
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::select_tree_statement(root))
            .await?;
        rows.iter().map(|row| codec::decode_meta(row)).collect()
    }

    /// 会话是否存在（写入路径用它把「来源/父会话缺失」与「外键拒绝」分开报告）。
    pub(super) async fn exists(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let store = self.store().await?;
        Ok(store
            .fetch_row(&session_sql::select_meta_statement(id))
            .await?
            .is_some())
    }

    /// 已保存的 frozen 快照原文（child 必须逐字节复用 root 的那一份）。
    pub(super) async fn frozen_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        let store = self.store().await?;
        let facts = store
            .fetch_row(&session_sql::select_session_statement(id))
            .await?
            .ok_or_else(not_found)?;
        Ok(text_at(&facts, session_sql::FACT_FROZEN).map(str::to_owned))
    }

    /// 会话的不可变绑定（用于 child 与父会话绑定一致性核对）。
    pub(super) async fn binding_of(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<SessionBinding>> {
        let store = self.store().await?;
        let facts = store
            .fetch_row(&session_sql::select_session_statement(id))
            .await?
            .ok_or_else(not_found)?;
        codec::decode_binding(&facts, session_sql::FACT_BINDING_VERSION)
    }

    /// 沿父链上溯到根（一次递归查询，不在本层手动循环）。
    pub(super) async fn root_of(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&session_sql::select_root_statement(id))
            .await?;
        match row.as_deref().and_then(|values| text_at(values, 0)) {
            Some(root) => Ok(root.to_owned()),
            // 链上没有根：父行缺失或出现环，事实不完整，不猜一个根出来。
            None => Err(codec::corrupt("session parent chain has no root")),
        }
    }
}

/// 会话事实行 → 快照里的非历史部分。
struct SessionFacts {
    meta: ThreadMeta,
    binding: BindingState,
    frozen: FrozenState,
    inherited: InheritedContext,
}

/// 事实行的唯一解码口：完整快照与有效视图用同一份解读，避免两条读路径漂移。
fn decode_session_facts(facts: &[Value]) -> SessionResourceResult<SessionFacts> {
    Ok(SessionFacts {
        meta: codec::decode_meta(facts)?,
        binding: {
            let binding = codec::decode_binding(facts, session_sql::FACT_BINDING_VERSION)?;
            classify_binding(binding, facts)
        },
        frozen: match text_at(facts, session_sql::FACT_FROZEN) {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            // 远端没有 legacy 历史：没有快照就是没有，不猜来源。
            None => FrozenState::LegacyAbsent,
        },
        inherited: codec::decode_inherited(text_at(facts, session_sql::FACT_INHERITED))?,
    })
}

/// 消息行 → payload + 非默认 flags（flags 是派生视图，默认值不入映射）。
fn decode_history(
    rows: &[Vec<Value>],
) -> SessionResourceResult<(Vec<PersistedPayload>, HashMap<MessageId, MessageFlags>)> {
    let mut payloads = Vec::with_capacity(rows.len());
    let mut flags = HashMap::new();
    for row in rows {
        let payload = codec::decode_message_row(row)?;
        let row_flags = codec::decode_message_flags_row(row)?;
        if !codec::flags_are_default(&row_flags) {
            flags.insert(payload.id(), row_flags);
        }
        payloads.push(payload);
    }
    Ok((payloads, flags))
}

/// 一页候选行：`(message_id, content)` 两列，形状不符或缺列即损坏
/// （不猜、不用默认值顶替，也不静默丢行）。
/// 阶段一候选行的解码（**唯一**一处）：身份 + 存储序键 + 正文长度。
///
/// 三列都必须存在：少一列即回复形状不符——用默认值兜底会把「少读了一列」变成
/// 「这一页的长度未知」，页预算就失去了依据。
fn decode_page_row(row: &[Value]) -> SessionResourceResult<HistoryPageCandidate> {
    let message_id = text_at(row, 0).ok_or_else(|| codec::corrupt("history page row has no id"))?;
    let position =
        int_at(row, 1).ok_or_else(|| codec::corrupt("history page row has no position"))?;
    let encoded_bytes =
        int_at(row, 2).ok_or_else(|| codec::corrupt("history page row has no length"))?;
    HistoryPageCandidate::new(message_id, position, encoded_bytes)
}

/// 阶段二正文回复的解码：请求了几个 id 就要**恰好**几行，且行内正文与原 id 对应。
///
/// 一个 id 一行。`read_batch` 已在出口校验结果集数量；这里仍逐段核对「恰好一行」：少一行
/// 可能是并发删除，多一行意味着语句没有按 id 收敛。两者都不能降级成短页或取第一行。
fn decode_page_bodies(
    ids: &[MessageId],
    bodies: Vec<Vec<Vec<Value>>>,
) -> SessionResourceResult<Vec<HistoryPageRow>> {
    let mut rows = Vec::with_capacity(ids.len());
    for (message_id, batch) in ids.iter().zip(bodies) {
        let content =
            sole_row(batch)?.ok_or_else(|| codec::corrupt("history page body is missing"))?;
        let content = text_at(&content, 0)
            .ok_or_else(|| codec::corrupt("history page body has no content"))?;
        rows.push(HistoryPageRow::new(
            &message_id.as_uuid().to_string(),
            content.to_owned(),
        )?);
    }
    Ok(rows)
}

/// 存储序键一行的解码：id + `rowid` 两列都必须存在（少一列即回复形状不符）。
fn decode_entry_position(row: &[Value]) -> SessionResourceResult<(MessageId, u64)> {
    let message_id = text_at(row, 0).ok_or_else(|| codec::corrupt("history row has no id"))?;
    let position = int_at(row, 1).ok_or_else(|| codec::corrupt("history row has no position"))?;
    Ok((
        message_id_of(message_id)?,
        u64::try_from(position).map_err(|_| codec::corrupt("history row position is negative"))?,
    ))
}

/// 边界事实的解码（**唯一**一处）：浏览范围起点文本 + 锚点行存在性。
///
/// 页读取与边界读取共用它，两条路径不会各自解释一次边界行。锚点列只是存在性判据：
/// 缺失即 `false`，由调用方按游标规则判 `Expired`。
fn decode_page_bounds(bounds: &[Value]) -> (Option<String>, bool) {
    (
        text_at(bounds, 0).map(str::to_owned),
        int_at(bounds, 1).is_some(),
    )
}

/// 绑定列 + 父关系 → 绑定分类。
///
/// 远端只回答绑定事实是否完整存在。执行资格由门面读取远端不可变
/// 发现快照（root 用于定位执行目录），再复核归属关系与 cwd 可用性；
/// 不再跑 Git，也不比较文件对象身份。缺快照不改变绑定分类。
fn classify_binding(binding: Option<SessionBinding>, facts: &[Value]) -> BindingState {
    match binding {
        Some(binding) => BindingState::Bound(binding),
        // 子会话没有绑定行：与本机同一分类（不得当作 legacy 自动接纳）。
        None if text_at(facts, session_sql::META_PARENT).is_some() => {
            BindingState::ExternalOrUnregistered
        }
        None => BindingState::Missing,
    }
}
