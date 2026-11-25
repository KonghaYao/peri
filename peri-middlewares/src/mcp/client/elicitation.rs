//! 外部 MCP Server 的 elicitation 归属与交互适配（单一权威）。
//!
//! MCP 的反向 `elicitation/create` 不携带 Peri 会话身份，只能落到「当前连接上
//! 正在执行的工具调用」。本模块维护该连接内 active call → 会话/归属/broker 的
//! 映射，并把 MCP 协议参数与 [`InteractionContext`] 互转。
//!
//! 解析 fail-closed：没有 active call，或同一连接上同时有多个 active call 时，
//! 一律返回 `decline`，不按时间/工具名猜测归属。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::interaction::{
    ElicitationDecision, ElicitationRequest, InteractionContext, InteractionFailure,
    InteractionResponse, TrustedInteractionOwner, UnansweredCause, UserInteractionBroker,
};
use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction, MetaObject};
use tokio_util::sync::CancellationToken;

/// 反向 elicitation 无可用归属时写入 `ElicitResult._meta` 的键。
pub(crate) const ELICITATION_UNAVAILABLE_META_KEY: &str = "io.peri/elicitationUnavailable";

/// 外部 MCP 连接的 elicitation 路由器。
///
/// 每个连接代一个实例：重连产生新 router，旧 lease 随工具调用结束自然失效，
/// 迟到请求不会被投递到新会话。
pub(crate) struct ElicitationRouter {
    server: String,
    generation: u64,
    state: parking_lot::Mutex<RouterState>,
    next_seq: AtomicU64,
    activity: Arc<ElicitationActivity>,
}

#[derive(Default)]
struct RouterState {
    active: BTreeMap<u64, ActiveCall>,
    /// URL elicitationId → 发起会话（2025-11-25 完成通知回看用）。
    url_routes: BTreeMap<String, String>,
    completed_urls: BTreeMap<String, String>,
}

#[derive(Clone)]
pub(crate) struct ActiveCall {
    pub(crate) session_id: String,
    pub(crate) tool: String,
    pub(crate) owner: TrustedInteractionOwner,
    pub(crate) broker: Arc<dyn UserInteractionBroker>,
    pub(crate) cancellation: CancellationToken,
}

/// 反向往返的解析结果；`Ambiguous` 只计数，不暴露内部调用细节。
pub(crate) enum RouteResolution {
    Unique(ActiveCall),
    NoActiveCall,
    Ambiguous { active: usize },
}

struct ElicitationActivity {
    asking: AtomicUsize,
    changed: tokio::sync::watch::Sender<u64>,
}

impl Default for ElicitationActivity {
    fn default() -> Self {
        Self {
            asking: AtomicUsize::new(0),
            changed: tokio::sync::watch::channel(0).0,
        }
    }
}

impl ElicitationActivity {
    fn enter(&self) {
        self.asking.fetch_add(1, Ordering::AcqRel);
        self.changed.send_modify(|version| *version += 1);
    }

    fn leave(&self) {
        self.asking.fetch_sub(1, Ordering::AcqRel);
        self.changed.send_modify(|version| *version += 1);
    }

