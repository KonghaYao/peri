use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
use crate::kit::tui_render_unit::{EntryStatus, FoldState, TuiReasoningBlock, TuiUserBubble};

fn body(unit: &TuiRenderUnit) -> &Arc<TuiAssistantBubble> {
    let TuiRenderUnit::TuiAssistantBubble(bubble) = unit else {
        panic!("expected assistant");
    };
    bubble
}

#[test]
fn persistent_vector_cow_shares_large_assistant_payloads() {
    let unit = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            text: "中文正文".repeat(16_384),
            reasoning: Some(TuiReasoningBlock {
                text: "reasoning".repeat(16_384),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: None,
            }),
            message_id: None,
            started_at: None,
            duration_ms: None,
            content_hash: 42,
        }
        .into(),
    );
    let original = im::Vector::from(vec![unit; 128]);
    reset_perf_counters();
    for _ in 0..32 {
        let mut published = original.clone();
        published.push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "next".into(),
        )));
        published.set(
            1,
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("replace adjacent item".into())),
        );
        assert!(Arc::ptr_eq(body(&published[0]), body(&original[0])));
        assert!(Arc::ptr_eq(body(&published[127]), body(&original[127])));
    }
    assert_eq!(perf_counters().assistant_clone_calls, 0);
    assert_eq!(perf_counters().assistant_clone_bytes, 0);
}
