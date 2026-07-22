//! Tests for compact

use super::*;
use crate::agent::stages::StageContext;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

#[tokio::test]
async fn test_compact_without_budget_skips() {
    // 无 context_budget → 跳过
    let ctx = make_context();
    let input = CompactInput {
        context: ctx,
        has_tool_calls: false,
    };
    let output = run_compact(input).await.unwrap();
    assert!(!output.compacted);
}
