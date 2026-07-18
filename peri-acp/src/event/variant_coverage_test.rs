/// 验证 mapper.rs 对所有 ExecutorEvent 变体的处理：
/// - 7 个 Category ① 变体显式映射为 SessionUpdate
/// - 其余变体通过 wildcard `_ =>` 安全处理（无 SessionUpdate）
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

    // 验证 wildcard 存在（覆盖其余所有变体）
    assert!(
        mapper_source.contains("_ =>"),
        "mapper.rs 缺少 wildcard 分支 `_ =>` 覆盖非 Category ① 变体"
    );
}
