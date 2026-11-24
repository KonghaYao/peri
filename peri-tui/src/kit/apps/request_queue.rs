//! `app-request` 的 wire 类型与有界队列契约。
//!
//! runtime 的每视图中继队列用同等容量的有界 `mpsc`（容量取自
//! [`DEFAULT_APP_REQUEST_CAPACITY`]），保证「满则回填错误、不静默丢弃」；
//! 本模块的 [`AppRequestQueue`] 固定同样的 FIFO/容量语义并作为单测基准。

use std::collections::VecDeque;

use serde_json::Value;

/// 单条 App → 宿主请求（网关 `app-request` 事件正文）。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAppRequest {
    /// 网关分配的去重/回填序号（同一会话内递增）。
    pub seq: u64,
    /// App 自己的 JSON-RPC id（原样回填）。
    pub id: Value,
    /// 方法名（中继只接受 `tools/call`）。
    pub method: String,
    /// 参数（`tools/call` 为 `{name, arguments}`）。
    pub params: Value,
}

impl PendingAppRequest {
    /// wire 解码：只保留可安全展示/转发的字段。
    ///
    /// 方法名限制为短 ASCII 标识符（MCP 方法名），过长或含控制字符视为非法。
    #[must_use]
    pub fn from_wire(value: Value) -> Option<Self> {
        let object = value.as_object()?;
        let seq = object.get("seq")?.as_u64()?;
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        let method = object.get("method")?.as_str()?.trim();
        if method.is_empty()
            || method.len() > 128
            || !method
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
        {
            return None;
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(Self {
            seq,
            id,
            method: method.to_string(),
            params,
        })
    }
}

/// 每视图中继队列容量（与网关 `MAX_PENDING_REQUESTS` 的 64 不同：
/// 宿主侧只需保留可在一帧内处理的部分，超限显式丢弃并允许 App 收到错误）。
pub const DEFAULT_APP_REQUEST_CAPACITY: usize = 32;

/// 有界 FIFO：宿主逐条取出执行并回填。
#[derive(Debug)]
pub struct AppRequestQueue {
    pending: VecDeque<PendingAppRequest>,
    capacity: usize,
    dropped: u64,
}

impl Default for AppRequestQueue {
    fn default() -> Self {
        Self::new(DEFAULT_APP_REQUEST_CAPACITY)
    }
}

impl AppRequestQueue {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    /// 入队；表满返回 `false`（运行时据此回填 `app_request_overflow`）。
    pub fn enqueue(&mut self, request: PendingAppRequest) -> bool {
        if self.pending.len() >= self.capacity {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.pending.push_back(request);
        true
    }

    /// 取下一个待回填请求。
    pub fn take_next(&mut self) -> Option<PendingAppRequest> {
        self.pending.pop_front()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// 因表满被丢弃的请求数（UI 提示与错误回填统计）。
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
#[path = "request_queue_test.rs"]
mod tests;
