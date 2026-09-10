//! initialize 协商与 session capability 投影；状态仍由 SessionManager 唯一持有。

use std::sync::Arc;

use dashmap::DashMap;
use peri_acp_types::PeriCaps;

use super::SessionManager;

impl SessionManager {
    /// initialize handler 调用：暂存 clientCapabilities 中的 peri caps。
    pub fn set_pending_caps(&self, caps: PeriCaps) {
        *self.inner.pending_caps.lock() = Some(caps);
    }

    /// 查询 initialize 是否已被调用（pending_caps 是否被设置过）。
    /// 用于 MpscTransport 路径判断：若未调用 initialize，默认全部 cap=true。
    pub fn pending_caps_was_set(&self) -> bool {
        self.inner.pending_caps.lock().is_some()
    }

    /// 返回当前 ACP 连接在 initialize 阶段协商的进程级能力。
    /// host 级事件没有 session identity，必须读取此快照而不能回退到任意
    /// session registry 条目。
    pub fn negotiated_caps(&self) -> PeriCaps {
        self.inner.pending_caps.lock().clone().unwrap_or_default()
    }

    /// Host 请求面的有效能力：stdio/外部连接必须显式协商；未调用
    /// initialize 的进程内 MPSC/TUI 路径保持历史的全能力语义。
    pub fn effective_host_caps(&self) -> PeriCaps {
        self.inner
            .pending_caps
            .lock()
            .clone()
            .unwrap_or_else(PeriCaps::all_enabled)
    }

    /// session/new 时调用：将暂存的 caps 关联到 session_id，返回 caps 副本。
    /// 如果 initialize 时未声明任何 caps，返回默认值（全 false）。
    ///
    /// S1.1：改为 clone 而非 take —— 协商值是 server 进程级配置（initialize 只
    /// 调用一次），必须保留供第 2+ 个 session 复用；否则 stdio 第 2 个
    /// session/new 会取到 None 注册全 false caps。
    pub fn consume_pending_caps(&self, session_id: &str) -> PeriCaps {
        let caps = self.inner.pending_caps.lock().clone().unwrap_or_default();
        self.inner
            .caps_registry
            .insert(session_id.to_string(), caps.clone());
        caps
    }

    /// Sending point 调用：读取 session 的 peri caps。
    /// 未设置时返回默认值（全 false）。
    pub fn get_caps(&self, session_id: &str) -> PeriCaps {
        self.inner
            .caps_registry
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// 获取 caps_registry 的 Arc clone，用于传递给 TransportEventSink
    /// 等需独立访问 registry 的组件。
    pub fn caps_registry(&self) -> Arc<DashMap<String, PeriCaps>> {
        self.inner.caps_registry.clone()
    }

    /// 确保指定 session 的 caps 已在 registry 中注册。
    ///
    /// - registry 已有条目 → 直接返回（幂等）。
    /// - 已协商过（`pending_caps_was_set()`，stdio 路径经过 initialize）→
    ///   用协商值 clone 并写入。
    /// - 未协商（MpscTransport / TUI 内部路径，无 initialize）→ 写入 `all_enabled()`。
    ///
    /// S1.1：协商值不再被 consume 清空，因此 load/resume/fork 新 session id
    /// 也能拿到协商值；未协商才回退 all_enabled（与 `consume_pending_caps`
    /// 未协商回退全 false 的语义刻意不同，两侧不可互换）。
    ///
    /// 幂等：重复调用不会覆盖已有值。
    /// 与 `consume_pending_caps` 的 lock 独立操作，避免 TOCTOU 竞态。
    pub fn ensure_session_caps(&self, session_id: &str) -> PeriCaps {
        // 已有注册 → 直接返回（幂等）
        if let Some(caps) = self.inner.caps_registry.get(session_id) {
            return caps.clone();
        }
        // 协商过 → 用协商值 clone；未协商 → 默认全启用。
        // pending_caps 只被 set 从不被 take/clear，was_set 检查后 clone 无竞态。
        let caps = if self.pending_caps_was_set() {
            self.inner.pending_caps.lock().clone().unwrap_or_default()
        } else {
            PeriCaps::all_enabled()
        };
        self.inner
            .caps_registry
            .insert(session_id.to_string(), caps.clone());
        caps
    }
}
