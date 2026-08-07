//! 中间件链执行追踪器。
//!
//! 追踪中间件链中每个中间件的执行 span：
//! - on_start：开始中间件 span，返回 MiddlewareSpanHandle
//! - on_end：结束中间件 span，返回 MiddlewareEndRecord
//!
//! 支持同一 hook 并发执行多个中间件（通过 span_id 保证配对正确）。

use peri_agent::agent::events::{MiddlewareHook, StageStatus};
use std::collections::HashMap;

pub(crate) struct MiddlewareSpanHandle {
    pub span_id: String,
    pub name: String,
    pub hook: MiddlewareHook,
}

struct ActiveMiddleware {
    name: String,
    hook: MiddlewareHook,
    start_time: String,
}

pub(crate) struct MiddlewareEndRecord {
    pub span_id: String,
    pub name: String,
    pub hook: MiddlewareHook,
    pub start_time: String,
    pub status: StageStatus,
    pub is_error: bool,
}

pub(crate) struct MiddlewareTracer {
    active: HashMap<String, ActiveMiddleware>,
}

impl MiddlewareTracer {
    pub(crate) fn new() -> Self {
        Self {
            active: HashMap::new(),
        }
    }

    pub(crate) fn on_start(&mut self, name: &str, hook: MiddlewareHook) -> MiddlewareSpanHandle {
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        self.active.insert(
            span_id.clone(),
            ActiveMiddleware {
                name: name.to_string(),
                hook,
                start_time: start_time.clone(),
            },
        );
        MiddlewareSpanHandle {
            span_id,
            name: name.to_string(),
            hook,
        }
    }

    pub(crate) fn on_end(
        &mut self,
        handle: &MiddlewareSpanHandle,
        status: StageStatus,
        _error: Option<String>,
    ) -> Option<MiddlewareEndRecord> {
        let active = self.active.remove(&handle.span_id)?;
        Some(MiddlewareEndRecord {
            span_id: handle.span_id.clone(),
            name: active.name,
            hook: active.hook,
            start_time: active.start_time,
            status,
            is_error: status == StageStatus::Error,
        })
    }

    /// 按 name + hook 查找活跃中间件的 span_id（用于从外部事件重建 handle）
    pub(crate) fn find_active(&self, name: &str, hook: MiddlewareHook) -> Option<String> {
        self.active
            .iter()
            .find(|(_, m)| m.name == name && m.hook == hook)
            .map(|(span_id, _)| span_id.clone())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
