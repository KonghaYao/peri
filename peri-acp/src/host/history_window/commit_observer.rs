//! Full Compact 提交事实的即时发布。
//!
//! `CompactCompleted` 只表示“提交已发生、事件已产生”，不携带存储水位。窗口通知
//! 需要读取 Store 的精确边界并在 Host 侧发布，因此由宿主注入
//! [`StoreCompactCommitObserver`]；事件序列化路径本身不查询存储。
//!
//! 观察者只处理提交事实的发布；事件是否送达、窗口是否已打开都不影响 Store 的
//! durable 事实。读取失败时保留旧窗口元数据并记录日志，不把已提交的 turn 解释为
//! 失败。

use super::*;

/// Host 侧 Full Compact 提交观察者：在 `CompactCompleted` 产生时立即读取 Store
/// 的精确边界并通知已打开的窗口，不等待 prompt 收尾。
pub(crate) struct StoreCompactCommitObserver {
    sessions: SharedSessions,
    resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
}

impl StoreCompactCommitObserver {
    pub(crate) fn new(
        sessions: SharedSessions,
        resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    ) -> Self {
        Self {
            sessions,
            resources,
        }
    }
}

#[async_trait::async_trait]
impl crate::session::event_sink::CompactCommitObserver for StoreCompactCommitObserver {
    async fn on_compact_committed(&self, session_id: &str) {
        let Some((thread_id, generation, structure_version)) =
            commit_snapshot(&self.sessions, session_id).await
        else {
            return;
        };
        if let Err(error) = publish_committed_from_store(
            &self.sessions,
            self.resources.as_ref(),
            session_id,
            &thread_id,
            generation,
            structure_version,
        )
        .await
        {
            tracing::warn!(
                session_id,
                error = %error,
                "history window checkpoint publication after compact commit failed"
            );
        }
    }
}

/// 读取发布所需的三元组；rewind 在途或会话不存在时返回 None（不发布）。
pub(crate) async fn commit_snapshot(
    sessions: &SharedSessions,
    session_id: &str,
) -> Option<(String, u64, u64)> {
    let states = sessions.lock().await;
    let state = states.get(session_id)?;
    if state.history_window.rewind_in_progress {
        return None;
    }
    Some((
        state.thread_id.clone(),
        state.session_generation,
        state.structure_version,
    ))
}

/// 按已固定的 thread/generation/structure 读取 Store 边界并发布提交事实；
/// 发布成功后向当前活动窗口投递一次幂等通知。
///
/// 返回 `Ok(false)` 表示读取期间代次/结构已变化或进入 rewind，结果被丢弃。
pub(crate) async fn publish_committed_from_store(
    sessions: &SharedSessions,
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    session_id: &str,
    thread_id: &str,
    generation: u64,
    structure_version: u64,
) -> Result<bool, SessionResourceError> {
    let bounds = resources
        .history_window_bounds(&ThreadId::from(thread_id.to_owned()))
        .await?;
    let published = {
        let mut states = sessions.lock().await;
        let Some(state) = states.get_mut(session_id) else {
            return Ok(false);
        };
        if state.session_generation != generation
            || state.structure_version != structure_version
            || state.history_window.rewind_in_progress
        {
            tracing::debug!(
                session_id,
                "discarding compact commit metadata read across a concurrent mutation"
            );
            return Ok(false);
        }
        publish_commit(
            state,
            bounds.checkpoint.as_ref(),
            bounds.own_latest.as_ref(),
        );
        true
    };
    if published {
        notify_commit_if_open(sessions, session_id).await;
    }
    Ok(published)
}
