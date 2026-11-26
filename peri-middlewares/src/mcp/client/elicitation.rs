//! 外部 MCP Server 的 elicitation 参数映射与交互归属（单一权威）。
//!
//! 2026-07-28 起 elicitation 只随原 `tools/call` 的 `inputRequests` 返回，
//! 不再有独立的 server → client 反向 RPC。归属由发起工具调用的一方在构造
//! [`InteractionRoute`] 时显式给定，不做按连接猜测。

use std::sync::Arc;

use peri_acp_types::interaction::{
    ElicitationDecision, ElicitationRequest, InteractionContext, InteractionFailure,
    InteractionResponse, TrustedInteractionOwner, UnansweredCause, UserInteractionBroker,
};
use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction, MetaObject};
use tokio_util::sync::CancellationToken;

/// 无可用交互入口时写入 `ElicitResult._meta` 的键。
pub(crate) const ELICITATION_UNAVAILABLE_META_KEY: &str = "io.peri/elicitationUnavailable";

/// 一次逻辑工具调用的交互归属。
///
/// owner / broker / server / tool / 取消令牌都由实际执行 `tools/call` 的一方
/// 传入；`ask` 不会读取其他调用或连接的状态。
pub(crate) struct InteractionRoute {
    pub(crate) server: String,
    pub(crate) tool: String,
    pub(crate) owner: TrustedInteractionOwner,
    pub(crate) broker: Arc<dyn UserInteractionBroker>,
    pub(crate) cancellation: CancellationToken,
}

impl InteractionRoute {
    pub(crate) async fn ask(
        &self,
        params: &ElicitRequestParams,
    ) -> Result<MappedElicitation, ElicitationChannelFailure> {
        ask_elicitation(
            &*self.broker,
            self.owner.clone(),
            &self.server,
            &self.tool,
            params,
            &self.cancellation,
        )
        .await
    }
}

/// MCP 参数映射后的答复；`unavailable` 表示没有可作答方（带真实原因）。
#[derive(Debug, Clone)]
pub(crate) struct MappedElicitation {
    pub decision: ElicitationDecision,
    pub unavailable: Option<UnansweredCause>,
    /// URL 模式的 accept 只表示同意导航，MCP 结果不得携带 content。
    url_mode: bool,
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
    let url_mode = matches!(params, ElicitRequestParams::UrlElicitationParams { .. });
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
        ElicitRequestParams::UrlElicitationParams { message, url, .. } => ElicitationRequest::Url {
            server: server.to_string(),
            tool: tool.to_string(),
            message: message.clone(),
            url: url.clone(),
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
                url_mode: false,
            });
        }
    };

    Ok(match response {
        InteractionResponse::Elicitation(decision) => MappedElicitation {
            decision,
            unavailable: None,
            url_mode,
        },
        InteractionResponse::Rejected => MappedElicitation {
            decision: ElicitationDecision::Decline,
            unavailable: None,
            url_mode,
        },
        InteractionResponse::Unanswered { cause } => MappedElicitation {
            decision: ElicitationDecision::Decline,
            unavailable: Some(cause),
            url_mode,
        },
        // 超时是「这次提问没有被回答」的用户侧中止，不是协议失败：按 cancel 回包。
        InteractionResponse::Failed {
            failure: InteractionFailure::Timeout,
        } => MappedElicitation {
            decision: ElicitationDecision::Cancel,
            unavailable: None,
            url_mode,
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
            ElicitationDecision::Accept { content } if !self.url_mode => {
                ElicitResult::new(ElicitationAction::Accept).with_content(content)
            }
            ElicitationDecision::Accept { .. } => ElicitResult::new(ElicitationAction::Accept),
            ElicitationDecision::Decline => ElicitResult::new(ElicitationAction::Decline),
            ElicitationDecision::Cancel => ElicitResult::new(ElicitationAction::Cancel),
        };
        match self.unavailable {
            Some(cause) => result.with_meta(unavailable_meta(cause)),
            None => result,
        }
    }
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
    use peri_acp_types::interaction::InteractionContext;
    use std::sync::Arc;

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

    fn route(broker: Arc<StubBroker>, cancellation: CancellationToken) -> InteractionRoute {
        InteractionRoute {
            server: "srv".to_string(),
            tool: "tool".to_string(),
            owner: owner(),
            broker: broker as Arc<dyn UserInteractionBroker>,
            cancellation,
        }
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

    fn url_params() -> ElicitRequestParams {
        ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "authorize".into(),
            url: "https://example.com/connect".into(),
            elicitation_id: String::new(),
        }
    }

    #[tokio::test]
    async fn form_maps_to_the_shared_interaction_entry() {
        let broker = StubBroker::new(InteractionResponse::Elicitation(
            ElicitationDecision::Accept {
                content: serde_json::json!({ "approve": true }),
            },
        ));
        let mapped = route(Arc::clone(&broker), CancellationToken::new())
            .ask(&form_params())
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
    async fn url_maps_without_an_mcp_elicitation_id() {
        let broker = StubBroker::new(InteractionResponse::Elicitation(
            ElicitationDecision::Decline,
        ));
        let mapped = route(Arc::clone(&broker), CancellationToken::new())
            .ask(&url_params())
            .await
            .expect("mapped");
        assert_eq!(mapped.decision, ElicitationDecision::Decline);
        let seen = broker.seen.lock();
        let InteractionContext::Elicitation {
            request:
                ElicitationRequest::Url {
                    server,
                    tool,
                    message,
                    url,
                },
        } = &seen[0]
        else {
            panic!("expected url elicitation");
        };
        assert_eq!(server, "srv");
        assert_eq!(tool, "tool");
        assert_eq!(message, "authorize");
        assert_eq!(url, "https://example.com/connect");
    }

    #[tokio::test]
    async fn timeout_maps_to_cancel_and_unanswered_maps_to_decline_with_meta() {
        let timeout = route(
            StubBroker::new(InteractionResponse::Failed {
                failure: InteractionFailure::Timeout,
            }),
            CancellationToken::new(),
        )
        .ask(&form_params())
        .await
        .expect("timeout is not a channel failure");
        assert_eq!(timeout.decision, ElicitationDecision::Cancel);

        let unanswered = route(
            StubBroker::new(InteractionResponse::Unanswered {
                cause: UnansweredCause::NonInteractiveClient,
            }),
            CancellationToken::new(),
        )
        .ask(&form_params())
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

    #[tokio::test]
    async fn url_accept_omits_content_in_mcp_result() {
        let broker = StubBroker::new(InteractionResponse::Elicitation(
            ElicitationDecision::Accept {
                content: serde_json::json!({ "unexpected": true }),
            },
        ));
        let mapped = route(Arc::clone(&broker), CancellationToken::new())
            .ask(&url_params())
            .await
            .expect("mapped");
        let result = mapped.into_result();
        assert_eq!(result.action, ElicitationAction::Accept);
        assert!(
            result.content.is_none(),
            "URL accept only means consent to navigate; content must be omitted"
        );
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

        let cancel = CancellationToken::new();
        let route = InteractionRoute {
            server: "srv".to_string(),
            tool: "tool".to_string(),
            owner: owner(),
            broker: Arc::new(HangingBroker),
            cancellation: cancel.clone(),
        };
        let task = tokio::spawn(async move { route.ask(&form_params()).await });
        cancel.cancel();
        let mapped = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("cancellation wakes the ask")
            .expect("join")
            .expect("mapped");
        assert_eq!(mapped.decision, ElicitationDecision::Cancel);
    }
}
