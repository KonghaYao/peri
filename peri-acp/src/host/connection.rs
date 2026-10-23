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
    pub(crate) acp_mcp: std::sync::Arc<peri_acp_types::acp_mcp::AcpMcpRegistry>,
    /// 本连接的 History V2 服务（窗口与幂等账本不跨连接共享）。
    pub(crate) history_v2: std::sync::Arc<super::history_service::HistoryV2Service>,
    /// 本连接的实时投递注册表（按会话索引；连接关闭时整体清理）。
    pub(crate) live: std::sync::Arc<super::live_delivery::LiveDelivery>,
    /// 测试专用历史后端注入点；生产连接恒为 `None`（`#[cfg(test)]`，不进入 API）。
    #[cfg(test)]
    pub(crate) history_v2_backend_override:
        Option<std::sync::Arc<dyn peri_acp_types::history_v2::HistoryV2Backend>>,
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
            acp_mcp: Default::default(),
            history_v2: std::sync::Arc::new(super::history_service::HistoryV2Service::system()),
            live: std::sync::Arc::new(super::live_delivery::LiveDelivery::new()),
            #[cfg(test)]
            history_v2_backend_override: None,
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

    /// Admission and notifications require this open connection's initialize result.
    pub(crate) fn admission_caps(&self) -> PeriCaps {
        if self.initialized && self.lifecycle == ConnectionLifecycle::Open {
            self.negotiated_caps.clone().unwrap_or_default()
        } else {
            PeriCaps::default()
        }
    }

    /// `peri.history` V2 准入：只有本连接成功协商出 `HistoryServerCapability` 才为 true。
    ///
    /// 未 `initialize`、未请求 V2，或协商失败（不提交 caps）都不会把连接变成 V2。
    /// 后续 `_peri/history` 分流必须读本方法。
    pub(crate) fn history_v2(&self) -> bool {
        self.initialized
            && self
                .negotiated_caps
                .as_ref()
                .and_then(|caps| caps.history_v2.as_ref())
                .is_some()
    }

    pub(crate) fn mcp_control_v1(&self) -> bool {
        self.initialized
            && self.lifecycle == ConnectionLifecycle::Open
            && self
                .negotiated_caps
                .as_ref()
                .is_some_and(|caps| caps.mcp_control_v1)
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        self.initialized && self.lifecycle == ConnectionLifecycle::Open && self.apps_enabled
    }

    pub(crate) fn snapshot_for_request(&self) -> Self {
        Self {
            id: self.id.clone(),
            lifecycle: self.lifecycle,
            initialized: self.initialized,
            negotiated_caps: self.negotiated_caps.clone(),
            apps_enabled: self.apps_enabled,
            acp_mcp: self.acp_mcp.clone(),
            history_v2: self.history_v2.clone(),
            live: self.live.clone(),
            #[cfg(test)]
            history_v2_backend_override: self.history_v2_backend_override.clone(),
            cancellation: self.cancellation.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_history_v2_backend_override(
        &mut self,
        backend: std::sync::Arc<dyn peri_acp_types::history_v2::HistoryV2Backend>,
    ) {
        self.history_v2_backend_override = Some(backend);
    }

    #[cfg(test)]
    pub(crate) fn history_v2_backend_for_test(
        &self,
    ) -> Option<std::sync::Arc<dyn peri_acp_types::history_v2::HistoryV2Backend>> {
        self.history_v2_backend_override.clone()
    }

    pub(crate) fn cancellation(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn begin_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closing;
        self.cancellation.cancel();
    }

    #[allow(dead_code)]
    pub(crate) fn finish_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closed;
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// 请求路由可依赖的连接事实快照（一次请求取一份）。
    ///
    /// 只装**按连接分流**的判定结果与本连接的协商能力集。
    /// 路由每个请求都会取一份，因此不整份 [`Self::snapshot_for_request`]（它还要克隆 app
    /// session 表，路由不需要），也不能传 `&ConnectionContext`（它在本循环里由 `Mutex` 持有，
    /// 跨请求派发持锁会与 `initialize` 的 `commit_initialize` 互相等待）。
    pub(crate) fn facts(&self) -> ConnectionFacts {
        ConnectionFacts {
            history_v2: self.history_v2(),
            mcp_control_v1: self.mcp_control_v1(),
            caps: self.caps(),
            admission_caps: self.admission_caps(),
        }
    }
}

/// 请求路由需要的连接事实快照，见 [`ConnectionContext::facts`]。
#[derive(Clone, Debug)]
pub(crate) struct ConnectionFacts {
    /// `peri.history` V2 是否由**当前连接**成功协商（存在服务端回显）。失败关闭。
    pub(crate) history_v2: bool,
    pub(crate) mcp_control_v1: bool,
    /// **当前连接**的 peri 能力集（[`ConnectionContext::caps`] 的原样取值）。
    pub(crate) caps: PeriCaps,
    /// Explicit negotiation for admission; absent or closed connections have no capabilities.
    pub(crate) admission_caps: PeriCaps,
}

impl ConnectionFacts {
    /// 没有连接上下文的调用方（进程内直接调用、单测）：按**未协商**处理。
    ///
    /// 这类调用方没有连接，也就没有协商事实可以继承——V2 准入因此 false。
    #[cfg(test)]
    pub(crate) fn unnegotiated() -> Self {
        Self {
            history_v2: false,
            mcp_control_v1: false,
            caps: PeriCaps::all_enabled(),
            admission_caps: PeriCaps::default(),
        }
    }
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
