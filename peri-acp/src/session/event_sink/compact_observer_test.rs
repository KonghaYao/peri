//! Full Compact 提交观察者：只有 Full 触发宿主回调，展示事件不被吞掉。

use super::*;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use serde_json::Value;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct RecordingTransport {
    notifications: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl AcpTransport for RecordingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(Value::Null)
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
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

#[tokio::test]
async fn full_compact_completion_notifies_commit_observer_but_other_strategies_do_not() {
    #[derive(Default)]
    struct CountingObserver {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CompactCommitObserver for CountingObserver {
        async fn on_compact_committed(&self, session_id: &str) {
            self.calls.lock().unwrap().push(session_id.to_owned());
        }
    }

    fn completed(strategy: peri_acp_types::event::CompactStrategy) -> ExecutorEvent {
        ExecutorEvent::CompactCompleted {
            summary: String::new(),
            messages: vec![],
            trigger: peri_acp_types::event::CompactTrigger::Auto,
            strategy,
            affected_count: 1,
            estimated_tokens_saved: 0,
            files: vec![],
            skills: vec![],
        }
    }

    let transport = Arc::new(RecordingTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let observer = Arc::new(CountingObserver::default());
    let sink = TransportEventSink::new(transport.clone(), caps)
        .with_compact_commit_observer(observer.clone());

    for strategy in [
        peri_acp_types::event::CompactStrategy::Full,
        peri_acp_types::event::CompactStrategy::Micro,
        peri_acp_types::event::CompactStrategy::Smart,
    ] {
        sink.push_event("s1", &completed(strategy), 0)
            .await
            .unwrap();
    }

    assert_eq!(
        observer.calls.lock().unwrap().as_slice(),
        ["s1".to_string()],
        "只有 Full 提交产生检查点；Micro/Smart 不得伪造 Full 窗口事实"
    );
    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(
        notifications.len(),
        3,
        "提交观察者不吞掉原有 CompactCompleted 展示事件"
    );
}