    fn is_asking(&self) -> bool {
        self.asking.load(Ordering::Acquire) > 0
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

impl ElicitationRouter {
    pub(crate) fn new(server: impl Into<String>, generation: u64) -> Arc<Self> {
        Arc::new(Self {
            server: server.into(),
            generation,
            state: parking_lot::Mutex::new(RouterState::default()),
            next_seq: AtomicU64::new(1),
            activity: Arc::new(ElicitationActivity::default()),
        })
    }

    pub(crate) fn server(&self) -> &str {
        &self.server
    }

    /// 注册一次 active 工具调用；lease drop 时移除并取消仍在等待的交互。
    pub(crate) fn register(
        self: &Arc<Self>,
        session_id: impl Into<String>,
        tool: impl Into<String>,
        owner: TrustedInteractionOwner,
        broker: Arc<dyn UserInteractionBroker>,
        cancellation: CancellationToken,
    ) -> ElicitationLease {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        tracing::trace!(
            server = %self.server,
            generation = self.generation,
            seq,
            "registering MCP elicitation route"
        );
        self.state.lock().active.insert(
            seq,
            ActiveCall {
                session_id: session_id.into(),
                tool: tool.into(),
                owner,
                broker,
                cancellation: cancellation.clone(),
            },
        );
        ElicitationLease {
            router: Arc::clone(self),
            seq,
            activity: Arc::clone(&self.activity),
            activity_rx: std::sync::Mutex::new(self.activity.subscribe()),
            cancellation,
        }
    }

    pub(crate) fn resolve(&self) -> RouteResolution {
        let state = self.state.lock();
        match state.active.len() {
            0 => RouteResolution::NoActiveCall,
            1 => RouteResolution::Unique(
                state
                    .active
                    .values()
                    .next()
                    .expect("length checked")
                    .clone(),
            ),
            active => RouteResolution::Ambiguous { active },
        }
    }

    /// 记录 URL elicitation 与发起会话的关联（完成通知回看用）。
    pub(crate) fn note_url_route(&self, elicitation_id: &str, session_id: &str) {
        if elicitation_id.is_empty() {
            return;
        }
        self.state
            .lock()
            .url_routes
            .insert(elicitation_id.to_string(), session_id.to_string());
    }

    /// 处理 server → client 的 `notifications/elicitation/complete`。
    ///
    /// 2025-11-25 规范只允许 server 通知发起该 elicitation 的 client；本方法
    /// 找不到 id 时返回 `None`（规范要求忽略未知 id）。返回发起会话供日志。
    pub(crate) fn complete_url(&self, elicitation_id: &str) -> Option<String> {
        let mut state = self.state.lock();
        let session = state.url_routes.get(elicitation_id)?.clone();
        state
            .completed_urls
            .insert(elicitation_id.to_string(), session.clone());
        Some(session)
    }

    #[cfg(test)]
    pub(crate) fn completed_url_count(&self) -> usize {
        self.state.lock().completed_urls.len()
    }

    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.state.lock().active.len()
    }
}

/// 一次工具调用持有的 elicitation 路由租约。
pub(crate) struct ElicitationLease {
    router: Arc<ElicitationRouter>,
    seq: u64,
    activity: Arc<ElicitationActivity>,
    activity_rx: std::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
    cancellation: CancellationToken,
}

impl ElicitationLease {
    pub(crate) fn is_asking(&self) -> bool {
        self.activity.is_asking()
    }

