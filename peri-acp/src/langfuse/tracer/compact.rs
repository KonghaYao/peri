//! Compact 操作 Span 追踪器。
//!
//! 管理上下文压缩操作的单次 span 生命周期：
//! - on_start：开始 compact span（支持 double-start 覆盖）
//! - on_end：结束 compact span，返回 CompactSpanContext 供外层构造 IngestionEvent

use peri_agent::agent::events::{CompactStrategy, CompactTrigger};

pub(crate) struct CompactSpanStart {
    pub span_id: String,
    pub start_time: String,
}

pub(crate) struct CompactSpanContext {
    pub span_id: String,
    pub start_time: String,
    pub strategy: CompactStrategy,
    pub trigger: CompactTrigger,
}

pub(crate) struct CompactSpan {
    ctx: Option<CompactSpanContext>,
}

impl CompactSpan {
    pub(crate) fn new() -> Self {
        Self { ctx: None }
    }

    pub(crate) fn on_start(
        &mut self,
        strategy: CompactStrategy,
        trigger: CompactTrigger,
    ) -> CompactSpanStart {
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let start = CompactSpanStart {
            span_id: span_id.clone(),
            start_time: start_time.clone(),
        };
        self.ctx = Some(CompactSpanContext {
            span_id,
            start_time,
            strategy,
            trigger,
        });
        start
    }

    pub(crate) fn on_end(&mut self) -> Option<CompactSpanContext> {
        self.ctx.take()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.ctx.is_some()
    }
}

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;
