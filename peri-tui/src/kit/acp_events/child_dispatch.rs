//! Child-origin-aware dispatch entry points.
//!
//! Kept out of `acp_events::mod` so the origin-neutral wrapper and test helper
//! do not grow the core dispatch module past its file-size budget.

use super::*;

#[cfg(test)]
pub(crate) fn dispatch_trusted_structured_for_bridge(
    state: &mut BridgeState,
    event: AcpEventData,
) -> PublicationIntent {
    dispatch_trusted_structured_for_bridge_with_origin(state, event, None)
}

pub(crate) fn dispatch_trusted_structured_for_bridge_with_origin(
    state: &mut BridgeState,
    event: AcpEventData,
    child_origin: Option<&ChildEventOrigin>,
) -> PublicationIntent {
    match event {
        AcpEventData::SystemReminder {
            reminder,
            source_agent_id,
            ..
        } => {
            system::handle_trusted_system_reminder(state, reminder, source_agent_id.as_deref());
            std::mem::take(&mut state.publication_intent)
        }
        other => dispatch_for_bridge_with_origin(state, &other, child_origin),
    }
}

/// 同步测试/非 bridge 调用的兼容入口；生产 bridge 使用 `dispatch_for_bridge`
/// 消费 publication intent 并执行合帧。
#[cfg(test)]
pub(crate) fn dispatch_and_notify(state: &mut BridgeState, event: &AcpEventData) {
    if dispatch_for_bridge(state, event) != PublicationIntent::Published {
        render::push_view_models(state);
    }
    crate::kit::bg_task_live::publish_pending_streams();
}