    /// 活动变化订阅：elicitation 开始/结束时各推进一次版本。
    pub(crate) fn activity_receiver(&self) -> tokio::sync::watch::Receiver<u64> {
        self.activity_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 用本调用的 broker/归属发起一次 elicitation。
    ///
    /// `cancellation` 来自本次工具调用（父令牌 + 本 lease 的取消）；工具结束、
    /// 切会话或连接关闭时取消，broker future 立即丢弃并让 ACP transport 通过
    /// `$/cancel_request` 撤销客户端提示。
    #[allow(dead_code)] // MRTR 轮次在后续 commit 接入；反向路径必须保留同一入口。
    pub(crate) async fn ask(
        &self,
        params: &ElicitRequestParams,
    ) -> Result<MappedElicitation, ElicitationChannelFailure> {
        let route = {
            let state = self.router.state.lock();
            state.active.get(&self.seq).cloned()
        };
        let Some(route) = route else {
            return Ok(MappedElicitation {
                decision: ElicitationDecision::Cancel,
                unavailable: None,
            });
        };
        self.activity.enter();
        let answer = ask_elicitation(
            &*route.broker,
            route.owner.clone(),
            &self.router.server,
            &route.tool,
            params,
            &route.cancellation,
        )
        .await;
        self.activity.leave();
        answer
    }
}

impl Drop for ElicitationLease {
    fn drop(&mut self) {
        self.router.state.lock().active.remove(&self.seq);
        // 工具调用结束（成功 / 失败 / 取消 / 超时）时让仍在等待的反向请求
        // 以 cancel 结算，而不是继续占用会话交互门到 broker 超时。
        self.cancellation.cancel();
    }
}

/// MCP 参数映射后的答复；`unavailable` 表示没有可作答方（带真实原因）。
#[derive(Debug, Clone)]
pub(crate) struct MappedElicitation {
    pub decision: ElicitationDecision,
    pub unavailable: Option<UnansweredCause>,
}

/// 交互通道本身失败（非用户决定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElicitationChannelFailure {
    pub detail: String,
}

/// 把 MCP elicitation 参数交给统一交互入口。
pub(crate) async fn ask_elicitation(
    broker: &dyn UserInteractionBroker,
    owner: TrustedInteractionOwner,
    server: &str,
    tool: &str,
    params: &ElicitRequestParams,
    cancellation: &CancellationToken,
) -> Result<MappedElicitation, ElicitationChannelFailure> {
    let request = match params {
        ElicitRequestParams::FormElicitationParams {
            message,
            requested_schema,
            ..
        } => ElicitationRequest::Form {
            server: server.to_string(),
            tool: tool.to_string(),
            message: message.clone(),
            requested_schema: serde_json::to_value(requested_schema).map_err(|error| {
                ElicitationChannelFailure {
                    detail: format!("failed to serialize MCP requestedSchema: {error}"),
                }
            })?,
        },
        ElicitRequestParams::UrlElicitationParams {
            message,
            url,
            elicitation_id,
            ..
        } => ElicitationRequest::Url {
            server: server.to_string(),
            tool: tool.to_string(),
            message: message.clone(),
            url: url.clone(),
            elicitation_id: elicitation_id.clone(),
        },
        _ => {
            return Err(ElicitationChannelFailure {
                detail: "unsupported MCP elicitation mode".to_string(),
            });
        }
    };

    let response = tokio::select! {
        response = broker.request(
            owner,
            InteractionContext::Elicitation { request },
        ) => response,
        _ = cancellation.cancelled() => {
            return Ok(MappedElicitation {
                decision: ElicitationDecision::Cancel,
                unavailable: None,
            });
        }
    };

    Ok(match response {
        InteractionResponse::Elicitation(decision) => MappedElicitation {
            decision,
            unavailable: None,
        },
        InteractionResponse::Rejected => MappedElicitation {
            decision: ElicitationDecision::Decline,
            unavailable: None,
        },
        InteractionResponse::Unanswered { cause } => MappedElicitation {
            decision: ElicitationDecision::Decline,
            unavailable: Some(cause),
        },
        // 超时是「这次提问没有被回答」的用户侧中止，不是协议失败：按 cancel 回包。
        InteractionResponse::Failed {
            failure: InteractionFailure::Timeout,
        } => MappedElicitation {
            decision: ElicitationDecision::Cancel,
            unavailable: None,
        },
        InteractionResponse::Failed { failure } => {
            return Err(ElicitationChannelFailure {
                detail: failure.reason_text(),
            });
        }
        InteractionResponse::Answers(_) | InteractionResponse::Decisions(_) => {
            return Err(ElicitationChannelFailure {
                detail: "interaction broker returned a non-elicitation response".to_string(),
            });
        }
    })
}

impl MappedElicitation {
    pub(crate) fn into_result(self) -> ElicitResult {
        let result = match self.decision {
            ElicitationDecision::Accept { content } => {
                ElicitResult::new(ElicitationAction::Accept).with_content(content)
            }
            ElicitationDecision::Decline => ElicitResult::new(ElicitationAction::Decline),
            ElicitationDecision::Cancel => ElicitResult::new(ElicitationAction::Cancel),
        };
        match self.unavailable {
            Some(cause) => result.with_meta(unavailable_meta(cause)),
            None => result,
        }
    }
}

/// 无交互入口时回 `decline` + `_meta` 原因（不得伪装成用户拒绝）。
pub(crate) fn unavailable_decline(cause: UnansweredCause) -> ElicitResult {
    ElicitResult::new(ElicitationAction::Decline).with_meta(unavailable_meta(cause))
}

fn unavailable_meta(cause: UnansweredCause) -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0.insert(
        ELICITATION_UNAVAILABLE_META_KEY.to_string(),
        serde_json::Value::String(
            serde_json::to_value(cause)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string()),
        ),
    );
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct StubBroker {
        response: Option<InteractionResponse>,
        seen: parking_lot::Mutex<Vec<InteractionContext>>,
    }

    impl StubBroker {
        fn new(response: InteractionResponse) -> Arc<Self> {
            Arc::new(Self {
                response: Some(response),
                seen: parking_lot::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl UserInteractionBroker for StubBroker {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            ctx: InteractionContext,
        ) -> InteractionResponse {
            self.seen.lock().push(ctx);
            self.response.clone().expect("single use")
        }
    }

    fn owner() -> TrustedInteractionOwner {
        TrustedInteractionOwner::host("peri/test", 1)
    }

    fn form_params() -> ElicitRequestParams {
        ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "approve?".into(),
            requested_schema: rmcp::model::ElicitationSchema::builder()
                .required_bool("approve")
                .build()
                .expect("schema"),
        }
    }

    #[tokio::test]
    async fn single_active_call_is_routed_and_removed_on_drop() {
        let router = ElicitationRouter::new("srv", 1);
        let lease = router.register(
            "session-1",
            "tool-1",
            owner(),
            StubBroker::new(InteractionResponse::Elicitation(
                ElicitationDecision::Decline,
            )),
            CancellationToken::new(),
        );
        assert!(matches!(router.resolve(), RouteResolution::Unique(_)));
        let mapped = lease.ask(&form_params()).await.expect("mapped");
        assert_eq!(mapped.decision, ElicitationDecision::Decline);
        drop(lease);
        assert!(matches!(router.resolve(), RouteResolution::NoActiveCall));
        assert_eq!(router.active_count(), 0);
    }

    #[tokio::test]
    async fn concurrent_active_calls_fail_closed() {
        let router = ElicitationRouter::new("srv", 1);
        let first = router.register(
            "session-1",
            "tool-1",
            owner(),
            StubBroker::new(InteractionResponse::Elicitation(
                ElicitationDecision::Decline,
            )),
            CancellationToken::new(),
        );
        let _second = router.register(
            "session-2",
            "tool-2",
            owner(),
            StubBroker::new(InteractionResponse::Elicitation(
                ElicitationDecision::Decline,
            )),
            CancellationToken::new(),
        );
        assert!(matches!(
            router.resolve(),
            RouteResolution::Ambiguous { active: 2 }
        ));
        drop(first);
    }

    #[tokio::test]
    async fn cancellation_wins_over_pending_broker() {
        struct HangingBroker;
        #[async_trait]
        impl UserInteractionBroker for HangingBroker {
            async fn request(
                &self,
                _owner: TrustedInteractionOwner,
                _ctx: InteractionContext,
            ) -> InteractionResponse {
                std::future::pending().await
            }
        }

        let router = ElicitationRouter::new("srv", 1);
        let cancel = CancellationToken::new();
        let lease = router.register(
            "session-1",
            "tool-1",
            owner(),
            Arc::new(HangingBroker),
            cancel.clone(),
        );
        let task = {
            let router = Arc::clone(&router);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let lease = router.register(
                    "session-1",
                    "tool-1",
                    owner(),
                    Arc::new(HangingBroker),
                    cancel,
                );
                lease.ask(&form_params()).await
            })
        };
        drop(lease);
        cancel.cancel();
        let mapped = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("cancellation wakes the ask")
            .expect("join")
            .expect("mapped");
        assert_eq!(mapped.decision, ElicitationDecision::Cancel);
    }

    #[tokio::test]
    async fn form_and_url_map_without_losing_identity() {
        let broker = StubBroker::new(InteractionResponse::Elicitation(
            ElicitationDecision::Accept {
                content: serde_json::json!({ "approve": true }),
            },
        ));
        let mapped = ask_elicitation(
            &*broker,
            owner(),
            "srv",
            "tool",
            &form_params(),
            &CancellationToken::new(),
        )
        .await
        .expect("mapped");
        assert_eq!(
            mapped.decision,
            ElicitationDecision::Accept {
                content: serde_json::json!({ "approve": true })
            }
        );
        let seen = broker.seen.lock();
        let InteractionContext::Elicitation {
            request:
                ElicitationRequest::Form {
                    server,
                    tool,
                    message,
                    requested_schema,
                },
        } = &seen[0]
        else {
            panic!("expected form elicitation");
        };
        assert_eq!(server, "srv");
        assert_eq!(tool, "tool");
        assert_eq!(message, "approve?");
        assert_eq!(requested_schema["properties"]["approve"]["type"], "boolean");
    }

    #[tokio::test]
    async fn timeout_maps_to_cancel_and_unanswered_maps_to_decline_with_meta() {
        let timeout = ask_elicitation(
            &*StubBroker::new(InteractionResponse::Failed {
                failure: InteractionFailure::Timeout,
            }),
            owner(),
            "srv",
            "tool",
            &form_params(),
            &CancellationToken::new(),
        )
        .await
        .expect("timeout is not a channel failure");
        assert_eq!(timeout.decision, ElicitationDecision::Cancel);

        let unanswered = ask_elicitation(
            &*StubBroker::new(InteractionResponse::Unanswered {
                cause: UnansweredCause::NonInteractiveClient,
            }),
            owner(),
            "srv",
            "tool",
            &form_params(),
            &CancellationToken::new(),
        )
        .await
        .expect("unanswered is a decline");
        assert_eq!(unanswered.decision, ElicitationDecision::Decline);
        let result = unanswered.into_result();
        assert_eq!(result.action, ElicitationAction::Decline);
        assert_eq!(
            result.meta.expect("meta").0[ELICITATION_UNAVAILABLE_META_KEY],
            serde_json::json!("non_interactive_client")
        );
    }

    #[test]
    fn url_completion_only_for_known_route() {
        let router = ElicitationRouter::new("srv", 1);
        router.note_url_route("e-1", "session-1");
        assert_eq!(router.complete_url("e-1").as_deref(), Some("session-1"));
        assert_eq!(router.complete_url("unknown"), None);
        assert_eq!(router.completed_url_count(), 1);
    }
}
