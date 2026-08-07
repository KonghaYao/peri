//! Langfuse 桥接器抽象 trait。
//!
//! peri-agent 层通过此 trait 消费 v2 事件，无需依赖 peri-controller。
//! peri-controller 的 `LangfuseBridge` impl 此 trait，内部完成
//! RenderEvent/ObserveEvent → UnifiedLangfuseEvent → LangfuseTracer 的映射链路。

/// Langfuse 桥接器抽象。
///
/// peri-agent 层通过此 trait 消费 v2 事件，无需依赖 peri-controller。
/// peri-controller 的 `LangfuseBridge` impl 此 trait，内部完成
/// RenderEvent/ObserveEvent → UnifiedLangfuseEvent → LangfuseTracer 的映射链路。
///
/// L5：`Any` 上界供 ACP 装配面 downcast（`Arc<dyn LangfuseBridgeLike>` →
/// `Arc<LangfuseBridge>`，executor 迁入 peri-agent 后观测旁路桥构造留在
/// ACP 宿主侧）。
pub trait LangfuseBridgeLike: Send + Sync + std::any::Any {
    /// 处理 RenderEvent，映射为 Langfuse 追踪事件。
    ///
    /// 调用时机：SubAgent 事件转发器的 render 分支内，
    /// 在 `ev` 被 `render_event_to_executor(ev)` move 之前。
    fn process_render_event(&self, ev: &crate::agent::events_v2::RenderEvent);

    /// 处理 ObserveEvent，映射为 Langfuse 追踪事件。
    ///
    /// 调用时机：SubAgent 事件转发器的 observe 分支内，
    /// 在 `ev` 被 `observe_event_to_executor(ev)` move 之前。
    fn process_observe_event(&self, ev: &crate::agent::events_v2::ObserveEvent);
}
