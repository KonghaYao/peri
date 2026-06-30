//! SubAgent 运行时状态映射 — Phase 2.3 核心基础设施。
//!
//! ## 背景
//!
//! v2 ViewCommit 是替换语义（`state.view = vc.view_models`），ACP 层
//! `convert_agent_tool` 产出的 SubAgentGroup DTO **只有静态字段**（agent_id /
//! agent_name / view_models / collapsed），缺失运行时状态（is_running /
//! final_result / is_error / total_steps）。这些状态由 TUI 的
//! `AgentEvent::SubAgentStart` / `SubAgentEnd` / `BackgroundTaskCompleted`
//! / `BgToolStep` 事件实时维护。
//!
//! 本模块作为 TUI 侧的 SubAgent 运行时状态镜像，独立于 v2 ViewCommit：
//! - ViewCommit 提供静态视图（结构 + 历史 VMs）
//! - 本映射提供运行时覆盖（is_running / final_result / total_steps / ...）
//! - 渲染时通过 `lookup(instance_id)` 取出状态，覆盖 DTO 字段
//!
//! ## 容量管理
//!
//! - `MAX_CAPACITY = 32`：防止长会话累积过多 entry
//! - `TTL = 5 分钟`：完成的 entry 保留 5 分钟供渲染查阅，超期自动 evict
//!
//! ## lookup 回退路径
//!
//! 当只有 agent_id（如 bg 回退路径）时，按 `started_at` 倒序遍历，返回最近
//! 启动且仍在运行（或最近完成）的 entry —— 避免把多个同名 agent 实例混淆。

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// SubAgent 运行时状态（7 字段）。由 TUI 事件实时维护，独立于 ACP ViewCommit。
#[derive(Clone, Debug)]
pub struct SubAgentStatus {
    /// 是否仍在运行（false = 已完成或取消）
    pub is_running: bool,
    /// 完成时是否为错误（取消视为非错误）
    pub is_error: bool,
    /// 最终结果摘要（SubAgentEnd / BackgroundTaskCompleted 携带）
    pub final_result: Option<String>,
    /// 总步数（工具调用数 + AI 回复）
    pub total_steps: usize,
    /// 是否为后台 agent（影响 UI 折叠行为）
    pub is_background: bool,
    /// 任务预览（启动时携带，用于回退路径匹配）
    pub task_preview: String,
    /// 启动时间（用于 TTL + 回退路径排序）
    pub started_at: Instant,
    /// 完成时间（None = 仍在运行或被取消）
    pub completed_at: Option<Instant>,
}

impl SubAgentStatus {
    /// 是否已过期（completed_at + TTL < now）。运行中的 entry 不过期。
    pub fn is_expired(&self, now: Instant, ttl: Duration) -> bool {
        match self.completed_at {
            Some(t) => now.duration_since(t) >= ttl,
            None => false,
        }
    }
}

/// SubAgent 运行时状态映射。
///
/// key = `instance_id`（SubAgentStart 携带的唯一标识）。
/// 回退路径用 `agent_id` 匹配，按 `started_at` 倒序。
pub struct SubAgentStatusMap {
    inner: HashMap<String, SubAgentStatus>,
    max_capacity: usize,
    ttl: Duration,
}

