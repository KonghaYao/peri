use super::*;

#[tokio::test]
async fn test_invalid_batcher_config_disables_session_without_worker_panic() {
    for (max_events, interval) in [(0, 10), (50, 0), (usize::MAX, 10)] {
        let config = LangfuseConfig {
            public_key: Some("test-public".into()),
            secret_key: Some("test-secret".into()),
            host: "http://127.0.0.1:1".into(),
            batch_max_events: max_events,
            batch_flush_interval_secs: interval,
            ..Default::default()
        };
        assert!(
            LangfuseSession::new(config, "test-session".into())
                .await
                .is_none(),
            "非法批处理配置必须沿现有 Option 失败路径返回"
        );
    }
}
