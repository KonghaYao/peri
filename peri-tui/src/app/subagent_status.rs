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

use crate::kit::view_render::{SubAgentRenderInfo, SubAgentStatusProbe};

/// SubAgent 运行时状态（9 字段）。由 TUI 事件实时维护，独立于 ACP ViewCommit。
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
    /// Phase 2.6：子 Agent 内部累积的 v2 ViewModels。
    ///
    /// 由 `source_agent_id`（child_thread_id）路由的事件累积：
    /// - `ToolStart` / `ToolEnd` 路由匹配时，转换为 `ViewModel::ToolCard` 追加
    /// - 未来可扩展 `AssistantChunk` / `AiReasoning` 路由
    ///
    /// 这是 v2 渲染路径的**权威数据源**，独立于 v1 `view_messages`。
    /// `SessionSubAgentProbe::lookup_by_agent_id` 优先读取此字段。
    pub child_messages: Vec<ViewModel>,
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

    /// Cron #32: per-entry cap on `child_messages`. Running entries don't expire
    /// (TTL only counts after completion), so without this cap a long-running
    /// background/researcher subagent accumulates VMs without bound — a real
    /// memory leak for hour-scale agents. 200 keeps the thread panel useful
    /// while bounding memory (~200KB per entry, ~6MB worst case at capacity).
    pub(crate) const MAX_CHILD_MESSAGES: usize = 200;

    /// 内部辅助：push 后钳制 child_messages 长度。
    ///
    /// Cron #36: 优先丢弃「已完成」条目（含输出的 ToolCard、AssistantBubble 等），
    /// **保留 pending ToolCard**（output_summary 为空且非错误的 ToolStart entry）。
    ///
    /// 历史 bug：纯 FIFO 丢弃会移除尚未收到 ToolEnd 的 ToolStart entry。当 ToolEnd
    /// 到达时，`update_child_tool_output` 找不到 tool_id → 调用方 fallback
    /// （`agent_ops/mod.rs:259-278`）创建一个 `input_summary` 为空的**重复**
    /// ToolCard，原始 input_summary 永久丢失。在长运行 SubAgent（>200 工具调用）
    /// 中可观察到 orphaned output-only 卡片。
    ///
    /// 安全网：若可丢弃条目不足（极端情况：全是 pending ToolCard），降级为 FIFO
    /// 从头部丢弃——内存边界比 orphan 防御更重要（最坏情况多保留几条 pending 卡，
    /// 但仍受 cap 约束）。
    fn push_child_bounded(status: &mut SubAgentStatus, vm: ViewModel) {
        status.child_messages.push(vm);
        let cap = Self::MAX_CHILD_MESSAGES;
        if status.child_messages.len() <= cap {
            return;
        }
        let drop_n = status.child_messages.len() - cap;

        // 第一轮：保留 pending ToolCard，只丢弃已完成条目。
        let mut dropped = 0usize;
        status.child_messages.retain(|vm| {
            if dropped >= drop_n {
                return true;
            }
            let is_pending_tool = matches!(
                vm,
                ViewModel::ToolCard(d) if d.output_summary.is_empty() && !d.is_error
            );
            if is_pending_tool {
                true // 保留
            } else {
                dropped += 1;
                false // 丢弃
            }
        });

        // 安全网：若丢弃数量仍不足，FIFO 从头部丢弃（内存边界优先）。
        if dropped < drop_n {
            let remaining = drop_n - dropped;
            status.child_messages.drain(..remaining);
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
                child_messages: Vec::new(),
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

    /// Phase 2.6：通过 source identifier（child_thread_id 或 agent_id）
    /// 找到 owner entry 并返回可变引用。
    ///
    /// 优先精确匹配 instance_id；失败时回退到 `lookup_by_agent_id` 逻辑
    /// （运行中优先 + 最近启动优先）。返回 `&mut` 供调用方修改 child_messages。
    pub fn find_owner_mut(&mut self, source_id: &str) -> Option<&mut SubAgentStatus> {
        // 1. 精确匹配 instance_id
        if self.inner.contains_key(source_id) {
            return self.inner.get_mut(source_id);
        }
        // 2. 回退：按 agent_id 找最近匹配的 key（不能直接返回 &mut，需先定 key）
        let target_key: Option<String> = {
            let mut best: Option<(&String, &SubAgentStatus)> = None;
            for (k, s) in &self.inner {
                if s.agent_id != source_id {
                    continue;
                }
                best = Some(match best {
                    None => (k, s),
                    Some((prev_k, prev)) => {
                        let prefer_new = if s.is_running && !prev.is_running {
                            true
                        } else if s.is_running == prev.is_running {
                            s.started_at > prev.started_at
                        } else {
                            false
                        };
                        if prefer_new { (k, s) } else { (prev_k, prev) }
                    }
                });
            }
            best.map(|(k, _)| k.clone())
        };
        target_key.and_then(|k| self.inner.get_mut(&k))
    }

    /// Phase 2.6：路由 v2 ViewModel 到匹配的 SubAgent child_messages。
    ///
    /// 返回 `true` 表示成功路由（调用方应跳过 view_messages 累积）；
    /// 返回 `false` 表示无匹配（调用方应 fallback 到主消息流）。
    pub fn append_child_message(&mut self, source_id: &str, vm: ViewModel) -> bool {
        if let Some(s) = self.find_owner_mut(source_id) {
            Self::push_child_bounded(s, vm);
            true
        } else {
            false
        }
    }

    /// Phase 2.6：路由子 Agent 流式文本 chunk 到 child_messages。
    ///
    /// 在 `source_id` 匹配的 owner 中：
    /// - 若最后一个 VM 是 AssistantBubble，append text（同一轮回复累积）
    /// - 否则新建 AssistantBubble push（新一轮回复开始）
    ///
    /// 返回 `true` 表示成功路由。返回 `false` 表示无匹配 owner。
    pub fn append_child_text(&mut self, source_id: &str, text: &str) -> bool {
        if let Some(s) = self.find_owner_mut(source_id) {
            // Cron #32: 先用不可变 last() 判定是否需要新建 bubble，避免在
            // match last_mut() 期间持 &mut 同时调 push_child_bounded(s, ..) 的借用冲突。
            let needs_new_bubble =
                !matches!(s.child_messages.last(), Some(ViewModel::AssistantBubble(_)));
            if needs_new_bubble {
                Self::push_child_bounded(
                    s,
                    ViewModel::AssistantBubble(peri_acp_types::view_model::AssistantBubbleData {
                        text: text.to_string(),
                        reasoning: None,
                        tool_card_ids: Vec::new(),
                    }),
                );
            } else if let Some(ViewModel::AssistantBubble(d)) = s.child_messages.last_mut() {
                d.text.push_str(text);
            }
            true
        } else {
            false
        }
    }

    /// Phase 2.6：更新子 Agent ToolCard 的 output（ToolEnd 路由）。
    ///
    /// 在 `source_id` 匹配的 owner child_messages 中查找 `tool_id == tool_call_id`
    /// 的 ToolCard，更新 output_summary / is_error。返回 `true` 表示成功。
    pub fn update_child_tool_output(
        &mut self,
        source_id: &str,
        tool_call_id: &str,
        output: String,
        is_error: bool,
    ) -> bool {
        if let Some(s) = self.find_owner_mut(source_id) {
            for vm in &mut s.child_messages {
                if let ViewModel::ToolCard(d) = vm
                    && d.tool_id == tool_call_id
                {
                    d.output_summary = output;
                    d.is_error = is_error;
                    return true;
                }
            }
        }
        false
    }

    /// 回退路径：只有 agent_id 时，按 `started_at` 倒序返回最近匹配的 entry。
    ///
    /// 优先返回仍在运行的；若全部完成，返回最近完成的。用于：
    /// - v2 渲染：DTO 只有 agent_id（无 instance_id），需要通过 agent_id 查询运行时状态
    /// - BackgroundTaskCompleted 找不到 instance_id（child_thread_id）时按 agent_name 回退
    ///
    /// **歧义容忍**：同名 agent 多实例时返回最近启动的（仍在运行优先）。
    ///
    /// **[TRAP]** 当多个同类型 SubAgent 并发运行时（如两个 "researcher"），
    /// 此函数对所有 SubAgentGroup 占位符返回同一个 status entry，导致 misroute：
    /// 多个卡片显示相同的 is_running / final_result，子内容串到错误实例。
    /// 根因是 `SubAgentGroupData` DTO 缺少 `instance_id` 字段（ACP 层冻结）。
    /// 等 ACP 解冻后，DTO 加 instance_id → 渲染优先精确匹配。TUI 侧已预留
    /// `lookup_by_instance_id` 接口(`SubAgentStatusProbe::lookup_by_instance_id`)。
    /// See `docs/refactor/progress.html` SubAgent render-misroute。
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
            if let Some(c) = s.completed_at
                && (oldest.is_none() || c < oldest.as_ref().unwrap().1)
            {
                oldest = Some((k.clone(), c));
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

/// 复合 probe：包装 [`SubAgentStatusMap`]，注入 v2 子内容到 render。
///
/// **数据源**（Phase 2.6 step 4 已退役 legacy 兼容源）：
/// - 唯一权威源：`SubAgentStatus.child_messages` — 由 `source_agent_id`
///   路由的 ToolStart/ToolEnd/AssistantChunk 实时累积
///
/// Phase 2.5 之前曾维护 v1 `view_messages` 中的 `SubAgentGroup.recent_messages`
/// 作为兼容源，但因生产中永久为空 Vec（v1 设计如此）且 child_messages
/// 已完整覆盖（文本+工具+未来扩展），该路径已删除。
#[derive(Clone)]
pub struct SessionSubAgentProbe {
    /// 运行时状态（含 `child_messages` 权威源）
    pub status: SubAgentStatusMap,
}

impl SessionSubAgentProbe {
    /// 从 `SubAgentStatusMap` 构建 probe。
    pub fn new(status: SubAgentStatusMap) -> Self {
        Self { status }
    }
}

impl SubAgentStatusProbe for SessionSubAgentProbe {
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo> {
        SubAgentStatusProbe::lookup_by_agent_id(&self.status, agent_id).map(|mut info| {
            // 唯一权威源：SubAgentStatus.child_messages（按 agent_id 通过 status map 查询）
            // 注意：info 中没有 instance_id 字段，只能通过 agent_id 回退匹配。
            if let Some(s) = self.status.lookup_by_agent_id(agent_id)
                && !s.child_messages.is_empty()
            {
                info.recent_messages = s.child_messages.clone();
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

    // --- find_owner_mut / append_child_message / update_child_tool_output 测试 ---

    #[test]
    fn test_find_owner_mut_instance_id_exact_match() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // instance_id 精确匹配
        let owner = map.find_owner_mut("inst-1").expect("应精确匹配");
        assert_eq!(owner.agent_id, "fork");
    }

    #[test]
    fn test_find_owner_mut_fallback_to_agent_id() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // 用 agent_id 查询（非 instance_id）→ 回退路径
        let owner = map.find_owner_mut("fork").expect("应通过 agent_id 匹配");
        assert_eq!(owner.task_preview, "task");
    }

    #[test]
    fn test_find_owner_mut_no_match_returns_none() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        assert!(map.find_owner_mut("unknown").is_none());
    }

    #[test]
    fn test_append_child_message_routes_to_matching_owner() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);

        let vm = ViewModel::UserBubble(peri_acp_types::view_model::UserBubbleData {
            text: "child msg".into(),
        });
        let ok = map.append_child_message("fork", vm);
        assert!(ok, "应成功路由");

        let s = map.lookup("inst-1").unwrap();
        assert_eq!(s.child_messages.len(), 1);
    }

    #[test]
    fn test_append_child_message_returns_false_when_no_match() {
        let mut map = SubAgentStatusMap::new();
        let vm =
            ViewModel::UserBubble(peri_acp_types::view_model::UserBubbleData { text: "x".into() });
        assert!(!map.append_child_message("unknown", vm));
        assert!(map.is_empty(), "无 owner 时不应保留 vm");
    }

    #[test]
    fn test_update_child_tool_output_finds_by_tool_call_id() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // 累积一个 ToolCard
        let tool_vm = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
            tool_id: "tc-1".into(),
            tool_name: "Read".into(),
            input_summary: "file.rs".into(),
            output_summary: String::new(),
            is_error: false,
            diff: None,
        });
        map.append_child_message("inst-1", tool_vm);

        // ToolEnd 路由：更新 output
        let ok = map.update_child_tool_output("inst-1", "tc-1", "content".into(), false);
        assert!(ok);
        let s = map.lookup("inst-1").unwrap();
        if let ViewModel::ToolCard(d) = &s.child_messages[0] {
            assert_eq!(d.output_summary, "content");
        } else {
            panic!("应为 ToolCard");
        }
    }

    #[test]
    fn test_update_child_tool_output_no_match_returns_false() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // 没有 ToolCard → false
        assert!(!map.update_child_tool_output("inst-1", "tc-x", "x".into(), false));
    }

    // --- SessionSubAgentProbe 测试 ---

    /// Phase 2.6 step 4：probe 仅包装 status map，权威源 = child_messages
    #[test]
    fn test_session_probe_injects_child_messages_from_authoritative_source() {
        use crate::kit::view_render::SubAgentStatusProbe;
        use peri_acp_types::view_model::ViewModel;

        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // 通过 source_agent_id 路由累积权威源
        let tool_vm = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
            tool_id: "tc-1".into(),
            tool_name: "Read".into(),
            input_summary: "f.rs".into(),
            output_summary: "ok".into(),
            is_error: false,
            diff: None,
        });
        map.append_child_message("fork", tool_vm);

        let probe = SessionSubAgentProbe::new(map);
        let info = probe
            .lookup_by_agent_id("fork")
            .expect("应找到 fork 的运行时状态");

        assert!(info.is_running);
        assert_eq!(
            info.recent_messages.len(),
            1,
            "应注入 1 个子内容（ToolCard）— 来自权威源 child_messages"
        );
        assert!(
            matches!(info.recent_messages[0], ViewModel::ToolCard(_)),
            "子内容应为 ToolCard"
        );
    }

    #[test]
    fn test_session_probe_recent_messages_empty_when_child_messages_empty() {
        use crate::kit::view_render::SubAgentStatusProbe;

        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // child_messages 为空 → recent_messages 应为空 Vec
        let probe = SessionSubAgentProbe::new(map);
        let info = probe.lookup_by_agent_id("fork").expect("status 应能找到");
        assert!(info.recent_messages.is_empty(), "无子内容时应为空 Vec");
    }

    #[test]
    fn test_session_probe_returns_none_for_unknown_agent() {
        use crate::kit::view_render::SubAgentStatusProbe;

        let map = SubAgentStatusMap::new();
        let probe = SessionSubAgentProbe::new(map);
        assert!(
            probe.lookup_by_agent_id("unknown").is_none(),
            "未知 agent_id 应返回 None"
        );
    }

    /// Phase 2.6 step 3：append_child_text 第一次 chunk 创建 AssistantBubble
    #[test]
    fn test_append_child_text_creates_new_bubble() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        let routed = map.append_child_text("inst-1", "Hello");
        assert!(routed);
        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(s.child_messages.len(), 1);
        match &s.child_messages[0] {
            ViewModel::AssistantBubble(d) => assert_eq!(d.text, "Hello"),
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    /// Phase 2.6 step 3：连续 chunk 累积到同一个 AssistantBubble
    #[test]
    fn test_append_child_text_accumulates_into_existing_bubble() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        map.append_child_text("inst-1", "Hello");
        map.append_child_text("inst-1", " world");
        map.append_child_text("inst-1", "!");
        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(s.child_messages.len(), 1, "三个 chunk 应累积到同一 bubble");
        match &s.child_messages[0] {
            ViewModel::AssistantBubble(d) => assert_eq!(d.text, "Hello world!"),
            other => panic!("expected AssistantBubble, got {:?}", other),
        }
    }

    /// Phase 2.6 step 3：ToolCard 后的 chunk 应创建新 bubble（不混入工具卡）
    #[test]
    fn test_append_child_text_after_toolcard_creates_new_bubble() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "task".into(), false);
        // 模拟先有文本，再有工具，再有文本
        map.append_child_text("inst-1", "first ");
        map.append_child_message(
            "inst-1",
            ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: "t1".into(),
                tool_name: "Read".into(),
                input_summary: "foo.rs".into(),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            }),
        );
        map.append_child_text("inst-1", "second");
        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(s.child_messages.len(), 3, "bubble + toolcard + new bubble");
        match &s.child_messages[2] {
            ViewModel::AssistantBubble(d) => assert_eq!(d.text, "second"),
            other => panic!("expected new bubble, got {:?}", other),
        }
    }

    /// Phase 2.6 step 3：未知 source_id 返回 false
    #[test]
    fn test_append_child_text_unknown_source_returns_false() {
        let mut map = SubAgentStatusMap::new();
        let routed = map.append_child_text("unknown", "text");
        assert!(!routed);
    }

    /// Phase 2.6 step 3：通过 agent_id 回退匹配也能累积
    #[test]
    fn test_append_child_text_falls_back_to_agent_id() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "researcher".into(), "task".into(), false);
        // 使用 agent_id（"researcher"）而非 instance_id（"inst-1"）
        let routed = map.append_child_text("researcher", "fallback text");
        assert!(routed, "agent_id 回退应匹配");
        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(s.child_messages.len(), 1);
    }

    // --- Cron #32: child_messages cap regression tests ---

    /// Cron #32: child_messages 增长超过 MAX_CHILD_MESSAGES 时，最旧条目应被 FIFO 丢弃。
    /// 长时间运行的 background/researcher SubAgent 可能累积上千条 VM，导致内存泄漏。
    #[test]
    fn test_append_child_message_caps_at_max_with_fifo_eviction() {
        let mut map = SubAgentStatusMap::new();
        map.start(
            "inst-1".into(),
            "researcher".into(),
            "long-running".into(),
            false,
        );
        let cap = SubAgentStatusMap::MAX_CHILD_MESSAGES;

        // 推入 cap + 50 条 ToolCard
        for i in 0..(cap + 50) {
            let vm = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("tc-{i}"),
                tool_name: "Read".into(),
                input_summary: format!("file-{i}.rs"),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            });
            assert!(map.append_child_message("inst-1", vm));
        }

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(
            s.child_messages.len(),
            cap,
            "child_messages 必须钳制在 MAX_CHILD_MESSAGES"
        );
        // 最旧 50 条应已被丢弃，保留下标 [50, cap+50)
        if let ViewModel::ToolCard(d) = &s.child_messages[0] {
            assert_eq!(
                d.tool_id, "tc-50",
                "FIFO 丢弃应保留最新 cap 条，最旧 50 条已删除"
            );
        } else {
            panic!("首条应为 ToolCard");
        }
        if let ViewModel::ToolCard(d) = s.child_messages.last().unwrap() {
            assert_eq!(d.tool_id, format!("tc-{}", cap + 49));
        }
    }

    /// Cron #32: append_child_text 也必须遵守 cap。新建 bubble 路径会增长 Vec，
    /// 验证超限后最旧 bubble 被丢弃。
    #[test]
    fn test_append_child_text_caps_when_many_new_bubbles() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "many-bubbles".into(), false);
        let cap = SubAgentStatusMap::MAX_CHILD_MESSAGES;

        // 每个 chunk 前先推一个 ToolCard，强制 append_child_text 走「新建 bubble」分支
        for i in 0..(cap + 10) {
            let tool_vm = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("tc-{i}"),
                tool_name: "Read".into(),
                input_summary: format!("f-{i}"),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            });
            map.append_child_message("inst-1", tool_vm);
            // 紧跟一个 chunk → 因为 last 是 ToolCard，会新建 AssistantBubble
            map.append_child_text("inst-1", &format!("text-{i}"));
        }

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(
            s.child_messages.len(),
            cap,
            "append_child_text 也必须钳制在 MAX_CHILD_MESSAGES"
        );
    }

    /// Cron #32: 正常使用（远低于 cap）不应触发任何丢弃。回归保护。
    #[test]
    fn test_cap_does_not_affect_normal_usage_below_limit() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "normal".into(), false);

        // 推 10 条（远低于 200 cap）
        for i in 0..10 {
            let vm = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("tc-{i}"),
                tool_name: "Read".into(),
                input_summary: format!("f-{i}"),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            });
            map.append_child_message("inst-1", vm);
        }

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(s.child_messages.len(), 10, "未达 cap 时不应丢弃");
        if let ViewModel::ToolCard(d) = &s.child_messages[0] {
            assert_eq!(d.tool_id, "tc-0", "首条应保留");
        }
    }

    // ─── Cron #36: push_child_bounded 保留 pending ToolCard ────────────────
    //
    // Bug（Cron #35 审计发现，af347/a78e1 verifier 确认）：
    // 长 SubAgent 累积 >200 工具调用时，push_child_bounded FIFO 丢弃会移除
    // 尚未收到 ToolEnd 的 ToolStart entry。当 ToolEnd 到达时，
    // update_child_tool_output 找不到 tool_id，调用方 fallback 创建一个
    // input_summary 为空的**重复** ToolCard——原始输入永久丢失。
    //
    // 修复：FIFO 丢弃时优先选择已完成条目，保留 pending ToolCard。

    /// Cron #36: 当 child_messages 含混合（completed + pending）条目且超出 cap 时，
    /// 应优先丢弃 completed ToolCard，保留 pending ToolCard。
    #[test]
    fn test_push_child_bounded_prefers_dropping_completed_over_pending() {
        let mut map = SubAgentStatusMap::new();
        map.start(
            "inst-1".into(),
            "researcher".into(),
            "many-tools".into(),
            false,
        );
        let cap = SubAgentStatusMap::MAX_CHILD_MESSAGES;

        // 布局：100 个 completed ToolCard（有 output）+ 100 个 pending ToolCard（无 output）= 200 = cap
        for i in 0..100 {
            let completed = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("completed-{i}"),
                tool_name: "Bash".into(),
                input_summary: format!("cmd-{i}"),
                output_summary: format!("output-{i}"),
                is_error: false,
                diff: None,
            });
            map.append_child_message("inst-1", completed);
        }
        for i in 0..100 {
            let pending = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("pending-{i}"),
                tool_name: "Read".into(),
                input_summary: format!("path-{i}"),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            });
            map.append_child_message("inst-1", pending);
        }

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(
            s.child_messages.len(),
            cap,
            "预热：恰好达到 cap，未触发丢弃"
        );

        // 触发：push 第 201 条 entry（pending ToolCard）
        let extra = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
            tool_id: "extra".into(),
            tool_name: "Edit".into(),
            input_summary: "edit-file".into(),
            output_summary: String::new(),
            is_error: false,
            diff: None,
        });
        assert!(map.append_child_message("inst-1", extra));

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(
            s.child_messages.len(),
            cap,
            "cap 保持不变（丢弃 1 条 completed）"
        );

        // 收集所有剩余 tool_id
        let remaining_ids: Vec<String> = s
            .child_messages
            .iter()
            .filter_map(|vm| match vm {
                ViewModel::ToolCard(d) => Some(d.tool_id.clone()),
                _ => None,
            })
            .collect();

        // 关键断言：completed-0 应该被丢弃（最旧的 completed）
        assert!(
            !remaining_ids.iter().any(|id| id == "completed-0"),
            "Cron #36: 最旧的 completed ToolCard 应被优先丢弃"
        );

        // 关键断言：所有 pending ToolCard 都保留
        for i in 0..100 {
            let id = format!("pending-{i}");
            assert!(
                remaining_ids.iter().any(|x| x == &id),
                "Cron #36: pending ToolCard {} 必须保留（未被 FIFO 丢弃）",
                id
            );
        }

        // 关键断言：新 entry 也保留
        assert!(
            remaining_ids.iter().any(|id| id == "extra"),
            "Cron #36: 新 push 的 entry 必须保留"
        );
    }

    /// Cron #36: 端到端验证 orphaned duplicate 不再产生。
    ///
    /// 场景：SubAgent 累积 >200 个 ToolStart（全部 pending，因为 SubAgent 还在
    /// 并发执行），然后 ToolEnd 到达。
    /// - 旧行为：FIFO 丢弃最旧 pending ToolCard → ToolEnd 找不到匹配 → fallback
    ///   创建 input_summary 空白的重复卡片。
    /// - 新行为：保留 pending ToolCard（即使超出 cap，由 safety net FIFO 在
    ///   AssistantBubble 等可丢弃条目中消化）→ ToolEnd 正常更新已有 entry。
    #[test]
    fn test_push_child_bounded_preserves_pending_allows_toolend_match() {
        let mut map = SubAgentStatusMap::new();
        map.start(
            "inst-1".into(),
            "researcher".into(),
            "concurrent-tools".into(),
            false,
        );
        let cap = SubAgentStatusMap::MAX_CHILD_MESSAGES;

        // 阶段 1：push cap 个 pending ToolCard（output 为空）
        // 这是「全是 pending」的极端情况——safety net 必须介入 FIFO 丢弃以
        // 维持内存边界。所以这里我们穿插 AssistantBubble 让第一轮丢弃生效。
        for i in 0..cap {
            // 穿插 AssistantBubble（droppable）—— i 偶数 push bubble，奇数 push tool
            if i % 2 == 0 {
                let bubble =
                    ViewModel::AssistantBubble(peri_acp_types::view_model::AssistantBubbleData {
                        text: format!("thinking-{i}"),
                        reasoning: None,
                        tool_card_ids: Vec::new(),
                    });
                map.append_child_message("inst-1", bubble);
            } else {
                let tool = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                    tool_id: format!("tool-{}", i),
                    tool_name: "Read".into(),
                    input_summary: format!("path-{}", i),
                    output_summary: String::new(),
                    is_error: false,
                    diff: None,
                });
                map.append_child_message("inst-1", tool);
            }
        }

        // 此时 child_messages.len() == cap（200），含 100 bubble + 100 pending tool。

        // 阶段 2：push 第 cap+1 个 pending ToolCard（触发丢弃 1 条）
        let new_tool_id = "tool-new".to_string();
        let new_tool = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
            tool_id: new_tool_id.clone(),
            tool_name: "Bash".into(),
            input_summary: "ls -la".into(),
            output_summary: String::new(),
            is_error: false,
            diff: None,
        });
        assert!(map.append_child_message("inst-1", new_tool));

        // 阶段 3：ToolEnd 到达，尝试更新刚 push 的 tool-new
        let updated = map.update_child_tool_output("inst-1", &new_tool_id, "success".into(), false);

        // 关键断言：update 成功（ToolCard 未被 FIFO 丢弃）
        assert!(
            updated,
            "Cron #36: ToolEnd 必须能匹配到新 push 的 ToolCard（未被 FIFO 丢弃）"
        );

        // 验证：找到的 entry output 已更新，input_summary 保留
        let s = map.lookup("inst-1").expect("entry exists");
        let target = s
            .child_messages
            .iter()
            .find_map(|vm| match vm {
                ViewModel::ToolCard(d) if d.tool_id == new_tool_id => Some(d),
                _ => None,
            })
            .expect("tool-new entry 应保留");

        assert_eq!(
            target.output_summary, "success",
            "ToolEnd 应更新 output_summary"
        );
        assert_eq!(
            target.input_summary, "ls -la",
            "原始 input_summary 应保留（未被孤儿重复替换）"
        );

        // 关键断言：未产生重复 ToolCard（同一 tool_id 只出现一次）
        let count = s
            .child_messages
            .iter()
            .filter(|vm| matches!(vm, ViewModel::ToolCard(d) if d.tool_id == new_tool_id))
            .count();
        assert_eq!(
            count, 1,
            "Cron #36: ToolCard 不应被重复（orphan duplicate 应消除）"
        );
    }

    /// Cron #36: 全 pending ToolCard 的极端场景——safety net FIFO 降级保证内存边界。
    ///
    /// 当所有条目都是 pending ToolCard（无可丢弃 completed entry）时，
    /// 必须从头部 FIFO 丢弃以维持 cap，避免无限增长。
    #[test]
    fn test_push_child_bounded_all_pending_falls_back_to_fifo() {
        let mut map = SubAgentStatusMap::new();
        map.start("inst-1".into(), "fork".into(), "all-pending".into(), false);
        let cap = SubAgentStatusMap::MAX_CHILD_MESSAGES;

        // 推入 cap + 30 个 pending ToolCard
        for i in 0..(cap + 30) {
            let tool = ViewModel::ToolCard(peri_acp_types::view_model::ToolCardData {
                tool_id: format!("tc-{i}"),
                tool_name: "Read".into(),
                input_summary: format!("f-{i}"),
                output_summary: String::new(),
                is_error: false,
                diff: None,
            });
            map.append_child_message("inst-1", tool);
        }

        let s = map.lookup("inst-1").expect("entry exists");
        assert_eq!(
            s.child_messages.len(),
            cap,
            "Cron #36 safety net: 全 pending 场景仍必须钳制到 cap"
        );

        // 最旧 30 条已 FIFO 丢弃，保留下标 [30, cap+30)
        if let ViewModel::ToolCard(d) = &s.child_messages[0] {
            assert_eq!(
                d.tool_id, "tc-30",
                "Cron #36 safety net: 全 pending 时 FIFO 从头部丢弃"
            );
        } else {
            panic!("首条应为 ToolCard");
        }
    }
}
