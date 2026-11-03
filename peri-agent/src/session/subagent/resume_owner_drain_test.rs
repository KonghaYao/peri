use super::*;

pub(super) struct FailOnStopBridge;

impl crate::agent::LangfuseBridgeLike for FailOnStopBridge {
    fn process_render_event(&self, _event: &crate::agent::events_v2::RenderEvent) {}

    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            panic!("测试中的事件排空失败");
        }
    }
}

/// [回归测试] 同步模型失败已交接后，终态事件排空失败仍拒绝恢复。
#[tokio::test(start_paused = true)]
async fn test_sync_confirmed_handoff_event_drain_failure_blocks_resume() {
    test_failed_model_handoff_then_resume(false, true).await;
}

/// [回归测试] 后台模型失败已交接后，终态事件排空失败仍拒绝恢复。
#[tokio::test(start_paused = true)]
async fn test_background_confirmed_handoff_event_drain_failure_blocks_resume() {
    test_failed_model_handoff_then_resume(true, true).await;
}
