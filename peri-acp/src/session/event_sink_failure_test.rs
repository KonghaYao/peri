//! 真实 TransportEventSink 的发送失败注入回归（与主 sink 测试分离，保持文件规模）。

use super::*;

/// 真实 TransportEventSink 的发送失败必须上报，而不是被 legacy/reminder helper 吞掉。
struct FailingTransport;

#[async_trait]
impl AcpTransport for FailingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Err(AcpError::new(-32603, "injected request failure"))
    }
    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Err(AcpError::new(-32603, "injected notification failure"))
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

#[tokio::test]
async fn real_transport_send_failures_propagate_for_delivery_and_reminder() {
    use peri_acp_types::system_reminder::{ReminderAudience, ReminderDelivery};

    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            user_input_queue: true,
            system_reminder: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(Arc::new(FailingTransport), caps);

    let delivered = ExecutorEvent::UserInputDelivered {
        generation: "g".into(),
        input_id: "input-1".into(),
        content: MessageContent::text("正文"),
    };
    assert!(
        sink.push_event("s1", &delivered, 0).await.is_err(),
        "legacy delivery send failure must propagate"
    );

    let reminder = reminder_for_test(
        "send_failure",
        ReminderDelivery::Configurable,
        vec![ReminderAudience::Tui],
    );
    assert!(
        sink.push_event(
            "s1",
            &ExecutorEvent::SystemReminder {
                reminder,
                source_agent_id: None,
            },
            0,
        )
        .await
        .is_err(),
        "system reminder send failure must propagate"
    );
}
