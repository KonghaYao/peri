use super::*;

#[tokio::test(flavor = "current_thread")]
async fn user_input_subscription_lag_republishes_current_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, _, sid) = make_user_input_session(&tmp).await;
    let transport = Arc::new(MockTransport::default());
    let wire: Arc<dyn crate::transport::AcpTransport> = transport.clone();
    let mailbox = crate::host::user_input::ensure_mailbox(&sid, &cfg, &wire).unwrap();
    // 当前线程不 yield：ensure_mailbox 已 subscribe，但 delivery task 尚未运行。
    // 全部填充为其他会话事件；正常分支不会发出任何本会话快照。
    let mut witness = cfg.controller.subscribe();
    for _ in 0..2048 {
        cfg.controller
            .publish(peri_acp_types::identity::EventEnvelope::new(
                "other-session",
                peri_acp_types::identity::SessionEpoch::initial(),
                "turn",
                "agent",
                peri_acp_types::identity::SessionSeq::initial(),
                peri_acp_types::identity::EventDeliveryClass::Critical,
            ));
    }
    assert!(matches!(
        witness.try_recv(),
        Err(peri_controller::SubscriptionError::Lagged(_))
    ));
    peri_time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshots: Vec<_> = transport
                .notifications()
                .into_iter()
                .filter_map(|(method, payload)| {
                    if method != "peri/agent_event" {
                        return None;
                    }
                    let event: crate::event::AcpEvent =
                        serde_json::from_str(payload["event_json"].as_str()?).ok()?;
                    match event {
                        crate::event::AcpEvent::UserInputQueueChanged { snapshot } => {
                            Some(snapshot)
                        }
                        _ => None,
                    }
                })
                .collect();
            if let Some(snapshot) = snapshots.first() {
                assert_eq!(snapshot.session_id, sid);
                assert_eq!(snapshot.generation, mailbox.generation());
                assert_eq!(snapshot.revision, mailbox.snapshot().revision);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Lagged 必须主动补快照，无需等待下一次输入变更");
}
