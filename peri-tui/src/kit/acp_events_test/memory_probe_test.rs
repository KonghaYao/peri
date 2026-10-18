//! S1：TUI bridge 对象计量只读诊断。
//!
//! 只验证 committed / current_turn / folded_history 的条目与字节可区分；
//! 不改变任何释放/折叠语义。

use super::*;

use std::sync::Arc;

#[test]
fn bridge_snapshot_separates_committed_current_and_folded_bytes() {
    let mut state = make_fold_test_state();

    let committed_text = "committed-body-".repeat(16);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            committed_text.clone(),
        )));
    let assistant_text = "assistant-body-".repeat(32);
    state
        .committed
        .push_back(TuiRenderUnit::TuiAssistantBubble(Arc::new(
            TuiAssistantBubble {
                text: assistant_text.clone(),
                reasoning: None,
                message_id: None,
                started_at: None,
                duration_ms: None,
                content_hash: 0,
            },
        )));
    let current_text = "current-turn-body-".repeat(24);
    state.current_turn.append_text(&current_text, None);

    let snapshot = state.memory_snapshot();
    println!("Bridge T2=[{snapshot:?}]");

    assert_eq!(snapshot.committed_entries, 2);
    assert_eq!(snapshot.current_turn_entries, 1);
    assert_eq!(snapshot.folded_entries, 2);
    assert!(
        snapshot.committed_body_bytes >= committed_text.len() + assistant_text.len(),
        "committed 层必须计入两条正文"
    );
    assert!(
        snapshot.current_turn_body_bytes >= current_text.len(),
        "current_turn 层必须计入流式正文"
    );
    assert!(
        snapshot.folded_body_bytes >= committed_text.len() + assistant_text.len(),
        "folded 层必须与 committed 一样覆盖投影正文"
    );
    assert!(
        snapshot.unique_body_bytes
            <= snapshot.committed_body_bytes
                + snapshot.current_turn_body_bytes
                + snapshot.folded_body_bytes,
        "unique 是跨层去重后的下界，不能大于三层求和"
    );

    // 追加 current 正文不改变 committed / folded 的条目与字节。
    let committed_before = snapshot.committed_body_bytes;
    let folded_before = snapshot.folded_body_bytes;
    state.current_turn.append_text(&"next-".repeat(8), None);
    let after = state.memory_snapshot();
    assert_eq!(after.committed_entries, snapshot.committed_entries);
    assert_eq!(after.committed_body_bytes, committed_before);
    assert_eq!(after.folded_entries, snapshot.folded_entries);
    assert_eq!(after.folded_body_bytes, folded_before);
    assert!(
        after.current_turn_body_bytes > snapshot.current_turn_body_bytes,
        "current_turn 层必须反映新增流式正文"
    );
}
