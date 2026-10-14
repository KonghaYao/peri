use std::collections::HashMap;

use peri_acp_types::mcp_apps::AppSessionBinding;
use peri_acp_types::PeriCaps;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionLifecycle {
    Open,
    Closing,
    Closed,
}

pub(crate) struct ConnectionContext {
    id: String,
    lifecycle: ConnectionLifecycle,
    initialized: bool,
    /// 本连接 `initialize` 协商出的 peri caps。
    ///
    /// 进程级的 `SessionManager::pending_caps` 会被后一个连接覆盖，会话级
    /// `caps_registry` 又会把协商值带给重连的同一会话；两者都不能回答
    /// 「**当前连接**声明了什么」。需要按连接分流的判定（历史分页等）读这里。
    negotiated_caps: Option<PeriCaps>,
    apps_enabled: bool,
    app_sessions: HashMap<String, AppSessionBinding>,
    cancellation: tokio_util::sync::CancellationToken,
}

impl ConnectionContext {
    pub(crate) fn new(apps_enabled: bool) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            lifecycle: ConnectionLifecycle::Open,
            initialized: false,
            negotiated_caps: None,
            apps_enabled,
            app_sessions: HashMap::new(),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(crate) fn commit_initialize(&mut self, caps: PeriCaps) {
        if !self.initialized && self.lifecycle == ConnectionLifecycle::Open {
            self.initialized = true;
            self.negotiated_caps = Some(caps);
        }
    }

    /// 本连接协商出的 peri caps。
    ///
    /// 未调用 `initialize` 的进程内路径（MpscTransport / TUI）保持历史的全能力语义，
    /// 与 `SessionManager::effective_host_caps` 同一判据：stdio 与外部连接必须显式协商，
    /// 未协商即全 false。
    pub(crate) fn caps(&self) -> PeriCaps {
        self.negotiated_caps
            .clone()
            .unwrap_or_else(PeriCaps::all_enabled)
    }

    /// 历史分页能力：**当前连接**是否协商过它（REQ §5.3、验收 A25）。
    ///
    /// 与 [`Self::caps`] 刻意不同：[`Self::caps`] 是「进程内路径保持历史全能力」的展示判据，
    /// 未协商的连接在那里拿到的是一份**兜底**能力集；分页是**准入**判定，兜底会把
    /// 「从未协商」变成「默认获得分页语义」——那样的连接会收到游标、拿到 `snapshotToken`，
    /// 并按分页语义解析 load，正是 REQ §5.3 要挡住的静默降级。因此这里失败关闭：
    /// 没有成功的 `initialize`（或声明里没有该键）一律 false。
    ///
    /// 调用约定：`session/load` 的模式分流（[`crate::host::history_page::classify`]）与
    /// 初始 load 的游标附加（[`crate::host::history_page::attach_initial_cursor`]）**必须**
    /// 用本方法判定，不得改用 [`Self::caps`] 或进程/会话级 caps 注册表——那两者会把别的
    /// 连接（或旧连接）的协商值带给当前连接。
    pub(crate) fn history_pagination(&self) -> bool {
        self.initialized
            && self
                .negotiated_caps
                .as_ref()
                .is_some_and(|caps| caps.history_pagination)
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        self.initialized && self.lifecycle == ConnectionLifecycle::Open && self.apps_enabled
    }

    #[allow(dead_code)]
    pub(crate) fn insert_app_session(&mut self, binding: AppSessionBinding) -> bool {
        if self.apps_enabled() && binding.owner_connection_id == self.id {
            self.app_sessions
                .insert(binding.app_session_id.clone(), binding);
            true
        } else {
            false
        }
    }

    pub(crate) fn app_session(&self, id: &str) -> Option<&AppSessionBinding> {
        self.apps_enabled()
            .then(|| self.app_sessions.get(id))
            .flatten()
    }

    pub(crate) fn snapshot_for_request(&self) -> Self {
        Self {
            id: self.id.clone(),
            lifecycle: self.lifecycle,
            initialized: self.initialized,
            negotiated_caps: self.negotiated_caps.clone(),
            apps_enabled: self.apps_enabled,
            app_sessions: self.app_sessions.clone(),
            cancellation: self.cancellation.clone(),
        }
    }

    pub(crate) fn cancellation(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn begin_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closing;
        self.cancellation.cancel();
        self.app_sessions.clear();
    }

    #[allow(dead_code)]
    pub(crate) fn finish_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closed;
        self.app_sessions.clear();
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// 请求路由可依赖的连接事实快照（一次请求取一份）。
    ///
    /// 只装**按连接分流**的判定结果：分页准入（失败关闭，见 [`Self::history_pagination`]）与
    /// 本连接的协商能力集（[`Self::caps`]，未 `initialize` 的进程内路径是兜底的全能力）。
    /// 路由每个请求都会取一份，因此不整份 [`Self::snapshot_for_request`]（它还要克隆 app
    /// session 表，路由不需要），也不能传 `&ConnectionContext`（它在本循环里由 `Mutex` 持有，
    /// 跨请求派发持锁会与 `initialize` 的 `commit_initialize` 互相等待）。
    ///
    /// 两个字段刻意并存、不可互相推导：分页是**准入**（兜底会把「从未协商」变成「默认获得
    /// 分页语义」），回放标记（`periReplay` / `periLoadGeneration`）是**展示**能力
    /// （未 `initialize` 的 MpscTransport / TUI 保持历史全能力）。两者都只回答
    /// 「**当前连接**声明了什么」，不回读进程 / 会话级 caps 注册表。
    pub(crate) fn facts(&self) -> ConnectionFacts {
        ConnectionFacts {
            history_pagination: self.history_pagination(),
            caps: self.caps(),
        }
    }
}

/// 请求路由需要的连接事实快照，见 [`ConnectionContext::facts`]。
#[derive(Clone, Debug)]
pub(crate) struct ConnectionFacts {
    /// 历史分页是否由**当前连接**协商（REQ §5.3、验收 A25）。失败关闭。
    pub(crate) history_pagination: bool,
    /// **当前连接**的 peri 能力集（[`ConnectionContext::caps`] 的原样取值）。
    ///
    /// 回放标记与代次戳（`periReplay` / `periLoadGeneration`）由它门控：判据必须是本连接
    /// `initialize` 的协商结果，不得回读会话级 `caps_registry`——后者的旧值会被先前连接写死，
    /// 未协商的标准连接因此可能继承到 Peri 私有字段（REQ §5.3、验收 A25）。
    pub(crate) caps: PeriCaps,
}

impl ConnectionFacts {
    /// 没有连接上下文的调用方（进程内直接调用、单测）：按**未协商**处理。
    ///
    /// 这类调用方没有连接，也就没有协商事实可以继承——分页准入因此 false，正是
    /// REQ §5.3 要求的失败关闭方向。展示能力集按 [`ConnectionContext::caps`] 的同一判据取
    /// `all_enabled`：没有连接 = 进程内路径（MpscTransport / TUI）的历史全能力语义，回放标记
    /// 因此照常出现；两者来源不同，不能用一个布尔值互相替代。
    #[cfg(test)]
    pub(crate) fn unnegotiated() -> Self {
        Self {
            history_pagination: false,
            caps: PeriCaps::all_enabled(),
        }
    }
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
