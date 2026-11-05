//! 只读快照投影与显式 MQ 对账。
//!
//! `snapshot` 不查询、不修改记录；`refresh_from_store` / `hydrate` 才用 MQ
//! 查询结果补齐或清理运行期记录（`docs/design/thread-mq.md` §3.2/§4.2）。

use std::collections::HashSet;

use peri_acp_types::{
    event_v2::StateEvent,
    session::{
        MqEntry, MqFilter, UserInputQueueItem, UserInputQueueSnapshot, UserInputQueueTerminalItem,
        UserInputState,
    },
};

use super::state::{fingerprint_for, user_input_payload};
use super::window::prune_terminal_records;
use super::{InputRecord, MailboxState, SnapshotSeed, UserInputMailbox, UserInputQueueError};

/// 对账重试上限：并发变更持续时宁可跳过本轮，也不把旧查询内容配给新 revision。
const REFRESH_ATTEMPTS: usize = 3;

impl UserInputMailbox {
    /// 从 MQ 恢复尚未移交的用户输入；不 promote、不发送、不启动执行。
    ///
    /// ACP 在会话加载/构造后调用。重复调用按 ID 合并，保留既有运行期状态。
    pub async fn hydrate(&self) -> Result<(), UserInputQueueError> {
        self.reconcile_from_store(false).await
    }

    /// 显式对账：按 MQ 查询结果补齐缺失记录，并清理已确定不在 MQ 的 Queued 记录。
    ///
    /// 只在明确的状态同步入口调用；`snapshot` 本身不触发任何写入。查询失败
    /// 或查询期间 revision 变化时不修改记录，避免把未知当成确定不存在。
    pub async fn refresh_from_store(&self) -> Result<(), UserInputQueueError> {
        self.reconcile_from_store(true).await
    }

    async fn reconcile_from_store(&self, prune: bool) -> Result<(), UserInputQueueError> {
        for _ in 0..REFRESH_ATTEMPTS {
            // revision 与记录边界在同一把锁内取得，查询结果只能与这个窗口对齐。
            let (start_revision, boundary) = {
                let state = self.state.lock();
                (state.revision, state.next_seq)
            };
            let entries = self.query_user_inputs().await?;
            let mut state = self.state.lock();
            if state.revision != start_revision {
                continue;
            }
            if merge_store_entries(&mut state, &entries, boundary, prune) {
                state.revision += 1;
            }
            return Ok(());
        }
        // 状态持续变化时保留现有投影；下一次显式对账会重新尝试。
        tracing::warn!("user input store reconcile skipped: concurrent state changes");
        Ok(())
    }

    /// 纯内存投影：revision 与条目在同一把锁内取得。
    pub async fn snapshot(&self) -> UserInputQueueSnapshot {
        let state = self.state.lock();
        let items = state
            .records
            .iter()
            .filter(|record| is_projected(record.state))
            .map(|record| UserInputQueueItem {
                input_id: record.input_id.clone(),
                content: record.content.clone(),
                original_draft: record.original_draft.clone(),
                state: record.state,
            })
            .collect();
        let seed = snapshot_seed(&state);
        drop(state);
        assemble_snapshot(&self.session_id, &self.generation, seed, items)
    }

    pub(super) async fn query_user_inputs(&self) -> Result<Vec<MqEntry>, UserInputQueueError> {
        self.inbox
            .queue()
            .query(MqFilter {
                user_input_only: true,
                ..Default::default()
            })
            .await
            .map_err(|error| UserInputQueueError::Store(error.to_string()))
    }

    pub(super) fn publish(&self, snapshot: UserInputQueueSnapshot) {
        let ids: HashSet<_> = snapshot
            .terminal_items
            .iter()
            .map(|item| item.input_id.clone())
            .collect();
        (self.emit)(StateEvent::UserInputQueueChanged {
            turn_id: self.control_turn,
            agent_id: self.control_agent,
            snapshot,
        });
        // 先发布再允许淘汰；这里只保证进入发布边界，不保证慢客户端收到。
        // 并发发布期间窗口可暂时超过容量，避免尚未发布的新终态被挤掉。
        let mut state = self.state.lock();
        for item in &mut state.terminal_display {
            if ids.contains(&item.input_id) {
                item.published = true;
            }
        }
        prune_terminal_records(&mut state);
    }
}

/// 待发投影只包含尚未被 Receive 领取的输入；Claimed 已离开 MQ 待发区。
fn is_projected(state: UserInputState) -> bool {
    matches!(state, UserInputState::Queued | UserInputState::Dispatching)
}

/// 把一次成功查询合并进运行期记录；返回是否改变了可见投影。
fn merge_store_entries(
    state: &mut MailboxState,
    entries: &[MqEntry],
    boundary: u64,
    prune: bool,
) -> bool {
    let mut changed = false;
    let mut known = HashSet::with_capacity(entries.len());
    for entry in entries {
        let input_id = entry.id.as_uuid().to_string();
        known.insert(input_id.clone());
        let Some((content, original_draft)) = user_input_payload(entry) else {
            tracing::warn!(
                input_id = %entry.id.as_uuid(),
                "MQ user input row has a non-human payload; skipped during reconcile"
            );
            continue;
        };
        if state
            .records
            .iter()
            .any(|record| record.input_id == input_id)
            || state.terminal_identities.contains_key(&entry.id.as_uuid())
        {
            continue;
        }
        let seq = state.next_record_seq();
        let fingerprint = fingerprint_for(&input_id, &content, &original_draft);
        state.records.push(InputRecord::new(
            input_id,
            fingerprint,
            seq,
            content,
            original_draft,
        ));
        changed = true;
    }
    if prune {
        // MQ 行消失说明已被 Receive 消费或取消。只清理查询开始前已存在、
        // 且查询结果确定不存在的 Queued；查询期间新增的记录必须保留。
        let before = state.records.len();
        state.records.retain(|record| {
            record.state != UserInputState::Queued
                || record.seq >= boundary
                || known.contains(&record.input_id)
        });
        changed |= state.records.len() != before;
    }
    changed
}

fn snapshot_seed(state: &MailboxState) -> SnapshotSeed {
    SnapshotSeed {
        revision: state.revision,
        active_request_id: state
            .active
            .as_ref()
            .filter(|active| active.managed && active.cancel.is_some())
            .map(|active| active.ticket.id.clone()),
        // 终态不随 pending 项消失：客户端据此补齐丢失的交付确认。
        terminal_items: state
            .terminal_display
            .iter()
            .filter_map(|item| {
                let input_uuid = uuid::Uuid::parse_str(&item.input_id).ok()?;
                let identity = state.terminal_identities.get(&input_uuid)?;
                Some(UserInputQueueTerminalItem {
                    input_id: item.input_id.clone(),
                    state: identity.state,
                })
            })
            .collect(),
    }
}

fn assemble_snapshot(
    session_id: &str,
    generation: &str,
    seed: SnapshotSeed,
    items: Vec<UserInputQueueItem>,
) -> UserInputQueueSnapshot {
    UserInputQueueSnapshot {
        session_id: session_id.to_owned(),
        generation: generation.to_owned(),
        revision: seed.revision,
        active_request_id: seed.active_request_id,
        items,
        terminal_items: seed.terminal_items,
    }
}
