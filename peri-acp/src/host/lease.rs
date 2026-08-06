//! 多读者 + 单 writer lease（ACP Host 侧会话写入权）。
//!
//! 背景（`docs/top-level.md` §8 / 伞形 PRD 未决项「多客户端 attach」）：
//! 多客户端 attach 采用多读者 + 单 writer lease——观察者只读（订阅事件），
//! 操作者唯一（可提交输入/取消）。本模块在 host 的 mpsc 通道层实现该策略，
//! **不进 wire format**（协议级扩展另立 issue）。
//!
//! 当前协议尚无客户端身份字段，writer 恒为 session 创建方（`"default"`），
//! lease 校验为恒真挂接点；机制语义由本模块单测覆盖。未来引入客户端身份
//! 后，host 侧校验点（prompt/cancel）直接按 client_id 判定即可。

use std::sync::{Arc, Mutex};

/// 单 writer 租赁：同一时刻至多一个客户端持有写入权。
///
/// - `try_acquire`：首个调用者成为 writer，其余失败（观察者只读）；
/// - `is_writer`：写入操作（prompt/cancel）入口校验；
/// - `release`：session 销毁或 writer 退出时释放，后续可重新获取。
#[derive(Debug)]
pub struct WriterLease {
    writer: Arc<Mutex<Option<String>>>,
}

impl WriterLease {
    /// 创建无 writer 的 lease。
    pub fn new() -> Self {
        Self {
            writer: Arc::new(Mutex::new(None)),
        }
    }

    /// 创建并立即由 `client_id` 持有 writer 权（session 创建方即 writer）。
    pub fn acquired(client_id: &str) -> Self {
        let lease = Self::new();
        assert!(
            lease.try_acquire(client_id),
            "fresh lease must be acquirable"
        );
        lease
    }

    /// 尝试获取 writer 权。已有 writer 时失败（`false`）。
    pub fn try_acquire(&self, client_id: &str) -> bool {
        let mut guard = self.writer.lock().unwrap();
        if guard.is_some() {
            return false;
        }
        *guard = Some(client_id.to_string());
        true
    }

    /// `client_id` 是否为当前 writer（写入操作唯一入口判定）。
    pub fn is_writer(&self, client_id: &str) -> bool {
        self.writer.lock().unwrap().as_deref() == Some(client_id)
    }

    /// 释放 writer 权。仅当前 writer 可释放；返回是否释放成功。
    pub fn release(&self, client_id: &str) -> bool {
        let mut guard = self.writer.lock().unwrap();
        if guard.as_deref() != Some(client_id) {
            return false;
        }
        *guard = None;
        true
    }

    /// 当前 writer 标识（调试用）。
    pub fn writer(&self) -> Option<String> {
        self.writer.lock().unwrap().clone()
    }
}

impl Default for WriterLease {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquirer_becomes_writer_second_fails() {
        let lease = WriterLease::new();
        assert!(lease.try_acquire("client-a"));
        // 第二个客户端只能观察，不能成为 writer
        assert!(!lease.try_acquire("client-b"));
        assert!(lease.is_writer("client-a"));
        assert!(!lease.is_writer("client-b"));
    }

    #[test]
    fn release_allows_reacquisition() {
        let lease = WriterLease::new();
        assert!(lease.try_acquire("client-a"));
        // 非 writer 不能释放
        assert!(!lease.release("client-b"));
        assert!(lease.release("client-a"));
        // 释放后新客户端可获取
        assert!(lease.try_acquire("client-c"));
        assert!(lease.is_writer("client-c"));
    }

    #[test]
    fn acquired_holds_writer_immediately() {
        let lease = WriterLease::acquired("default");
        assert!(lease.is_writer("default"));
        assert!(!lease.try_acquire("other"));
    }

    #[test]
    fn write_operations_gate_on_writer() {
        // 模拟 host 校验点：非 writer 的 prompt/cancel 被拒绝。
        let lease = WriterLease::acquired("default");
        assert!(lease.is_writer("default"), "writer 可提交输入/取消");
        assert!(!lease.is_writer("observer"), "观察者只读");
    }
}