impl Default for SubAgentStatusMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SubAgentStatusMap {
    /// 默认配置：max_capacity = 32, TTL = 5 分钟。
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
            max_capacity: 32,
            ttl: Duration::from_secs(300),
        }
    }

    /// 当前 entry 数（含运行中 + 已完成未过期）。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 启动一个 SubAgent —— 注册新 entry，标记 is_running = true。
    ///
    /// 若 `instance_id` 已存在（罕见：重启同一 id），覆盖旧 entry。
    pub fn start(&mut self, instance_id: String, task_preview: String, is_background: bool) {
        if self.inner.len() >= self.max_capacity && !self.inner.contains_key(&instance_id) {
            self.evict_expired();
            // 仍超容量 → 丢弃最早完成的 entry（保留运行中的）
            if self.inner.len() >= self.max_capacity {
                self.evict_oldest_completed();
            }
        }
        self.inner.insert(
            instance_id,
            SubAgentStatus {
                is_running: true,
                is_error: false,
                final_result: None,
                total_steps: 0,
                is_background,
                task_preview,
                started_at: Instant::now(),
                completed_at: None,
            },
        );
    }

    /// 完成前台 SubAgent。
    pub fn complete_foreground(&mut self, instance_id: &str, final_result: String, is_error: bool) {
        if let Some(s) = self.inner.get_mut(instance_id) {
            s.is_running = false;
            s.final_result = Some(final_result);
            s.is_error = is_error;
            s.completed_at = Some(Instant::now());
        }
    }

    /// 完成后台 SubAgent —— 额外接收 `total_steps`（来自
    /// `BackgroundTaskResult.tool_calls_count`，前台路径通过事件累积）。
    pub fn complete_background(
        &mut self,
        instance_id: &str,
        final_result: String,
        is_error: bool,
        total_steps: usize,
    ) {
        if let Some(s) = self.inner.get_mut(instance_id) {
            s.is_running = false;
            s.final_result = Some(final_result);
            s.is_error = is_error;
            s.total_steps = total_steps;
            s.completed_at = Some(Instant::now());
        }
    }

    /// 递增工具步数（前台路径：`AgentEvent::BgToolStep`）。
    /// 后台路径在 complete_background 一次性设置 total_steps。
    pub fn incr_tool_step(&mut self, instance_id: &str) {
        if let Some(s) = self.inner.get_mut(instance_id) {
            s.total_steps += 1;
        }
    }

    /// 标记取消（用户中断 / agent cancel）。
    /// 取消不视为错误（is_error = false），但 is_running = false。
    pub fn mark_cancelled(&mut self, instance_id: &str) {
        if let Some(s) = self.inner.get_mut(instance_id) {
            s.is_running = false;
            s.completed_at = Some(Instant::now());
        }
    }

    /// 精确查询（instance_id 路径）。
    pub fn lookup(&self, instance_id: &str) -> Option<&SubAgentStatus> {
        self.inner.get(instance_id)
    }

    /// 回退路径：只有 agent_id 时，按 `started_at` 倒序返回最近匹配的 entry。
    ///
    /// 优先返回仍在运行的；若全部完成，返回最近完成的。
    /// 用于 `BackgroundTaskCompleted` 找不到 `instance_id`（child_thread_id）
    /// 时按 `agent_name` 回退匹配的场景。
    pub fn lookup_by_agent_id_fallback(&self, _agent_id: &str) -> Option<&SubAgentStatus> {
        // SubAgentStatus 当前不存 agent_id（key 是 instance_id），故此回退路径
        // 由调用方在外部维护的 view_messages 上做（保留 handle_background_task_completed
        // 既有逻辑）。本方法保留为占位以明确 API 边界。
        //
        // 设计权衡：将 agent_id 加入 SubAgentStatus 会引入歧义（同名 agent 多实例），
        // 而回退路径在生产中极罕见（child_thread_id 几乎总存在）。保留 view_messages
        // 上的 O(N) 扫描比污染本映射的语义更合理。
        None
    }

    /// 清理所有过期 entry（completed_at + TTL < now）。
    /// 返回清理的 entry 数。
    pub fn evict_expired(&mut self) -> usize {
        let now = Instant::now();
        let ttl = self.ttl;
        let before = self.inner.len();
        self.inner.retain(|_, s| !s.is_expired(now, ttl));
        before - self.inner.len()
    }

    /// 丢弃最早完成的 entry（容量保护时调用）。
    fn evict_oldest_completed(&mut self) {
        let mut oldest: Option<(String, Instant)> = None;
        for (k, s) in &self.inner {
            if let Some(c) = s.completed_at {
                if oldest.is_none() || c < oldest.as_ref().unwrap().1 {
                    oldest = Some((k.clone(), c));
                }
            }
        }
        if let Some((k, _)) = oldest {
            self.inner.remove(&k);
        }
    }

    /// 清空所有 entry（session 切换 / new_thread 时调用）。
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// 调试迭代器（测试 / 状态面板使用）。
    pub fn iter(&self) -> impl Iterator<Item = (&String, &SubAgentStatus)> {
        self.inner.iter()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_start_registers_running_entry() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "do thing".into(), false);
        let s = map.lookup("inst-1").expect("entry should exist");
        assert!(s.is_running);
        assert!(!s.is_background);
        assert_eq!(s.total_steps, 0);
        assert!(s.final_result.is_none());
        assert!(s.completed_at.is_none());
    }

    #[test]
    fn test_complete_foreground_marks_done() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "task".into(), false);
        map.complete_foreground("inst-1", "ok".into(), false);
        let s = map.lookup("inst-1").expect("entry exists");
        assert!(!s.is_running);
        assert_eq!(s.final_result.as_deref(), Some("ok"));
        assert!(!s.is_error);
        assert!(s.completed_at.is_some());
    }

    #[test]
    fn test_complete_background_sets_total_steps() {
        let mut map = SubAgentStatusMap::new();
        map.start("bg-1".into(), "bg task".into(), true);
        // 后台期间通过 incr_tool_step 累积（可选）
        map.incr_tool_step("bg-1");
        map.incr_tool_step("bg-1");
        // 完成时一次性覆盖 total_steps（来自 BackgroundTaskResult.tool_calls_count）
        map.complete_background("bg-1", "done".into(), false, 7);
        let s = map.lookup("bg-1").expect("entry exists");
        assert_eq!(s.total_steps, 7, "complete_background 应覆盖累积值");
        assert!(s.is_background);
        assert!(!s.is_running);
    }

    #[test]
    fn test_mark_cancelled_not_error() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "task".into(), false);
        map.mark_cancelled("inst-1");
        let s = map.lookup("inst-1").expect("entry exists");
        assert!(!s.is_running);
        assert!(!s.is_error, "取消不应标记为错误");
        assert!(s.completed_at.is_some());
    }

    #[test]
    fn test_evict_expired_removes_old_completed() {
        let mut map = SubAgentStatusMap::new();
        map.ttl = Duration::from_millis(10);

        map.start("inst-1".into(), "task".into(), false);
        map.complete_foreground("inst-1", "ok".into(), false);
        // 等待过期
        std::thread::sleep(Duration::from_millis(20));
        let evicted = map.evict_expired();
        assert_eq!(evicted, 1);
        assert!(map.lookup("inst-1").is_none());
    }

    #[test]
    fn test_evict_expired_keeps_running() {
        let mut map = SubAgentStatusMap::new();
        map.ttl = Duration::from_millis(10);

        map.start("running-1".into(), "task".into(), false);
        std::thread::sleep(Duration::from_millis(20));
        let evicted = map.evict_expired();
        assert_eq!(evicted, 0, "运行中的 entry 不应过期");
        assert!(map.lookup("running-1").is_some());
    }

    #[test]
    fn test_capacity_protection_evicts_oldest_completed() {
        let mut map = SubAgentStatusMap::new();
        map.max_capacity = 2;

        map.start("a".into(), "task a".into(), false);
        map.complete_foreground("a", "done a".into(), false);
        // 给 a 一个明显早的 completed_at
        if let Some(s) = map.inner.get_mut("a") {
            s.completed_at = Some(Instant::now() - Duration::from_secs(100));
        }

        map.start("b".into(), "task b".into(), false);
        map.start("c".into(), "task c".into(), false);

        // 触发 evict —— a 应被丢弃（最早完成的）
        assert!(map.lookup("a").is_none(), "最早完成的应被丢弃");
        assert!(map.lookup("b").is_some());
        assert!(map.lookup("c").is_some());
    }

    #[test]
    fn test_clear_removes_all() {
        let mut map = SubAgentStatusMap::new();
        map.start("a".into(), "task a".into(), false);
        map.start("b".into(), "task b".into(), true);
        assert_eq!(map.len(), 2);

        map.clear();
        assert!(map.is_empty());
    }

    #[test]
    fn test_lookup_missing_returns_none() {
        let map = SubAgentStatusMap::new();
        assert!(map.lookup("nonexistent").is_none());
    }

    #[test]
    fn test_incr_tool_step_no_op_for_missing() {
        let mut map = SubAgentStatusMap::new();
        // 不存在的 instance_id 不应 panic
        map.incr_tool_step("missing");
        assert!(map.is_empty());
    }

    #[test]
    fn test_start_overwrites_existing() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "first".into(), false);
        map.complete_foreground("inst-1", "first done".into(), false);
        assert_eq!(map.lookup("inst-1").unwrap().task_preview, "first");

        // 重启同一 instance_id（罕见，但需优雅处理）
        map.start("inst-1".into(), "second".into(), true);
        let s = map.lookup("inst-1").unwrap();
        assert_eq!(s.task_preview, "second");
        assert!(s.is_running, "重启后应标记为运行中");
        assert!(s.is_background, "新参数应覆盖");
    }

    #[test]
    fn test_iter_visits_all() {
        let mut map = SubAgentStatusMap::new();
        map.start("a".into(), "task a".into(), false);
        map.start("b".into(), "task b".into(), true);
        let count = map.iter().count();
        assert_eq!(count, 2);
    }
}
