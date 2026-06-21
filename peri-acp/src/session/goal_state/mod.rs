//! GoalState — goal 子系统的并发状态机。
//!
//! 基于 `Arc<RwLock<GoalStateInner>>` + `parking_lot::RwLock`（短锁无 await）。
//! store 写入失败时退化为纯内存模式（snapshot 读仍可用），不阻塞 agent。
//!
//! 并发模型：read-and-reset + epoch（本 Task 先实现基础读写，account_progress 的
//! read-and-reset 在 Task 6 实现）。

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::goal::{GoalStatus, GoalStore, ThreadGoal};

/// Goal 快照（只读视图，供 middleware / TUI 读取）
#[derive(Debug, Clone, Default)]
pub struct GoalSnapshot {
    pub goal_id: Option<String>,
    pub objective: Option<String>,
    pub status: Option<GoalStatus>,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    /// set_goal / edit 后置 true，middleware 注入后清零
    pub objective_just_updated: bool,
}

impl GoalSnapshot {
    /// 是否有活跃的 goal
    pub fn has_active_goal(&self) -> bool {
        self.status == Some(GoalStatus::Active)
    }

    /// usage 百分比（0.0-1.0），budget=None 或 0 时返回 None
    pub fn usage_pct(&self) -> Option<f32> {
        self.token_budget
            .filter(|&b| b > 0)
            .map(|b| self.tokens_used as f32 / b as f32)
    }
}

/// 内部可变状态（受 RwLock 保护）
struct GoalStateInner {
    goal: Option<ThreadGoal>,
    /// set_goal / clear_goal 后置 true，GoalMiddleware 注入后清零
    objective_just_updated: bool,
    store: Arc<dyn GoalStore>,
    thread_id: String,
    /// 机制 3：continuation 期间用户消息缓冲（多条覆盖，只保留最后一条）
    pending_user_message: Option<String>,
    /// 待 flush 的 token 增量
    pending_token_delta: u64,
    /// 待 flush 的 time 增量（秒）
    pending_time_delta_seconds: u64,
}

/// 并发安全的状态句柄
#[derive(Clone)]
pub struct GoalState {
    inner: Arc<RwLock<GoalStateInner>>,
}

