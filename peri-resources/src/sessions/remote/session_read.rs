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

use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingState, FrozenSnapshotBytes, FrozenState, SessionResourceResult, SessionSnapshot,
};
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::thread::{ThreadId, ThreadListEntry, ThreadMeta};
use peri_acp_types::workspace::{
    ScopedThreadEntry, ScopedThreadPage, ScopedThreadQuery, SessionBinding, ThreadListCursor,
};
use turso_serverless::Value;

use super::mutation::sole_row;
use super::session_codec as codec;
use super::session_data::{not_found, RemoteSessionData};
use super::session_sql::{self, PAGE_BINDING_VERSION};
use super::sql::text_at;

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
        let meta = codec::decode_meta(&facts)?;
        let binding = codec::decode_binding(&facts, session_sql::FACT_BINDING_VERSION)?;
        let frozen = match text_at(&facts, session_sql::FACT_FROZEN) {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            // 远端没有 legacy 历史：没有快照就是没有，不猜来源。
            None => FrozenState::LegacyAbsent,
        };
        let inherited = codec::decode_inherited(text_at(&facts, session_sql::FACT_INHERITED))?;
        let (payloads, flags) = decode_history(&messages)?;
        Ok(SessionSnapshot {
            meta,
            binding: classify_binding(binding, &facts),
            frozen,
            payloads,
            flags,
            inherited,
        })
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

    /// 小型 metadata 投影（不含 frozen/继承区正文）。
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
        let limit = query.limit.clamp(1, 200) as usize;
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::page_statement(query)?)
            .await?;
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

/// 绑定列 + 父关系 → 绑定分类。
///
/// 「绑定的本机登记已不存在」在远端无从判断（登记只在本机）：远端只回答绑定事实是否
/// 完整存在，登记一致性与 legacy 判定留给门面按本机证据联合判定。
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
