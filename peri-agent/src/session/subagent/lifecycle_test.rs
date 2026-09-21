use super::*;

/// [回归测试] 取消正在 drain 的 owner 时不能 detach forwarder，随后发布成功 Stop。
#[tokio::test(flavor = "current_thread")]
async fn test_drain_subagent_events_abort_cancels_forwarder() {
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let (bus, _handles) = EventBus::new(Default::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let forwarder = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let _dropped = Dropped(Some(dropped_tx));
        entered_tx.send(()).unwrap();
        std::future::pending::<Option<crate::agent::events_v2::ObserveEvent>>().await
    }));
    let owner = tokio::spawn(drain_subagent_events(Arc::new(bus), forwarder, None));
    entered_rx.await.unwrap();
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), dropped_rx)
        .await
        .expect("owner 被 abort 后 forwarder 必须销毁，不能继续 detached 执行")
        .unwrap();
}

/// [回归测试] handler 同步阻塞期间强制 abort，恢复后不能由 forwarder 补发成功 Stop。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_drain_subagent_events_abort_during_handler_does_not_publish_stop() {
    use crate::agent::events_v2::{ObserveEvent, RenderEvent};
    struct GatedBridge {
        entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
        stops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl crate::agent::LangfuseBridgeLike for GatedBridge {
        fn process_render_event(&self, _event: &RenderEvent) {
            self.entered.lock().take().unwrap().send(()).unwrap();
            self.release.lock().recv().unwrap();
        }
        fn process_observe_event(&self, event: &ObserveEvent) {
            if matches!(event, ObserveEvent::SubagentStop { .. }) {
                self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    impl Drop for GatedBridge {
        fn drop(&mut self) {
            let _ = self.dropped.take().unwrap().send(());
        }
    }
    let (bus, handles) = EventBus::new(Default::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bridge = Arc::new(GatedBridge {
        entered: parking_lot::Mutex::new(Some(entered_tx)),
        release: parking_lot::Mutex::new(release_rx),
        dropped: Some(dropped_tx),
        stops: stops.clone(),
    });
    let turn_id = TurnId::new();
    let agent_id = AgentId::new();
    bus.emit_render(RenderEvent::TextChunk {
        turn_id,
        agent_id,
        message_id: peri_acp_types::messages::MessageId::new(),
        chunk: "tail".into(),
    });
    bus.emit_observe(build_subagent_stop_v2(
        turn_id,
        Some(agent_id),
        agent_id,
        "fixture",
        "done",
        false,
    ));
    let forwarder =
        crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion(
            handles,
            None,
            Some(bridge.clone()),
            "fixture".into(),
        );
    let owner = tokio::spawn(drain_subagent_events(
        Arc::new(bus),
        forwarder,
        Some(bridge.clone()),
    ));
    entered_rx.await.unwrap();
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    release_tx.send(()).unwrap();
    drop(bridge);
    tokio::time::timeout(std::time::Duration::from_secs(5), dropped_rx)
        .await
        .expect("forwarder 退出后必须释放最后的 bridge owner")
        .unwrap();
    assert_eq!(
        stops.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "强制取消后遥测终态保持 incomplete"
    );
}
