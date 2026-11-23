//! URL elicitation 端口 → 本会话交互 router 的映射：决策如实回传，
//! 未投递（无 endpoint / 多 endpoint）按 decline 结算，绝不广播。
//!
//! transport 是 stub（只记录 ACP 帧并按脚本作答）；router、门与 broker 是真实对象。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::ports::{UrlElicitationOutcome, UrlElicitationPort, UrlElicitationRequest};
use serde_json::Value;

use super::SessionUrlElicitation;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

/// 只记录 ACP 帧、对 `elicitation/create` 回脚本动作的 stub transport。
#[derive(Default)]
struct ScriptedTransport {
    frames: Mutex<Vec<(String, Value)>>,
    /// `elicitation/create` 的响应体（默认 `cancel`）。
    reply: Mutex<Value>,
}

impl ScriptedTransport {
    fn with_reply(action: &str) -> Arc<Self> {
        let transport = Arc::new(Self::default());
        *transport.reply.lock().unwrap() = serde_json::json!({ "action": action });
        transport
    }

    fn frames(&self) -> Vec<(String, Value)> {
        self.frames.lock().unwrap().clone()
    }
}

#[async_trait]
impl AcpTransport for ScriptedTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.frames
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(self.reply.lock().unwrap().clone())
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

fn router() -> Arc<crate::host::interaction_router::SessionInteractionRouter> {
    Arc::new(
        crate::host::interaction_router::SessionInteractionRouter::new(
            "session-1",
            Arc::new(tokio::sync::Mutex::new(())),
        ),
    )
}

fn request() -> UrlElicitationRequest {
    UrlElicitationRequest {
        server_name: "peri-browser".into(),
        message: "Authorize the browser session".into(),
        url: "https://example.test/authorize".into(),
        elicitation_id: "e-1".into(),
    }
}

#[tokio::test]
async fn accept_from_the_frontend_maps_to_accepted() {
    let router = router();
    let transport = ScriptedTransport::with_reply("accept");
    router
        .register_connection("conn-1", Arc::clone(&transport) as Arc<dyn AcpTransport>)
        .expect("注册唯一在线 endpoint");
    let port = SessionUrlElicitation::new(router);

    assert_eq!(
        port.elicit_url(request()).await,
        UrlElicitationOutcome::Accepted
    );

    let frames = transport.frames();
    assert_eq!(frames.len(), 1, "一次 URL elicitation 只发一帧");
    let (method, params) = &frames[0];
    assert_eq!(method, "elicitation/create");
    assert_eq!(params["mode"], "url");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["url"], "https://example.test/authorize");
    assert_eq!(params["elicitationId"], "e-1");
    assert_eq!(params["message"], "Authorize the browser session");
    assert_eq!(
        params["_meta"][peri_acp_types::interaction::MCP_ELICITATION_SERVER_META_KEY],
        "peri-browser"
    );
}

#[tokio::test]
async fn decline_and_cancel_keep_the_users_choice() {
    for (action, expected) in [
        ("decline", UrlElicitationOutcome::Declined),
        ("cancel", UrlElicitationOutcome::Cancelled),
    ] {
        let router = router();
        let transport = ScriptedTransport::with_reply(action);
        router
            .register_connection("conn-1", Arc::clone(&transport) as Arc<dyn AcpTransport>)
            .expect("注册唯一在线 endpoint");
        let port = SessionUrlElicitation::new(router);

        assert_eq!(
            port.elicit_url(request()).await,
            expected,
            "action = {action}"
        );
    }
}

#[tokio::test]
async fn no_online_endpoint_declines_without_delivery() {
    let port = SessionUrlElicitation::new(router());

    assert_eq!(
        port.elicit_url(request()).await,
        UrlElicitationOutcome::Declined,
        "无前台可投递时按 decline 结算，不阻塞调用方"
    );
}

#[tokio::test]
async fn multiple_online_endpoints_decline_without_broadcast() {
    let router = router();
    let first = ScriptedTransport::with_reply("accept");
    let second = ScriptedTransport::with_reply("accept");
    router
        .register_connection("conn-1", Arc::clone(&first) as Arc<dyn AcpTransport>)
        .expect("注册首个连接");
    router
        .register_connection("conn-2", Arc::clone(&second) as Arc<dyn AcpTransport>)
        .expect("注册第二个连接");
    let port = SessionUrlElicitation::new(router);

    assert_eq!(
        port.elicit_url(request()).await,
        UrlElicitationOutcome::Declined,
        "归属歧义时不得猜连接"
    );
    assert!(first.frames().is_empty(), "不得广播到任意连接");
    assert!(second.frames().is_empty(), "不得广播到任意连接");
}
