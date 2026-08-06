/// 验证 mapper.rs 对所有 ExecutorEvent 变体的处理：
/// - 8 个 Category ① 变体显式映射为 SessionUpdate
/// - 其余变体显式穷尽列出（无 wildcard `_ =>` 兜底）
///
/// `2026-07-25-event-identity-diverges-across-dual-delivery-paths.md` 要求
/// mapper 对 canonical 事件使用穷尽匹配：新增变体无法静默落入 wildcard 丢弃分支。
#[test]
fn test_all_executor_event_variants_mapped() {
    let mapper_source = include_str!("mapper.rs");

    // Category ①: 必须显式处理的变体
    let explicit_variants = [
        "TextChunk",
        "AiReasoning",
        "ToolStart",
        "ToolEnd",
        "TodoUpdate",
        "LlmCallEnd",
        "MessageAdded",
    ];

    for v in explicit_variants {
        assert!(
            mapper_source.contains(v),
            "mapper.rs 缺少 ExecutorEvent::{} 的 Category ① 显式处理分支",
            v
        );
    }

    // 非 Category ① 变体必须显式穷尽列出（禁止 wildcard 兜底）
    let exhausted_variants = [
        "StateSnapshot",
        "TurnCommitted",
        "StateSnapshotMeta",
        "TurnSuspended",
        "LlmCallStart",
        "LlmRequestPayload",
        "ContextWarning",
        "LlmRetrying",
        "BackgroundTaskCompleted",
        "SubagentStarted",
        "SubagentStopped",
        "CompactStarted",
        "CompactCompleted",
        "RewindCompleted",
        "RewindError",
        "CompactError",
        "AgentExecutionFailed",
        "LspDiagnostics",
        "BgToolStep",
        "WorkflowProgress",
        "SessionStarted",
        "TurnStarted",
        "TurnEnded",
        "MiddlewareStarted",
        "MiddlewareEnded",
        "BudgetThresholdHit",
        "WorkflowStarted",
        "WorkflowEnded",
        "BgRegistryEvent",
    ];
    for v in exhausted_variants {
        assert!(
            mapper_source.contains(v),
            "mapper.rs 缺少 ExecutorEvent::{} 的显式穷尽分支（禁止 wildcard 兜底）",
            v
        );
    }

    // 穷尽匹配：map_event 的 match 不允许 `_ =>` wildcard 兜底
    // （新增变体会在编译期被 match 拒绝）。infer_tool_kind 的 String match
    // wildcard 属另一函数，不在此断言范围。
    let map_event_body = mapper_source
        .split("fn infer_tool_kind")
        .next()
        .expect("mapper.rs 应包含 infer_tool_kind");
    assert!(
        !map_event_body.contains("_ =>"),
        "mapper.rs 的 map_event 不得包含 wildcard 分支 `_ =>`（需显式穷尽 ExecutorEvent 变体）"
    );
}
