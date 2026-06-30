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

use peri_acp_types::view_model::ViewModel;

use crate::render::view_render::{SubAgentRenderInfo, SubAgentStatusProbe};

/// SubAgent 运行时状态（8 字段）。由 TUI 事件实时维护，独立于 ACP ViewCommit。
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
    /// 任务预览（启动时携带）
    pub task_preview: String,
    /// Subagent 类型（"fork" / "researcher" 等；对应 v2 DTO 的 agent_id）。
    /// v2 ViewCommit 中 SubAgentGroupData 没有 instance_id 字段，
    /// 渲染时需要通过 agent_id 查询运行时状态（按 started_at 倒序匹配）。
    pub agent_id: String,
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
#[derive(Clone)]
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
    pub fn start(
        &mut self,
        instance_id: String,
        agent_id: String,
        task_preview: String,
        is_background: bool,
    ) {
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
                agent_id,
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
    /// 优先返回仍在运行的；若全部完成，返回最近完成的。用于：
    /// - v2 渲染：DTO 只有 agent_id（无 instance_id），需要通过 agent_id 查询运行时状态
    /// - BackgroundTaskCompleted 找不到 instance_id（child_thread_id）时按 agent_name 回退
    ///
    /// **歧义容忍**：同名 agent 多实例时返回最近启动的（仍在运行优先）。
    pub fn lookup_by_agent_id(&self, agent_id: &str) -> Option<&SubAgentStatus> {
        let mut best: Option<&SubAgentStatus> = None;
        for s in self.inner.values() {
            if s.agent_id != agent_id {
                continue;
            }
            best = Some(match best {
                None => s,
                Some(prev) => {
                    // 优先仍在运行
                    if s.is_running && !prev.is_running {
                        s
                    } else if s.is_running == prev.is_running {
                        // 同状态 → 比较启动时间，新的优先
                        if s.started_at > prev.started_at {
                            s
                        } else {
                            prev
                        }
                    } else {
                        prev
                    }
                }
            });
        }
        best
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
// SubAgentStatusProbe 实现（基础版 — 仅状态字段，不含子内容）
// ---------------------------------------------------------------------------

impl SubAgentStatusProbe for SubAgentStatusMap {
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo> {
        // 先清理过期项（轻量查询时也触发 evict，避免缓存陈旧）
        // 注：lookup 是 &self，无法直接调 evict_expired（&mut self）。
        // 改为返回结果时由调用方周期性 evict（draw_now 中不调，但 main_loop
        // 每帧 cleanup_agent_state 时调）。这里直接查询即可。
        SubAgentStatusMap::lookup_by_agent_id(self, agent_id).map(|s| SubAgentRenderInfo {
            is_running: s.is_running,
            is_error: s.is_error,
            total_steps: s.total_steps,
            final_result: s.final_result.clone(),
            // 基础实现不注入子内容（DTO 占位符由 SessionSubAgentProbe 路径填充）
            recent_messages: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// SessionSubAgentProbe — 状态 + 子内容复合 probe
// ---------------------------------------------------------------------------

/// 复合 probe：包装 [`SubAgentStatusMap`] + 子内容缓存。
///
/// `draw_now` 从 v1 `view_messages` 中解析所有 `SubAgentGroup.recent_messages`
/// （通过 [`crate::render::vm_convert::message_view_models_to_v2`] 转换为 v2）
/// 一次性预计算，然后 probe 查询时按 agent_id 取出注入到 `SubAgentRenderInfo`。
///
/// 这样 `render_subagent_group` 即使遇到 ACP 层生成的空 placeholder DTO，
/// 也能从 v1 数据源拿到子内容渲染（Phase 2.6 切换前的过渡方案）。
#[derive(Clone)]
pub struct SessionSubAgentProbe {
    /// 运行时状态（共享 clone，避免 &self 与 closure 冲突）
    pub status: SubAgentStatusMap,
    /// agent_id → v2 子内容（已转换，按 agent_id 匹配最近一个）
    pub children: HashMap<String, Vec<ViewModel>>,
}

impl SessionSubAgentProbe {
    /// 从 v1 `view_messages` 解析所有 SubAgentGroup 子内容并构建 probe。
    ///
    /// 同名 agent_id 多次出现时，**后一个覆盖前一个**（保留最新状态）。
    /// 这是 v1 view_messages 的语义（追加）—— 最近的 SubAgentGroup 是最新状态。
    pub fn from_view_messages(
        status: SubAgentStatusMap,
        view_messages: &[crate::ui::message_view::MessageViewModel],
    ) -> Self {
        let mut children: HashMap<String, Vec<ViewModel>> = HashMap::new();
        for vm in view_messages {
            if let crate::ui::message_view::MessageViewModel::SubAgentGroup {
                agent_id,
                recent_messages,
                ..
            } = vm
            {
                let v2 = crate::render::vm_convert::message_view_models_to_v2(recent_messages);
                children.insert(agent_id.clone(), v2);
            }
        }
        Self { status, children }
    }
}

impl SubAgentStatusProbe for SessionSubAgentProbe {
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo> {
        // 通过 trait 方法查询（返回 SubAgentRenderInfo），而非 inherent 方法
        // （inherent 方法返回 &SubAgentStatus，是给运行时维护用的）。
        SubAgentStatusProbe::lookup_by_agent_id(&self.status, agent_id).map(|mut info| {
            // 注入子内容（DTO 占位符路径）
            if let Some(vms) = self.children.get(agent_id) {
                info.recent_messages = vms.clone();
            }
            info
        })
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
        map.start("inst-1".into(), "fork".into(), "do thing".into(), false);
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
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
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
        map.start("bg-1".into(), "fork".into(), "bg task".into(), true);
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
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
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

        map.start("inst-1".into(), "fork".into(), "task".into(), false);
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

        map.start("running-1".into(), "fork".into(), "task".into(), false);
        std::thread::sleep(Duration::from_millis(20));
        let evicted = map.evict_expired();
        assert_eq!(evicted, 0, "运行中的 entry 不应过期");
        assert!(map.lookup("running-1").is_some());
    }

    #[test]
    fn test_capacity_protection_evicts_oldest_completed() {
        let mut map = SubAgentStatusMap::new();
        map.max_capacity = 2;

        map.start("a".into(), "fork".into(), "task a".into(), false);
        map.complete_foreground("a", "done a".into(), false);
        // 给 a 一个明显早的 completed_at
        if let Some(s) = map.inner.get_mut("a") {
            s.completed_at = Some(Instant::now() - Duration::from_secs(100));
        }

        map.start("b".into(), "fork".into(), "task b".into(), false);
        map.start("c".into(), "fork".into(), "task c".into(), false);

        // 触发 evict —— a 应被丢弃（最早完成的）
        assert!(map.lookup("a").is_none(), "最早完成的应被丢弃");
        assert!(map.lookup("b").is_some());
        assert!(map.lookup("c").is_some());
    }

    #[test]
    fn test_clear_removes_all() {
        let mut map = SubAgentStatusMap::new();
        map.start("a".into(), "fork".into(), "task a".into(), false);
        map.start("b".into(), "fork".into(), "task b".into(), true);
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
        map.start("inst-1".into(), "fork".into(), "first".into(), false);
        map.complete_foreground("inst-1", "first done".into(), false);
        assert_eq!(map.lookup("inst-1").unwrap().task_preview, "first");

        // 重启同一 instance_id（罕见，但需优雅处理）
        map.start("inst-1".into(), "fork".into(), "second".into(), true);
        let s = map.lookup("inst-1").unwrap();
        assert_eq!(s.task_preview, "second");
        assert!(s.is_running, "重启后应标记为运行中");
        assert!(s.is_background, "新参数应覆盖");
    }

    #[test]
    fn test_iter_visits_all() {
        let mut map = SubAgentStatusMap::new();
        map.start("a".into(), "fork".into(), "task a".into(), false);
        map.start("b".into(), "fork".into(), "task b".into(), true);
        let count = map.iter().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_lookup_by_agent_id_returns_running_priority() {
        let mut map = SubAgentStatusMap::new();
        // a-1 已完成
        map.start("a-1".into(), "fork".into(), "first".into(), false);
        map.complete_foreground("a-1", "done".into(), false);
        // a-2 仍在运行
        map.start("a-2".into(), "fork".into(), "second".into(), false);

        // 按 agent_id="fork" 查询 → 优先返回运行中的 a-2
        let s = map.lookup_by_agent_id("fork").expect("应找到匹配");
        assert!(s.is_running, "应优先返回运行中的");
        assert_eq!(s.task_preview, "second");
    }

    #[test]
    fn test_lookup_by_agent_id_returns_most_recent_when_all_done() {
        let mut map = SubAgentStatusMap::new();
        map.start("a-1".into(), "fork".into(), "first".into(), false);
        map.complete_foreground("a-1", "done-1".into(), false);
        // 短暂延迟保证 started_at 不同
        std::thread::sleep(Duration::from_millis(2));
        map.start("a-2".into(), "fork".into(), "second".into(), false);
        map.complete_foreground("a-2", "done-2".into(), false);

        // 全部完成 → 返回最近完成的 a-2
        let s = map.lookup_by_agent_id("fork").expect("应找到匹配");
        assert!(!s.is_running);
        assert_eq!(s.task_preview, "second", "应返回最近启动的");
    }

    #[test]
    fn test_lookup_by_agent_id_missing_returns_none() {
        let map = SubAgentStatusMap::new();
        assert!(map.lookup_by_agent_id("nonexistent").is_none());
    }

    #[test]
    fn test_lookup_by_agent_id_distinct_types() {
        let mut map = SubAgentStatusMap::new();
        map.start("a".into(), "fork".into(), "fork task".into(), false);
        map.start(
            "b".into(),
            "researcher".into(),
            "research task".into(),
            false,
        );

        let fork = map.lookup_by_agent_id("fork").expect("fork 应匹配");
        assert_eq!(fork.task_preview, "fork task");

        let researcher = map
            .lookup_by_agent_id("researcher")
            .expect("researcher 应匹配");
        assert_eq!(researcher.task_preview, "research task");
    }

    // --- SessionSubAgentProbe 测试 ---

    #[test]
    fn test_session_probe_injects_children_from_view_messages() {
        use crate::render::view_render::SubAgentStatusProbe;
        use crate::ui::message_view::MessageViewModel;
        use peri_acp_types::view_model::ViewModel;

        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);

        // 构造一个含子内容的 SubAgentGroup v1 视图
        let child_vm = MessageViewModel::user("hello from child".into());
        let parent = MessageViewModel::SubAgentGroup {
            agent_id: "fork".into(),
            instance_id: Some("inst-1".into()),
            task_preview: "task".into(),
            is_running: true,
            is_background: false,
            total_steps: 0,
            recent_messages: vec![child_vm],
            collapsed: false,
            bg_hash: None,
            final_result: None,
            is_error: false,
            batch_agents: Vec::new(),
            content_hash: 0,
        };

        let probe = SessionSubAgentProbe::from_view_messages(map, std::slice::from_ref(&parent));
        let info = probe
            .lookup_by_agent_id("fork")
            .expect("应找到 fork 的运行时状态");

        assert!(info.is_running);
        assert_eq!(
            info.recent_messages.len(),
            1,
            "应注入 1 个子内容（UserBubble）"
        );
        assert!(
            matches!(info.recent_messages[0], ViewModel::UserBubble(_)),
            "子内容应为 UserBubble（vm_convert 转换后）"
        );
    }

    #[test]
    fn test_session_probe_recent_messages_empty_when_no_match() {
        use crate::render::view_render::SubAgentStatusProbe;

        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);

        // 没有 SubAgentGroup view → children 为空，但 status 仍能查到
        let probe = SessionSubAgentProbe::from_view_messages(map, &[]);
        let info = probe.lookup_by_agent_id("fork").expect("status 应能找到");
        assert!(info.recent_messages.is_empty(), "无子内容时应为空 Vec");
    }

    #[test]
    fn test_session_probe_later_agent_overrides_earlier() {
        use crate::render::view_render::SubAgentStatusProbe;
        use crate::ui::message_view::MessageViewModel;

        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "first".into(), false);

        // 两个同名 agent_id 的 SubAgentGroup，后者应覆盖前者
        let first = MessageViewModel::SubAgentGroup {
            agent_id: "fork".into(),
            instance_id: Some("inst-1".into()),
            task_preview: "first".into(),
            is_running: false,
            is_background: false,
            total_steps: 0,
            recent_messages: vec![MessageViewModel::user("from first".into())],
            collapsed: false,
            bg_hash: None,
            final_result: None,
            is_error: false,
            batch_agents: Vec::new(),
            content_hash: 0,
        };
        let second = MessageViewModel::SubAgentGroup {
            agent_id: "fork".into(),
            instance_id: Some("inst-2".into()),
            task_preview: "second".into(),
            is_running: true,
            is_background: false,
            total_steps: 0,
            recent_messages: vec![MessageViewModel::user("from second".into())],
            collapsed: false,
            bg_hash: None,
            final_result: None,
            is_error: false,
            batch_agents: Vec::new(),
            content_hash: 0,
        };

        let probe = SessionSubAgentProbe::from_view_messages(map, &[first, second]);
        let info = probe.lookup_by_agent_id("fork").expect("应找到");

        // 后者覆盖前者 — recent_messages 应来自 second
        assert_eq!(info.recent_messages.len(), 1);
        if let peri_acp_types::view_model::ViewModel::UserBubble(d) = &info.recent_messages[0] {
            assert_eq!(d.text, "from second", "后一个 SubAgentGroup 应覆盖");
        } else {
            panic!("应为 UserBubble");
        }
    }

    #[test]
    fn test_session_probe_returns_none_for_unknown_agent() {
        use crate::render::view_render::SubAgentStatusProbe;

        let map = SubAgentStatusMap::new();
        let probe = SessionSubAgentProbe::from_view_messages(map, &[]);
        assert!(
            probe.lookup_by_agent_id("unknown").is_none(),
            "未知 agent_id 应返回 None"
        );
    }
}
