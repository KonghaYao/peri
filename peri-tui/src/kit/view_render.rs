//! SubAgent 运行时状态探针——供 render 阶段查询 SubAgent 的实时状态。
//!
//! 旧 view_render.rs 的 7 个变体渲染函数已迁移到 `bubbles/` 组件。
//! 仅保留 SubAgentStatusProbe + with_status_probe 基础设施。

use std::cell::RefCell;
use std::rc::Rc;

use crate::kit::tui_render_unit::TuiRenderUnit;

// ── SubAgent 运行时状态探针（thread-local） ─────────────────────────────────

/// 避免传递到 render 函数，通过 thread-local 注入。
/// 由 app 层通过 [`with_status_probe`] 注入；render_subagent_group 通过
/// [`lookup_subagent_status`] 查询。
thread_local! {
    static STATUS_PROBE: RefCell<Option<Rc<dyn SubAgentStatusProbe>>> = const { RefCell::new(None) };
}

/// SubAgent 渲染时需要的运行时状态快照。
#[derive(Debug, Clone)]
pub struct SubAgentRenderInfo {
    pub is_running: bool,
    pub is_error: bool,
    pub final_result: Option<String>,
    pub recent_messages: Vec<TuiRenderUnit>,
}

/// V2 TuiSubAgentGroup 状态查询接口。app 层实现并通过 [`with_status_probe`] 设置。
pub trait SubAgentStatusProbe {
    /// 按 agent_id 查询运行时信息。
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo>;
}

/// 在 closure 内设置 status probe，closure 结束后自动恢复（支持嵌套）。
pub fn with_status_probe<R>(probe: Rc<dyn SubAgentStatusProbe>, f: impl FnOnce() -> R) -> R {
    let prev = STATUS_PROBE.with(|cell| cell.replace(Some(probe)));
    let result = f();
    STATUS_PROBE.with(|cell| {
        let _ = cell.replace(prev);
    });
    result
}

/// render_subagent_group 内部使用：按 agent_id 查询运行时状态。
pub fn lookup_subagent_status(agent_id: &str) -> Option<SubAgentRenderInfo> {
    STATUS_PROBE.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|probe| probe.lookup_by_agent_id(agent_id))
    })
}
