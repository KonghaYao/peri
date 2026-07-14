/// 验证每个 ExecutorEvent 变体都在 mapper.rs 中有对应处理（防止漏映射）。
/// 用字符串 grep 验证 mapper.rs 覆盖，新增变体时手动在 variants 列表追加。
#[test]
fn test_all_executor_event_variants_mapped() {
    let mapper_source = include_str!("mapper.rs");

    // 列举所有应该被 mapper 处理的变体名
    let variants = [
        "SessionStarted",
        "TurnStarted",
        "TurnEnded",
        "StageStarted",
        "StageEnded",
        "MiddlewareStarted",
        "MiddlewareEnded",
        "AiReasoningChunk",
        "BudgetThresholdHit",
        "MessageQueueDrained",
        "WorkflowStarted",
        "WorkflowEnded",
        "CompactStarted",
        "CompactCompleted",
        "LlmCallStart",
        "LlmCallEnd",
        "LlmRetrying",
        "LlmRequestPayload",
        "ToolStart",
        "ToolEnd",
        "TextChunk",
        "AiReasoning",
        "SubagentStarted",
        "SubagentStopped",
        "ContextWarning",
        "StateSnapshot",
        "StateSnapshotMeta",
        "TurnCommitted",
        "TurnSuspended",
        "RewindCompleted",
        "BackgroundTaskCompleted",
        "BgToolStep",
        "LspDiagnostics",
        "AgentExecutionFailed",
        "WorkflowProgress",
        "TodoUpdate",
        "MessageAdded",
        "CompactError",
    ];

    for v in variants {
        assert!(
            mapper_source.contains(v),
            "mapper.rs 缺少 ExecutorEvent::{} 的处理分支",
            v
        );
    }
}