impl GoalState {
    pub fn new(store: Arc<dyn GoalStore>, thread_id: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(GoalStateInner {
                goal: None,
                objective_just_updated: false,
                store,
                thread_id,
                pending_user_message: None,
                pending_token_delta: 0,
                pending_time_delta_seconds: 0,
            })),
        }
    }

    /// set_goal：UPSERT（新 goal_id），触发 objective_updated。
    /// store 写入失败不回滚内存镜像（内存优于 store 原则）。
    pub async fn set_goal(
        &self,
        objective: String,
        token_budget: Option<u64>,
    ) -> Result<(), peri_agent::goal::GoalStoreError> {
        let new_goal = ThreadGoal::new(objective, token_budget);
        let (thread_id, store) = {
            let mut guard = self.inner.write();
            guard.goal = Some(new_goal.clone());
            guard.objective_just_updated = true;
            (guard.thread_id.clone(), guard.store.clone())
        };

        // 短锁：lock 在块作用域结束即释放，store.save 是长操作但不持锁
        if let Err(e) = store.save(&thread_id, new_goal).await {
            tracing::warn!(error = %e, "GoalState: store save 失败，退化为纯内存模式");
            return Err(e);
        }
        Ok(())
    }

    /// clear：清空 goal
    pub async fn clear(&self) -> Result<(), peri_agent::goal::GoalStoreError> {
        let (thread_id, store) = {
            let mut guard = self.inner.write();
            guard.goal = None;
            guard.objective_just_updated = false;
            guard.pending_user_message = None;
            (guard.thread_id.clone(), guard.store.clone())
        };

        if let Err(e) = store.delete(&thread_id).await {
            tracing::warn!(error = %e, "GoalState: store delete 失败");
            return Err(e);
        }
        Ok(())
    }

    /// set_status（简化封装，reason 为空字符串）
    pub async fn set_status(&self, target: GoalStatus) -> Result<(), String> {
        self.set_status_with_reason(target, String::new()).await
    }

    /// set_status 附带 reason（Blocked 必填）
    pub async fn set_status_with_reason(
        &self,
        target: GoalStatus,
        reason: String,
    ) -> Result<(), String> {
        let (thread_id, store, goal_clone) = {
            let mut guard = self.inner.write();
            let goal = guard
                .goal
                .as_mut()
                .ok_or_else(|| "无活跃 goal，无法 set_status".to_string())?;

            if !goal.status.can_transition_to(&target) {
                return Err(format!(
                    "非法状态转换: {:?} → {:?}（终态不可恢复）",
                    goal.status, target
                ));
            }

            // Blocked 必须附带 reason
            if matches!(target, GoalStatus::Blocked) && reason.trim().is_empty() {
                return Err("Blocked 状态必须附带 reason".to_string());
            }

            goal.status = target;
            goal.updated_at = chrono::Utc::now();
            if matches!(target, GoalStatus::Blocked) {
                goal.blocked_reason = Some(reason.clone());
            }
            let goal_clone = goal.clone();
            // 终态清零 pending_user_message（终态不需要用户消息）
            if target.is_terminal() {
                guard.pending_user_message = None;
            }
            (guard.thread_id.clone(), guard.store.clone(), goal_clone)
        };

        // best-effort store 写入（短锁已释放）
        let _ = store.save(&thread_id, goal_clone).await;
        Ok(())
    }

    /// 只读快照（短锁，立即释放）
    pub fn snapshot(&self) -> GoalSnapshot {
        let guard = self.inner.read();
        match &guard.goal {
            Some(g) => GoalSnapshot {
                goal_id: Some(g.goal_id.clone()),
                objective: Some(g.objective.clone()),
                status: Some(g.status),
                token_budget: g.token_budget,
                tokens_used: g.accounting.tokens_used,
                time_used_seconds: g.accounting.time_used_seconds,
                objective_just_updated: guard.objective_just_updated,
            },
            None => GoalSnapshot {
                objective_just_updated: guard.objective_just_updated,
                ..Default::default()
            },
        }
    }

    /// 消费 objective_just_updated 标志（middleware 注入后调用）
    pub fn consume_objective_updated(&self) -> bool {
        let mut guard = self.inner.write();
        let was_set = guard.objective_just_updated;
        guard.objective_just_updated = false;
        was_set
    }

    /// 机制 3：写入用户消息（覆盖旧值）
    pub fn put_pending_user_message(&self, message: String) {
        self.inner.write().pending_user_message = Some(message);
    }

    /// 机制 3：取出并清空用户消息
    pub fn take_pending_user_message(&self) -> Option<String> {
        self.inner.write().pending_user_message.take()
    }

    /// 记录 token 增量到 pending 缓冲
    pub fn record_token_usage(&self, delta: u64) {
        self.inner.write().pending_token_delta += delta;
    }

    /// 记录时间增量到 pending 缓冲
    pub fn record_time_usage(&self, delta_seconds: u64) {
        self.inner.write().pending_time_delta_seconds += delta_seconds;
    }

    /// flush：将 pending 增量累加到 goal.accounting 并写 store
    pub async fn flush_progress(&self) -> Result<(), String> {
        let (thread_id, store, goal_clone) = {
            let mut guard = self.inner.write();
            let token_delta = std::mem::take(&mut guard.pending_token_delta);
            let time_delta = std::mem::take(&mut guard.pending_time_delta_seconds);

            let goal = match guard.goal.as_mut() {
                Some(g) => g,
                None => return Ok(()), // 无 goal，no-op
            };

            goal.accounting.tokens_used += token_delta;
            goal.accounting.time_used_seconds += time_delta;
            goal.updated_at = chrono::Utc::now();

            let goal_clone = goal.clone();
            (guard.thread_id.clone(), guard.store.clone(), goal_clone)
        };

        // best-effort store 写入（短锁已释放）
        let _ = store.save(&thread_id, goal_clone).await;
        Ok(())
    }
}

impl peri_agent::goal::GoalStateView for GoalState {
    fn snapshot(&self) -> peri_agent::goal::GoalViewSnapshot {
        let snap = self.snapshot();
        peri_agent::goal::GoalViewSnapshot {
            objective: snap.objective,
            status: snap.status,
            token_budget: snap.token_budget,
            tokens_used: snap.tokens_used,
            objective_just_updated: snap.objective_just_updated,
        }
    }

    fn consume_objective_updated(&self) -> bool {
        self.consume_objective_updated()
    }
}

#[async_trait]
impl peri_agent::goal::GoalController for GoalState {
    async fn create_goal(&self, objective: String) -> Result<(), String> {
        // 原子化：检查 + 插入在同一写锁内，消除 TOCTOU 竞态窗口
        let new_goal = ThreadGoal::new(objective, None);
        let (thread_id, store) = {
            let mut guard = self.inner.write();
            if guard.goal.is_some() {
                return Err("goal 已存在，请先 clear 后重建".to_string());
            }
            guard.goal = Some(new_goal.clone());
            guard.objective_just_updated = true;
            (guard.thread_id.clone(), guard.store.clone())
        };
        // best-effort store 写入（短锁已释放）
        if let Err(e) = store.save(&thread_id, new_goal).await {
            tracing::warn!(error = %e, "GoalState: store save 失败，退化为纯内存模式");
            return Err(e.to_string());
        }
        Ok(())
    }

    async fn complete_goal(&self) -> Result<(), String> {
        self.set_status(GoalStatus::Complete).await
    }

    async fn block_goal(&self, reason: String) -> Result<(), String> {
        self.set_status_with_reason(GoalStatus::Blocked, reason)
            .await
    }

    fn snapshot(&self) -> peri_agent::goal::GoalViewSnapshot {
        peri_agent::goal::GoalStateView::snapshot(self)
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
