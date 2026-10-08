//! 一次性 hook 状态跟踪器。
//!
//! 将每个已触发的 once hook 用稳定 key 记录到 HashSet，
//! 后续命中同一 key 的 hook 会被跳过。

use std::collections::HashSet;

use parking_lot::Mutex;

use crate::hooks::types::{HookType, RegisteredHook};

/// Once-fired 状态管理：用 `HashSet<String>` 跟踪一次性 hook。
///
/// 由 `HookMiddleware` 通过 `Arc` 共享，可被 dispatcher 与 standalone
/// 路径同时访问。所有方法均为 `&self`，内部用 `parking_lot::Mutex`。
pub struct OnceTracker {
    fired: Mutex<HashSet<String>>,
}

impl OnceTracker {
    pub fn new() -> Self {
        Self {
            fired: Mutex::new(HashSet::new()),
        }
    }

    /// 判断给定 hook 是否是一次性 hook（依据 `HookType::is_once`）。
    pub fn is_once_hook(hook: &HookType) -> bool {
        hook.is_once()
    }

    /// 构造 once key：由 `plugin_id + hook 序列化 + event` 三元组组合，
    /// 保证同一 hook 配置在多次调用间稳定。
    pub fn once_key(registered: &RegisteredHook) -> String {
        format!(
            "{}:{}:{:?}",
            registered.plugin_id,
            serde_json::to_string(&registered.hook).unwrap_or_default(),
            registered.event
        )
    }

    /// 原子预留该 once hook：单锁内"查 + 插入"，返回 `true` 表示本次调用拿到了
    /// 执行权（首次触发），`false` 表示已被其它触发预留过、必须跳过。
    ///
    /// [TRAP] 禁止拆成 `was_fired()`（查）+ 执行后 `mark_fired()`（标记）：
    /// 两步之间锁已释放，生命周期闭包经 `tokio::spawn` 分离触发时，两次触发
    /// 会同时通过检查并各执行一次（once 失效）。预留即消费——即使本次执行
    /// 失败或被取消也不重试，符合 once 语义。
    pub fn try_reserve(&self, registered: &RegisteredHook) -> bool {
        let key = Self::once_key(registered);
        self.fired.lock().insert(key)
    }
}

impl Default for OnceTracker {
    fn default() -> Self {
        Self::new()
    }
}
