// Responsibility-specific cases share the original test namespace and fixtures.

/// 流永远 pending，绝不产出 Completed；取消由 bridge 侧处理。
struct CancellingModel;

#[async_trait]
impl Model for CancellingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

/// 取消前先 emit 一个 TextDelta，随后永久 pending。
struct HalfStreamingModel {
    reasoning: bool,
}

#[async_trait]
impl Model for HalfStreamingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let delta = if self.reasoning {
            ModelStreamEvent::ReasoningDelta {
                text: "partial".into(),
            }
        } else {
            ModelStreamEvent::TextDelta {
                text: "partial".into(),
            }
        };
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(delta)]).chain(stream::pending::<ModelResult<ModelStreamEvent>>()),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn bridge_maps_precancelled_token_to_interrupted_without_events() {
    let bridge = AgentModelBridge::from_arc(Arc::new(CancellingModel));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        cancel,
        call_context: None,
    };

    let result = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await;

    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "预取消 token 应映射为 Interrupted，实际 {:?}",
        result
    );
    assert!(
        handles.render_rx.try_recv().is_err(),
        "取消后不得 emit 任何 RenderEvent"
    );
}

async fn assert_bridge_stops_emitting_after_cancellation(reasoning: bool) {
    let bridge = AgentModelBridge::from_arc(Arc::new(HalfStreamingModel { reasoning }));
    let cancel = CancellationToken::new();
    let cancel_on_first_render = cancel.clone();
    let (bus, handles) = EventBus::new(EventBusConfig::default());
    // v2 直发后无法在发射点挂钩 cancel：观察任务在首个 Render 事件到达时取消，
    // 并统计取消后的残留事件（bridge 必须在 poll 下一项前中断，不得再 emit）。
    let mut handles_for_watcher = handles;
    let watcher = tokio::spawn(async move {
        let first = handles_for_watcher.render_rx.recv().await;
        if first.is_none() {
            return 0; // 通道关闭（未收到事件）
        }
        cancel_on_first_render.cancel();
        let mut extra = 0;
        while handles_for_watcher.render_rx.recv().await.is_some() {
            extra += 1;
        }
        extra
    });
    let streaming = StreamingContext {
        event_bus: Arc::new(bus),
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        cancel,
        call_context: None,
    };

    let result = bridge
        .generate_reasoning(&[BaseMessage::human("hello")], &[], Some(streaming))
        .await;

    let extra_events = watcher
        .await
        .expect("the watcher fixture result is required for this assertion");
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "流中途取消应映射为 Interrupted，实际 {:?}",
        result
    );
    assert_eq!(
        extra_events, 0,
        "取消后不得再 emit 事件（残留 {} 个）",
        extra_events
    );
}

#[tokio::test]
async fn bridge_stops_emitting_events_after_mid_stream_cancellation() {
    assert_bridge_stops_emitting_after_cancellation(false).await;
}

#[tokio::test]
async fn bridge_stops_emitting_events_after_mid_thinking_cancellation() {
    assert_bridge_stops_emitting_after_cancellation(true).await;
}
