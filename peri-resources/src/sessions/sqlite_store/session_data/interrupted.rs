//! 进入根时的遗留 Active 候选读取（只读）。

use super::*;

impl SqliteSessionData {
    /// 根子树中非根、`active` 且 `idle`/`stopped` 的 thread；旧代际库零候选。
    pub(super) async fn interrupted_threads_impl(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<peri_acp_types::recovery::InterruptedThread>> {
        if !self.database.has_work_state_columns() {
            return Ok(Vec::new());
        }
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let root = match strict_thread_root_on(&mut connection, id).await {
            Ok(root) => root,
            Err(ThreadRootFailure::Missing) => return Err(corrupt("thread ancestry is missing")),
            Err(ThreadRootFailure::Cyclic) => return Err(corrupt("cyclic thread ancestry")),
            Err(ThreadRootFailure::Store(error)) => return Err(read_failure(error)),
        };
        let rows: Vec<(String, String)> = sqlx::query_as(INTERRUPTED_TREE_SQL)
            .bind(root.as_str())
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut candidates = Vec::with_capacity(rows.len());
        for (thread_id, state) in rows {
            let work_state = state
                .parse::<WorkState>()
                .map_err(|_| corrupt("stored work state is not a known value"))?;
            match work_state {
                WorkState::Idle | WorkState::Stopped => {
                    candidates.push(peri_acp_types::recovery::InterruptedThread {
                        thread_id,
                        work_state,
                    });
                }
                // SQL 已过滤；这里只防御未来投影漂移。
                WorkState::Working => continue,
            }
        }
        Ok(candidates)
    }
}
