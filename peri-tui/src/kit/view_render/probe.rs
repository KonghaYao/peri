//! SubAgent 运行时状态探针（thread-local）。
//!
//! V2 TuiSubAgentGroup 渲染所需的运行时状态（用于显示状态 emoji + total_steps）。
//! 由 app 层通过 [`with_status_probe`] 注入；render_subagent_group 通过
//! agent_id 查询。对应 v2 DTO `TuiSubAgentGroup` 缺失的运行时字段。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;

use crate::kit::tui_render_unit::TuiRenderUnit;

/// V2 TuiSubAgentGroup 渲染所需的运行时状态（用于显示状态 emoji + total_steps）。
///
/// 由 app 层通过 [`with_status_probe`] 注入；render_subagent_group 通过
/// agent_id 查询。对应 v2 DTO `TuiSubAgentGroup` 缺失的运行时字段。
#[derive(Clone, Debug, Default)]
pub struct SubAgentRenderInfo {
    pub is_running: bool,
    pub is_error: bool,
    pub total_steps: usize,
    pub final_result: Option<String>,
    /// 子 Agent 的最近消息（v2 TuiRenderUnit 形式）。
    ///
    /// 当 v2 DTO `TuiSubAgentGroup.view_models` 为空（ACP 层 view_mapper
    /// 生成的 placeholder）时，渲染层从此字段取子内容。app 层通过
    /// 通过 `subagent_status` 状态 probe 把 SubAgent 运行时状态转换为 v2 VMs
    /// 后填充此字段。
    pub recent_messages: Vec<TuiRenderUnit>,
}

/// V2 TuiSubAgentGroup 状态查询接口。app 层实现并通过 [`with_status_probe`] 设置。
///
/// 实现者通常是 `SubAgentStatusMap` 的快照或借用包装。
pub trait SubAgentStatusProbe {
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo>;
}

thread_local! {
    /// 当前线程的 status probe。draw_now 在调用 terminal.draw 前设置，
    /// render_subagent_group 通过 lookup_subagent_status 查询。
    pub(crate) static STATUS_PROBE: RefCell<Option<Rc<dyn SubAgentStatusProbe>>> = const { RefCell::new(None) };
}

thread_local! {
    /// 全局渲染调用计数器，用于跨递归边界的 yield 决策。
    /// 每次 render_v2_vm 入口递增 1；render_bridge::append_entries
    /// 每 N 次调用检查后 yield。在 append_entries 结束时重置为 0。
    pub(crate) static RENDER_CALL_COUNT: AtomicUsize = const { AtomicUsize::new(0) };
}

/// 在 closure 内设置 status probe，closure 结束后自动恢复（支持嵌套）。
///
/// 典型用法：`draw_now` 中 `with_status_probe(probe, || self.terminal.draw(...))`。
pub fn with_status_probe<R>(probe: Rc<dyn SubAgentStatusProbe>, f: impl FnOnce() -> R) -> R {
    let prev = STATUS_PROBE.with(|cell| cell.replace(Some(probe)));
    let result = f();
    STATUS_PROBE.with(|cell| {
        let _ = cell.replace(prev);
    });
    result
}

/// render_subagent_group 内部使用：按 agent_id 查询运行时状态。
pub(crate) fn lookup_subagent_status(agent_id: &str) -> Option<SubAgentRenderInfo> {
    STATUS_PROBE.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|probe| probe.lookup_by_agent_id(agent_id))
    })
}
