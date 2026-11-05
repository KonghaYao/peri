//! 远端恢复范围读取：严格解析根，再取根子树的 working 候选。

use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::thread::ThreadId;

use super::super::session_codec;
use super::super::{session_sql, sql};
use super::RemoteSessionData;

impl RemoteSessionData {
    pub(super) async fn read_recovery_scope(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::recovery::RecoveryScope> {
        // 与本地 adapter 同语义：先严格解析根（远端不退回自身），再读根子树的 working 候选。
        let root = self.root_of(id).await?;
        // 旧形状（v10/v11 只读）没有两列工作快照：读不到工作意图就是零候选，不能猜成 working。
        if self.legacy_shape {
            return Ok(peri_acp_types::recovery::RecoveryScope::idle(root));
        }
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::select_working_tree_statement(&root))
            .await?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let thread_id = sql::text_at(&row, 0)
                .ok_or_else(|| session_codec::corrupt("recovery scope row has no thread id"))?
                .to_owned();
            let parent_thread_id = sql::text_at(&row, 1).map(str::to_owned);
            let context_usage_tokens = match sql::int_at(&row, 2) {
                None => None,
                Some(usage) => Some(u64::try_from(usage).map_err(|_| {
                    session_codec::corrupt("recovery scope row has a negative context usage")
                })?),
            };
            let state = sql::text_at(&row, 3)
                .ok_or_else(|| session_codec::corrupt("recovery scope row has no work state"))?;
            let state = state
                .parse::<peri_acp_types::thread::WorkState>()
                .map_err(|_| session_codec::corrupt("stored work state is not a known value"))?;
            if state != peri_acp_types::thread::WorkState::Working {
                continue;
            }
            candidates.push(peri_acp_types::recovery::RecoveryCandidate {
                thread_id,
                parent_thread_id,
                context_usage_tokens,
            });
        }
        Ok(peri_acp_types::recovery::RecoveryScope {
            root_thread_id: root,
            candidates,
        })
    }

    pub(super) async fn read_interrupted_threads(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<peri_acp_types::recovery::InterruptedThread>> {
        // 旧形状没有工作快照列：读不到工作意图就是零候选，不能猜成 idle。
        if self.legacy_shape {
            return Ok(Vec::new());
        }
        let root = self.root_of(id).await?;
        let store = self.store().await?;
        let rows = store
            .fetch_rows(&session_sql::select_interrupted_tree_statement(&root))
            .await?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let thread_id = sql::text_at(&row, 0)
                .ok_or_else(|| session_codec::corrupt("interrupted row has no thread id"))?
                .to_owned();
            let state = sql::text_at(&row, 1)
                .ok_or_else(|| session_codec::corrupt("interrupted row has no work state"))?;
            let work_state = state
                .parse::<peri_acp_types::thread::WorkState>()
                .map_err(|_| session_codec::corrupt("stored work state is not a known value"))?;
            match work_state {
                peri_acp_types::thread::WorkState::Idle
                | peri_acp_types::thread::WorkState::Stopped => {
                    candidates.push(peri_acp_types::recovery::InterruptedThread {
                        thread_id,
                        work_state,
                    });
                }
                peri_acp_types::thread::WorkState::Working => continue,
            }
        }
        Ok(candidates)
    }
}
