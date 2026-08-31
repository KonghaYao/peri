use super::*;

use peri_acp_types::messages::BaseMessage;

/// stdio 部署过滤 rewind/clear：stdio（true）命中 rewind/clear 返回 true。
#[test]
fn test_stdio_filters_rewind_and_clear_only_when_stdio() {
    // stdio 开启：rewind / clear（含别名解析后的 fullname）被过滤。
    assert!(super::stdio_filters_command("core:rewind", true));
    assert!(super::stdio_filters_command("core:clear", true));
    // 其它命令不过滤。
    assert!(!super::stdio_filters_command("core:compact", true));
    assert!(!super::stdio_filters_command("core:loop", true));
    assert!(!super::stdio_filters_command("core:cron", true));
    // 非 stdio（TUI/print）：一律不过滤，rewind/clear 照常作为命令。
    assert!(!super::stdio_filters_command("core:rewind", false));
    assert!(!super::stdio_filters_command("core:clear", false));
}

/// 测试 strip_leaked_prepends：有原始历史时，通过 ID 匹配定位并剥离 leaked system prepends
#[test]
fn test_strip_leaked_prepends_有历史时剥离头部system消息() {
    // Arrange: 原始历史 [Human("hello"), Ai("hi")]
    let history = [BaseMessage::human("hello"), BaseMessage::ai("hi")];
    // 模拟 execute() 错误路径返回的 messages:
    // [SystemPrepend, SystemPrompt, Human("hello"), Ai("hi"), Human("new"), Ai("response")]
    let leaked_system_1 = BaseMessage::system("injected by middleware");
    let leaked_system_2 = BaseMessage::system("system prompt");
    let result_messages = vec![
        leaked_system_1,
        leaked_system_2,
        history[0].clone(),
        history[1].clone(),
        BaseMessage::human("new question"),
        BaseMessage::ai("response"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("完整历史应保留在结果中");

    assert_eq!(cleaned.len(), 4, "应去掉2条leaked system，剩4条");
    assert_eq!(
        cleaned[0].id(),
        history[0].id(),
        "第一条应为原始历史的第一条"
    );
    assert!(!cleaned[0].is_system(), "不应包含leaked system");
}

/// 测试 strip_leaked_prepends：原始历史为空时，剥离所有头部 system 消息
#[test]
fn test_strip_leaked_prepends_空历史时剥离头部system() {
    let history: Vec<BaseMessage> = vec![];
    let result_messages = vec![
        BaseMessage::system("injected by middleware"),
        BaseMessage::system("system prompt"),
        BaseMessage::human("new question"),
        BaseMessage::ai("response"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("空历史的结果应可使用");

    assert_eq!(cleaned.len(), 2, "应去掉头部两条 system，只保留 human + ai");
    assert!(!cleaned[0].is_system(), "第一条不应是system消息");
}

/// [回归测试] 取消轮的临时 transcript 未含既有历史时，不能用它覆盖内存 history。
///
/// 历史背景：取消后的下一轮 prompt 仅从 `SessionState.history` seed；若此处返回
/// 临时结果，会使当前进程丢失前文，而重启后从 ThreadStore load 又恢复前文。
#[test]
fn test_strip_leaked_prepends_未提交full_compact时拒绝替换历史() {
    let history = [BaseMessage::human("已完成的用户消息")];
    let incomplete_result = vec![
        BaseMessage::system("system prompt"),
        BaseMessage::human("本轮用户消息"),
        BaseMessage::ai("被取消前的部分输出"),
    ];

    let cleaned = strip_leaked_prepends(
        &incomplete_result,
        history.first().map(|message| message.id()),
        false,
    );

    assert!(
        cleaned.is_none(),
        "不含原历史首条消息的 partial result 不能替换 SessionState.history"
    );
}

///
/// 取消可能发生在 compact 提交后；此时 ThreadStore 已保存 excluded flags 和摘要，
/// 若拒绝这个可见快照，下一轮会 seed 已被排除的旧消息而丢失摘要上下文。
#[test]
fn test_strip_leaked_prepends_已提交full_compact时接受替换历史() {
    let history = [BaseMessage::human("已完成的用户消息")];
    let compacted_result = vec![
        BaseMessage::system("system prompt"),
        BaseMessage::human("会话摘要"),
        BaseMessage::human("本轮用户消息"),
    ];

    let cleaned = strip_leaked_prepends(
        &compacted_result,
        history.first().map(|message| message.id()),
        true,
    );

    assert_eq!(
        cleaned.expect("已提交 Full Compact 的结果必须替换 history")[0].content(),
        "会话摘要"
    );
}

/// 测试 strip_leaked_prepends：没有 leaked prepends 时正常返回
#[test]
fn test_strip_leaked_prepends_无leaked时正常返回() {
    let history = [BaseMessage::human("hello"), BaseMessage::ai("hi")];
    let result_messages = vec![
        history[0].clone(),
        history[1].clone(),
        BaseMessage::human("new question"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("完整历史应保留在结果中");

    assert_eq!(cleaned.len(), 3, "无leaked时应正常返回所有消息");
    assert_eq!(cleaned[0].id(), history[0].id());
}

/// [AsyncContinuation] 续跑不吞 recall：clone 而非 mem::take，保留在
/// SessionState 给后续用户 prompt；续跑结束也不覆盖（不改变保留值）。
#[test]
fn test_continuation_recall_not_consumed_or_overwritten() {
    let prior_recall = vec!["上一轮留给用户 prompt 的 recall".to_string()];

    // 续跑读取：clone（不 take），SessionState 值保持不变
    let mut state_recall = prior_recall.clone();
    let incoming = take_recall_for_turn(&mut state_recall, true);
    assert_eq!(
        incoming, prior_recall,
        "续跑注入侧取到 clone（供 executor 判定后丢弃，不注入）"
    );
    assert_eq!(
        state_recall, prior_recall,
        "续跑不得 take recall——必须保留给后续用户 prompt"
    );

    // 续跑结束：不回写（result recall 不覆盖保留值）
    let continuation_result_recall = vec!["续跑产生的 recall".to_string()];
    if recall_overwrite_allowed(true) {
        state_recall = continuation_result_recall.clone();
    }
    assert_eq!(
        state_recall, prior_recall,
        "续跑结束不得改变 SessionState.recall_items"
    );

    // 对照：用户 prompt 正常 take + 回写
    let mut user_state_recall = prior_recall.clone();
    let user_incoming = take_recall_for_turn(&mut user_state_recall, false);
    assert_eq!(user_incoming, prior_recall);
    assert!(
        user_state_recall.is_empty(),
        "用户 prompt 应 take 掉 recall"
    );
    if recall_overwrite_allowed(false) {
        user_state_recall = vec!["本轮新 recall".to_string()];
    }
    assert_eq!(user_state_recall, vec!["本轮新 recall".to_string()]);
}

// ── ACP 结果投影 seam（spec/issues/2026-08-18-acp-error-handler.md D2）────────
//
// 测外部协议行为（`run_prompt` 尾部的 wire 形态决定），不断言内部局部变量：
// fatal → `Err(AcpError)`（code/message/data 契约）；cancel / max-iterations /
// end-turn → 成功 `PromptResponse`。`ExecutionFailureKind` 穷尽映射 + 脱敏
// 消息基线一并固定。
mod wire_projection {
    use peri_acp_types::{
        error::AgentError,
        session::{ExecutionFailure, ExecutionFailureKind, EXECUTION_FAILURE_FALLBACK_MESSAGE},
    };

    use super::{
        execution_failure_kind_code, execution_failure_to_acp_error, prompt_wire_response,
        ACP_TURN_EXECUTION_FAILED_CODE,
    };

    /// fatal failure → 唯一 `Internal` 类别穷尽映射到命名 code `-32000`。
    #[test]
    fn execution_failure_kind_exhaustive_mapping_pins_named_code() {
        assert_eq!(
            execution_failure_kind_code(ExecutionFailureKind::Internal),
            -32000
        );
        assert_eq!(
            execution_failure_kind_code(ExecutionFailureKind::Internal),
            ACP_TURN_EXECUTION_FAILED_CODE,
            "Internal 必须使用具名常量（替代调用点 magic number）"
        );
        assert_eq!(
            execution_failure_kind_code(ExecutionFailureKind::Llm),
            ACP_TURN_EXECUTION_FAILED_CODE
        );
        assert_eq!(
            execution_failure_kind_code(ExecutionFailureKind::LlmHttp),
            ACP_TURN_EXECUTION_FAILED_CODE
        );
    }

    /// fatal → Err：code = -32000（具名常量）、message = failure 的脱敏
    /// public message、data = 稳定 allowlist 分类。
    #[test]
    fn fatal_failure_maps_to_server_error_with_code_message_data() {
        let failure = ExecutionFailure::internal("LLM API error");
        let err = execution_failure_to_acp_error(&failure);
        assert_eq!(err.code, ACP_TURN_EXECUTION_FAILED_CODE);
        assert_eq!(err.message, "LLM API error");
        assert_eq!(err.data, Some(serde_json::json!({"kind": "internal"})));
    }

    #[test]
    fn llm_http_failure_maps_status_and_redacted_original_to_wire() {
        let failure = ExecutionFailure::from_agent_error(&AgentError::LlmHttpError {
            status: 421,
            message: "Misdirected Request token=top-secret".to_string(),
        });
        let err = execution_failure_to_acp_error(&failure);

        assert_eq!(err.code, ACP_TURN_EXECUTION_FAILED_CODE);
        assert!(err.message.contains("LLM HTTP 421"));
        assert!(err.message.contains("Misdirected Request"));
        assert!(!err.message.contains("top-secret"));
        assert_eq!(
            err.data,
            Some(serde_json::json!({"kind": "llm_http", "status": 421}))
        );
    }

    /// fatal 空 message → 非空稳定 fallback（脱敏、无内部细节）。
    #[test]
    fn fatal_failure_empty_message_falls_back_to_nonempty_safe_text() {
        let failure = ExecutionFailure::internal("");
        let err = execution_failure_to_acp_error(&failure);
        assert_eq!(err.code, ACP_TURN_EXECUTION_FAILED_CODE);
        assert_eq!(err.message, EXECUTION_FAILURE_FALLBACK_MESSAGE);
        assert!(!err.message.is_empty(), "fallback message 必须非空");
    }

    /// `prompt_wire_response`：fatal（即便 stop_reason=EndTurn）→ Err，且
    /// serialized 形态无 `data` 字段（`AcpError.data` 为 None 时跳过序列化）。
    #[test]
    fn prompt_wire_response_fatal_returns_error_with_allowlist_data() {
        let failure = ExecutionFailure::internal("middleware fatal");
        let err = prompt_wire_response(
            Some(&failure),
            crate::session::executor::PromptStopReason::EndTurn,
        )
        .expect_err("fatal failure 必须映射为 Err，不得返回成功 PromptResponse");
        assert_eq!(err.code, ACP_TURN_EXECUTION_FAILED_CODE);
        assert_eq!(err.message, "middleware fatal");
        assert_eq!(err.data, Some(serde_json::json!({"kind": "internal"})));

        let wire = serde_json::to_value(&err).expect("AcpError 序列化不应失败");
        assert_eq!(wire["code"], ACP_TURN_EXECUTION_FAILED_CODE);
        assert_eq!(wire["message"], "middleware fatal");
        assert_eq!(wire["data"], serde_json::json!({"kind": "internal"}));
    }

    /// 用户 cancel → 成功 `PromptResponse(Cancelled)`，不升级为请求错误。
    #[test]
    fn prompt_wire_response_cancel_is_success_prompt_response() {
        let value =
            prompt_wire_response(None, crate::session::executor::PromptStopReason::Cancelled)
                .expect("cancel 必须返回成功 PromptResponse");
        assert_eq!(value["stopReason"], "cancelled", "{value}");
        assert!(value.get("error").is_none(), "成功响应不应携带 error 字段");
    }

    /// 最大轮数 → 成功 `PromptResponse(MaxTurnRequests)`。
    #[test]
    fn prompt_wire_response_max_turn_requests_is_success_prompt_response() {
        let value = prompt_wire_response(
            None,
            crate::session::executor::PromptStopReason::MaxTurnRequests,
        )
        .expect("max-iterations 必须返回成功 PromptResponse");
        assert_eq!(value["stopReason"], "max_turn_requests", "{value}");
        assert!(value.get("error").is_none());
    }

    /// 正常完成 → 成功 `PromptResponse(EndTurn)`。
    #[test]
    fn prompt_wire_response_end_turn_is_success_prompt_response() {
        let value = prompt_wire_response(None, crate::session::executor::PromptStopReason::EndTurn)
            .expect("正常完成必须返回成功 PromptResponse");
        assert_eq!(value["stopReason"], "end_turn", "{value}");
        assert!(value.get("error").is_none());
    }
}
