//! 交付确认日志与未领取输入的回收。

use std::collections::HashSet;

use peri_acp_types::{
    messages::{MessageContent, MessageId},
    session::UserInputState,
};

use super::state::matches_id;
use super::window::{
    prune_terminal_records, retain_terminal, PendingDeliveryNotification, DELIVERY_JOURNAL_CAPACITY,
};
use super::UserInputMailbox;

impl UserInputMailbox {
    /// 返回本次首次接纳的注册输入 ID，调用方在本轮 render FIFO 发聊天事件。
    ///
    /// 日志槽位在 enqueue 时已预留，因此这里不会因容量在 transcript 写入后
    /// 失败；预留随日志条目转移，泵确认后才释放。
    pub(crate) async fn mark_delivered(
        &self,
        inputs: &[(MessageId, MessageContent)],
    ) -> Vec<String> {
        self.mark_delivery_notifications(inputs).await
    }

    /// 事件泵的补发来源：按 Receive 实际交付顺序返回尚未推送的确认。
    ///
    /// 调用方必须先推送、再 [`Self::ack_delivery_notification`]；本方法只复制，
    /// 不持有锁等待 UI。进程内日志不持久化，重启后不承诺恢复展示。
    pub(crate) fn pending_delivery_notifications(&self) -> Vec<PendingDeliveryNotification> {
        self.state.lock().delivery_journal.iter().cloned().collect()
    }

    pub(crate) fn ack_delivery_notification(&self, input_id: &str) {
        let mut state = self.state.lock();
        if let Some(index) = state
            .delivery_journal
            .iter()
            .position(|item| item.input_id == input_id)
        {
            state.delivery_journal.remove(index);
        }
    }

    async fn mark_delivery_notifications(
        &self,
        inputs: &[(MessageId, MessageContent)],
    ) -> Vec<String> {
        let delivered = {
            let mut state = self.state.lock();
            if !state.valid {
                return Vec::new();
            }
            let mut delivered = Vec::new();
            {
                let super::MailboxState {
                    records,
                    delivered_notifications,
                    delivery_journal,
                    ..
                } = &mut *state;
                // Receive 的 ids 是 MQ 实际消费顺序，不可按 enqueue 的 records 顺序补确认。
                for (id, content) in inputs {
                    let Some(record) = records.iter_mut().find(|record| matches_id(record, &[*id]))
                    else {
                        continue;
                    };
                    if matches!(
                        record.state,
                        UserInputState::Claimed | UserInputState::Delivered
                    ) && delivered_notifications.insert(record.input_id.clone())
                    {
                        debug_assert!(
                            record.delivery_reserved,
                            "accepted input must hold a delivery journal slot"
                        );
                        if !record.delivery_reserved
                            && delivery_journal.len() >= DELIVERY_JOURNAL_CAPACITY
                        {
                            tracing::error!(
                                input_id = %record.input_id,
                                "delivery reservation invariant broken; journal temporarily over capacity"
                            );
                        }
                        record.state = UserInputState::Delivered;
                        record.delivery_reserved = false;
                        delivered.push(record.input_id.clone());
                        // 正文在记录清理之前进入有序日志：事件通道即使丢掉
                        // UserInputDelivered，日志条目仍会在本轮尾部补发，不会静默丢失。
                        delivery_journal.push_back(PendingDeliveryNotification {
                            input_id: record.input_id.clone(),
                            content: content.clone(),
                            session_seq: None,
                        });
                    }
                }
            }
            if delivered.is_empty() {
                return delivered;
            }
            state.revision += 1;
            for id in &delivered {
                retain_terminal(&mut state, id);
            }
            prune_terminal_records(&mut state);
            delivered
        };
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        delivered
    }

    /// 仍存在于 MQ 的 Dispatching 记录恢复 Queued；已移交的不恢复。
    pub(super) async fn reclaim(&self) {
        let ids = {
            let state = self.state.lock();
            if !state.valid {
                return;
            }
            let ids: Vec<String> = state
                .records
                .iter()
                .filter(|record| record.state == UserInputState::Dispatching)
                .map(|record| record.input_id.clone())
                .collect();
            ids
        };
        if ids.is_empty() {
            return;
        }
        let existing: HashSet<String> = match self.query_user_inputs().await {
            Ok(entries) => entries
                .iter()
                .map(|entry| entry.id.as_uuid().to_string())
                .collect(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "user input reclaim: MQ query failed; keeping dispatching runtime state"
                );
                return;
            }
        };
        let mut reclaimed = Vec::new();
        {
            let mut state = self.state.lock();
            let mut changed = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching
                    && (!record.handed_off || existing.contains(&record.input_id))
                {
                    record.state = UserInputState::Queued;
                    record.handed_off = false;
                    reclaimed.push(record.input_id.clone());
                    changed = true;
                }
            }
            state.ready.clear();
            if changed {
                state.revision += 1;
            }
        }
        let reclaimed_ids: Vec<MessageId> = reclaimed
            .iter()
            .filter_map(|id| uuid::Uuid::parse_str(id).ok().map(MessageId::from))
            .collect();
        if !reclaimed_ids.is_empty() {
            self.inbox.queue().withdraw_send_inputs(&reclaimed_ids);
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }
}
