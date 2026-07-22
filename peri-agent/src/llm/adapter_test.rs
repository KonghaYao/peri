use super::*;

/// 空脚本时不 panic，返回默认兜底答案 "Done"
#[tokio::test]
async fn test_mockllm_empty_script_returns_default() {
    let mock = MockLLM::new(vec![]);
    let r = mock.generate_reasoning(&[], &[], None).await.unwrap();
    assert!(r.final_answer.is_some(), "空脚本应返回最终答案");
    assert_eq!(r.final_answer.unwrap(), "Done");
    assert!(r.tool_calls.is_empty());
}

/// 脚本耗尽后持续返回最后一项（粘性行为）
#[tokio::test]
async fn test_mockllm_exhausted_script_sticks_to_last() {
    let mock = MockLLM::new(vec![
        Reasoning::with_answer("step0", "first"),
        Reasoning::with_answer("step1", "second"),
    ]);

    let r0 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r1 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    // 超出脚本长度，应粘在最后一项 "second"
    let r2 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r3 = mock.generate_reasoning(&[], &[], None).await.unwrap();

    assert_eq!(r0.final_answer.as_deref(), Some("first"));
    assert_eq!(r1.final_answer.as_deref(), Some("second"));
    assert_eq!(
        r2.final_answer.as_deref(),
        Some("second"),
        "脚本耗尽后应粘在最后项"
    );
    assert_eq!(
        r3.final_answer.as_deref(),
        Some("second"),
        "多次耗尽仍应粘在最后项"
    );
}

/// index 跨多次调用持续累加，不会重置
#[tokio::test]
async fn test_mockllm_index_monotonically_increases() {
    let mock = MockLLM::new(vec![
        Reasoning::with_answer("t0", "zero"),
        Reasoning::with_answer("t1", "one"),
        Reasoning::with_answer("t2", "two"),
    ]);

    let r0 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r1 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r2 = mock.generate_reasoning(&[], &[], None).await.unwrap();

    assert_eq!(r0.final_answer.as_deref(), Some("zero"));
    assert_eq!(r1.final_answer.as_deref(), Some("one"));
    assert_eq!(r2.final_answer.as_deref(), Some("two"));
}

/// always_answer 工厂：无论调用多少次始终粘在唯一答案
#[tokio::test]
async fn test_mockllm_always_answer_factory() {
    let mock = MockLLM::always_answer("fixed answer");

    let r0 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r1 = mock.generate_reasoning(&[], &[], None).await.unwrap(); // 超出，粘在唯一项

    assert_eq!(r0.final_answer.as_deref(), Some("fixed answer"));
    assert_eq!(
        r1.final_answer.as_deref(),
        Some("fixed answer"),
        "单项脚本应粘性重复"
    );
    assert!(r0.tool_calls.is_empty());
}

/// tool_then_answer 工厂：第一步有工具调用，第二步为最终答案
#[tokio::test]
async fn test_mockllm_tool_then_answer_factory() {
    let mock = MockLLM::tool_then_answer("my_tool", serde_json::json!({"key": "val"}), "final");

    let r0 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r1 = mock.generate_reasoning(&[], &[], None).await.unwrap();

    // 第一步：工具调用，无最终答案
    assert_eq!(r0.tool_calls.len(), 1);
    assert_eq!(r0.tool_calls[0].name, "my_tool");
    assert!(r0.final_answer.is_none());

    // 第二步：无工具调用，有最终答案
    assert!(r1.tool_calls.is_empty());
    assert_eq!(r1.final_answer.as_deref(), Some("final"));
}

/// 单项工具调用脚本耗尽后粘在工具调用步骤
#[tokio::test]
async fn test_mockllm_single_tool_call_script_sticks() {
    let call = ToolCall::new("id1", "echo", serde_json::json!({}));
    let mock = MockLLM::new(vec![Reasoning::with_tools("thinking", vec![call])]);

    let r0 = mock.generate_reasoning(&[], &[], None).await.unwrap();
    let r1 = mock.generate_reasoning(&[], &[], None).await.unwrap(); // 超出，粘在第一项

    assert_eq!(r0.tool_calls.len(), 1);
    assert_eq!(r0.tool_calls[0].name, "echo");
    assert_eq!(r1.tool_calls.len(), 1, "粘性：应仍返回工具调用步骤");
}
