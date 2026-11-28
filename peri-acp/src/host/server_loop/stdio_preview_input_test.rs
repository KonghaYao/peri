use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    close_error, confirmed_withdrawals, dispatch_error, resolve_dispatch_failure,
    settle_failed_close, DispatchFailureReply,
};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use peri_agent::session::user_input_mailbox::UserInputQueueError;

#[test]
fn unconfirmed_dispatch_schedules_delivery_before_acknowledging() {
    let mut scheduled = 0;
    let reply = resolve_dispatch_failure(Ok(false), || scheduled += 1);
    assert_eq!(
        reply,
        DispatchFailureReply::Scheduled,
        "撤回未确认时输入可能仍在 Dispatching，必须按成功回复"
    );
    assert_eq!(
        scheduled, 1,
        "撤回未确认时必须调度投递，否则空闲会话的 prompt 永久挂起"
    );

    let mut scheduled = 0;
    let reply =
        resolve_dispatch_failure(Err(AcpError::new(-32603, "store down")), || scheduled += 1);
    assert_eq!(
        reply,
        DispatchFailureReply::Scheduled,
        "撤回失败同样不能排除输入已插入"
    );
    assert_eq!(
        scheduled, 1,
        "撤回失败时也必须调度投递，不能只记日志后返回成功"
    );

    let mut scheduled = 0;
    let reply = resolve_dispatch_failure(Ok(true), || scheduled += 1);
    assert_eq!(
        reply,
        DispatchFailureReply::Failed,
        "确认撤回说明未插入，必须返回错误"
    );
    assert_eq!(scheduled, 0, "已确认撤回的输入不会被投递");
}

#[test]
fn confirmed_withdrawal_reports_store_failure_as_retryable_internal_error() {
    let error = dispatch_error(UserInputQueueError::Store("mq down".into()));
    assert_eq!(error.code, -32603);
    assert!(error.message.contains("mq down"));
    let invalid = dispatch_error(UserInputQueueError::InvalidIdentity);
    assert_eq!(invalid.code, -32602);
}

#[test]
fn failed_close_payload_yields_every_confirmed_withdrawal() {
    let error = close_error(
        AcpError::new(-32603, "store down"),
        &["input-a".to_owned(), "input-b".to_owned()],
    );
    assert_eq!(
        confirmed_withdrawals(
            error
                .data
                .as_ref()
                .expect("close failure carries its withdrawals")
        ),
        vec!["input-a".to_owned(), "input-b".to_owned()],
        "对象形态的 withdrawn 必须完整提取，否则已确认撤回永远不会结算"
    );
    assert_eq!(
        confirmed_withdrawals(&json!(["input-c"])),
        vec!["input-c".to_owned()],
        "成功回复仍是裸数组"
    );
    assert!(
        confirmed_withdrawals(&json!({"reason": "session_cleanup_incomplete"})).is_empty(),
        "不含撤回列表的错误负载不产生撤回项"
    );
}

#[derive(Default)]
struct RecordingTransport {
    settled: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}

impl RecordingTransport {
    fn settled(&self) -> Vec<(String, Vec<String>)> {
        self.settled.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
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
    async fn settle_withdrawn(&self, session_id: &str, input_ids: &[String]) {
        self.settled
            .lock()
            .unwrap()
            .push((session_id.to_owned(), input_ids.to_vec()));
    }
}

#[tokio::test]
async fn failed_close_settles_every_confirmed_withdrawal() {
    let transport = RecordingTransport::default();
    let error = close_error(
        AcpError::new(-32603, "store down"),
        &["input-a".to_owned(), "input-b".to_owned()],
    );
    settle_failed_close(&transport, "session-1", &error).await;
    assert_eq!(
        transport.settled(),
        vec![(
            "session-1".to_owned(),
            vec!["input-a".to_owned(), "input-b".to_owned()]
        )],
        "close 失败必须把已确认撤回的 input 交给 adapter 结算"
    );
}

#[tokio::test]
async fn close_failure_without_confirmed_withdrawals_settles_no_input() {
    let transport = RecordingTransport::default();
    settle_failed_close(
        &transport,
        "session-1",
        &AcpError::new(-32002, "Session not active"),
    )
    .await;
    assert!(
        transport.settled().iter().all(|(_, ids)| ids.is_empty()),
        "关闭前就失败时没有可结算的撤回"
    );
}
