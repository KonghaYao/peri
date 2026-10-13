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
            LangfuseSession::new(config, "test-session".into(), None)
                .await
                .is_none(),
            "非法批处理配置必须沿现有 Option 失败路径返回"
        );
    }
}

/// 遥测侧的运行期配置错误同样必须被拒绝，不能被默认值掩盖成一次"成功"部署。
#[tokio::test]
async fn test_invalid_telemetry_config_disables_session() {
    let config = LangfuseConfig {
        public_key: Some("test-public".into()),
        secret_key: Some("test-secret".into()),
        host: "http://127.0.0.1:1".into(),
        memory_budget_bytes: 0,
        ..Default::default()
    };
    assert!(
        LangfuseSession::new(config, "test-session".into(), None)
            .await
            .is_none(),
        "零内存预算属于运行期配置错误，必须返回 None 而不是降级运行"
    );
}

/// 委托链自证：`try_freeze`/`memory_budget`/`try_add_prepared`/`flush`/`shutdown` 必须全部落在
/// 同一份部署 Batcher 上——`TelemetryEvent::try_new` 逐项 `Arc::ptr_eq` 核对预算身份，金额相同的
/// 另一份预算会被拒；关闭报告必须与部署终态逐一对应，不得由 session 另造。
#[tokio::test]
async fn test_session_delegates_freeze_prepared_and_budget_to_one_deployment() {
    let config = LangfuseConfig {
        public_key: Some("test-public".into()),
        secret_key: Some("test-secret".into()),
        host: "http://127.0.0.1:1".into(),
        compression: LangfuseCompression::None,
        ..Default::default()
    };
    let session = LangfuseSession::new(config, "test-session".into(), None)
        .await
        .expect("合法配置必须构造出可用 session");
    let session_dyn: &dyn LangfuseSessionLike = &session;

    // 预算身份：session 透传部署自己那一份，不允许替换或另造等额预算（契约 §3.3）。
    assert!(
        Arc::ptr_eq(session_dyn.memory_budget(), session.batcher.memory_budget()),
        "SessionLike 必须透传部署预算身份"
    );

    // 冻结准入由部署执行：小输入必须拿到可交付来源，而不是 controller 侧伪造。
    let source: Arc<dyn SnapshotSource> = Arc::new(ConstantSource("{\"messages\":[]}"));
    let frozen = session_dyn
        .try_freeze(Arc::clone(&source))
        .expect("小输入必须被部署接受为内存快照");
    let event = IngestionEvent::TraceCreate {
        id: "trace-wire".into(),
        timestamp: peri_time::now_utc_rfc3339(),
        body: langfuse_client::types::TraceBody {
            id: Some("trace-wire".into()),
            ..Default::default()
        },
        metadata: None,
    };
    let prepared = TelemetryEvent::try_new(event, Some(frozen), None, session_dyn.memory_budget())
        .expect("冻结来源与事件必须属于同一部署预算");
    session_dyn
        .try_add_prepared(prepared)
        .expect("已冻结事件必须进入同一 Batcher");

    // 关闭后 flush 返回同一部署终态（契约 §3.6）：session 不得吞掉失败或另造成功。
    let deployment = session.batcher.shutdown().await;
    let flush = session_dyn.flush().await;
    assert_eq!(
        flush.is_ok(),
        deployment.is_ok(),
        "关闭后 flush 必须与部署终态同类: {flush:?} vs {deployment:?}"
    );
    let report = session.shutdown().await;
    assert_eq!(
        report,
        map_shutdown(deployment),
        "关闭报告必须与部署终态逐一对应，不得落 UnexpectedFailure"
    );
}

/// §3.6 全变体映射：待交付、持久待交付、交付不完整与无事件准备失败都必须照实上报计数，
/// 既不能被折叠成 `UnexpectedFailure`，也不能被当成成功。
#[test]
fn test_shutdown_mapping_covers_every_deployment_terminal_state() {
    let counts = DeliveryCounts {
        pending_events: 3,
        persisted_pending_events: 1,
        failed_events: 2,
        cleanup_pending_objects: 1,
        active_tasks: 1,
    };
    assert_eq!(
        map_shutdown(Ok(ShutdownReport::Complete)),
        LangfuseShutdownReport::Complete
    );
    assert_eq!(
        map_shutdown(Err(LangfuseError::Pending { counts })),
        LangfuseShutdownReport::Pending { counts }
    );
    assert_eq!(
        map_shutdown(Err(LangfuseError::PersistedPending { counts })),
        LangfuseShutdownReport::PersistedPending { counts }
    );
    assert_eq!(
        map_shutdown(Err(LangfuseError::DeliveryIncomplete { counts })),
        LangfuseShutdownReport::DeliveryIncomplete { counts }
    );
    // 准备失败没有事件级身份：按计数旁路报告，不伪造 failed_events。
    assert_eq!(
        map_shutdown(Err(LangfuseError::SnapshotPreparationFailed {
            failed_snapshots: 2,
            counts,
        })),
        LangfuseShutdownReport::DeliveryIncomplete { counts }
    );
    assert_eq!(
        map_shutdown(Err(LangfuseError::IngestionApi("delivery failed".into()))),
        LangfuseShutdownReport::DeliveryFailed {
            summary: "delivery failed".into(),
        }
    );
    assert_eq!(
        map_shutdown(Err(LangfuseError::WorkerJoinFailed { cancelled: true })),
        LangfuseShutdownReport::WorkerFailed { cancelled: true }
    );
    // 其余终态不得冒充完成或成功。
    assert_eq!(
        map_shutdown(Err(LangfuseError::BudgetMismatch)),
        LangfuseShutdownReport::UnexpectedFailure
    );
}

/// 最小冻结来源：无大分配遍历，正文为固定紧凑 JSON。
struct ConstantSource(&'static str);

impl SnapshotSource for ConstantSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        Ok(self.0.len())
    }

    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError> {
        out.write_all(self.0.as_bytes())
            .map_err(|_| LangfuseError::Config("test source write failed".into()))
    }
}
