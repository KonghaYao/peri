//! RewindCommand 事件发出辅助函数。
//!
//! Phase 5 Step 5：RewindError 变体已删除（解析失败 / 未找到目标改
//! `feedback(Error, UiOnly)` 收敛，emit_rewind_parse_error / emit_rewind_not_found
//! 随之退役）；本模块仅保留 `RewindCompleted` 发射模板（重建信号，保留原样）。

use std::sync::Arc;

use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;

use crate::session::event_sink::EventSink;

/// 发出回滚完成事件。
pub async fn emit_rewind_completed(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    summary: String,
    messages: Vec<BaseMessage>,
) {
    sink.push_event(
        session_id,
        &ExecutorEvent::RewindCompleted { summary, messages },
        0,
    )
    .await;
}
