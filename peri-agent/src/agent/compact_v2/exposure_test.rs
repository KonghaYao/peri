use super::*;
use crate::messages::MessageContent;

#[test]
fn test_exposure_requires_success_confirmation_and_exact_source() {
    let mut transcript = MessageTranscript::new();
    let original = BaseMessage::tool_result("read", "canonical result");
    let id = original.id();
    transcript.append(original.clone());
    let receipt = RequestExposure::capture(std::slice::from_ref(&original), &transcript).unwrap();
    let mut ledger = ExposureLedger::default();
    assert!(
        ledger.exposed_ids(&transcript).unwrap().is_empty(),
        "请求捕获不等于成功消费"
    );
    ledger.confirm(&receipt);
    assert!(ledger.exposed_ids(&transcript).unwrap().contains(&id));
    let mut changed = original;
    if let BaseMessage::Tool { content, .. } = &mut changed {
        *content = MessageContent::text("different result");
    }
    transcript.replace_by_id(changed);
    assert!(
        ledger.exposed_ids(&transcript).unwrap().is_empty(),
        "同ID内容变化必须重新暴露"
    );
}

#[test]
fn test_preview_and_virtual_receipt_do_not_expose_canonical_result() {
    let mut transcript = MessageTranscript::new();
    let source = BaseMessage::tool_result("read", "complete original");
    transcript.append(source.clone());
    let mut preview = source;
    if let BaseMessage::Tool { content, .. } = &mut preview {
        *content = MessageContent::text("incomplete preview");
    }
    let receipt =
        RequestExposure::capture(&[preview, BaseMessage::human("receipt")], &transcript).unwrap();
    let mut ledger = ExposureLedger::default();
    ledger.confirm(&receipt);
    assert!(
        ledger.exposed_ids(&transcript).unwrap().is_empty(),
        "占位符不能证明原文被消费"
    );
}

#[test]
fn test_exposure_cold_run_and_bound_are_conservative() {
    let mut ledger = ExposureLedger::default();
    let receipt = RequestExposure {
        members: (0..MAX_EXPOSED_MESSAGES + 1)
            .map(|_| (MessageId::new(), [1; 32]))
            .collect(),
        synchronous_terminal_calls: Vec::new(),
    };
    ledger.confirm(&receipt);
    assert_eq!(ledger.messages.len(), MAX_EXPOSED_MESSAGES);
    ledger.confirm_closed((0..MAX_EXPOSED_MESSAGES + 1).map(|id| format!("call-{id}")));
    assert_eq!(ledger.closed_calls.len(), MAX_EXPOSED_MESSAGES);
    assert!(
        !ledger.closed_calls.contains("call-0"),
        "有界遗忘只增加保护"
    );
    assert!(
        ExposureLedger::default().messages.is_empty(),
        "冷恢复不恢复执行证据"
    );
}

/// [回归测试] 冷历史只有精确全量暴露并成功响应后，才确认可信的同步终态。
#[test]
fn test_cold_terminal_evidence_requires_success_and_synchronous_typed_status() {
    use crate::tools::{ToolExecutionEvidence, ToolExecutionStatus};
    let cases = [
        ("completed", ToolExecutionStatus::Completed, None, true),
        ("failed", ToolExecutionStatus::Failed, None, true),
        ("cancelled", ToolExecutionStatus::Cancelled, None, true),
        ("unknown", ToolExecutionStatus::Unknown, None, false),
        ("running", ToolExecutionStatus::Running, None, false),
        (
            "timeout",
            ToolExecutionStatus::RunningAfterTimeout,
            None,
            false,
        ),
        (
            "unsettled-background",
            ToolExecutionStatus::Completed,
            Some("task".to_owned()),
            false,
        ),
    ];
    let mut transcript = MessageTranscript::new();
    let mut messages = Vec::new();
    for (id, status, task_id, _) in &cases {
        let message = BaseMessage::tool_result_with_execution(
            *id,
            "canonical",
            false,
            Some(ToolExecutionEvidence {
                status: *status,
                task_id: task_id.clone(),
                exit_code: None,
                output_ref: None,
                output_truncated: false,
            }),
        );
        transcript.append(message.clone());
        messages.push(message);
    }
    let receipt = RequestExposure::capture(&messages, &transcript).unwrap();
    let mut ledger = ExposureLedger::default();
    assert!(
        ledger.closed_calls().is_empty(),
        "请求失败或未确认不能确认旧终态"
    );
    ledger.confirm(&receipt);
    for (id, _, _, expected) in cases {
        assert_eq!(ledger.closed_calls().contains(id), expected, "{id}");
    }
}

/// [回归测试] 请求适配器不能用非 canonical execution 元数据伪造同步完成。
#[test]
fn test_cold_completion_reads_canonical_evidence_instead_of_request_metadata() {
    use crate::tools::{ToolExecutionEvidence, ToolExecutionStatus};
    let mut transcript = MessageTranscript::new();
    let source = BaseMessage::tool_result("call", "canonical");
    transcript.append(source.clone());
    let mut request = source;
    if let BaseMessage::Tool { execution, .. } = &mut request {
        *execution = Some(ToolExecutionEvidence {
            status: ToolExecutionStatus::Completed,
            task_id: None,
            exit_code: None,
            output_ref: None,
            output_truncated: false,
        });
    }
    let receipt = RequestExposure::capture(&[request], &transcript).unwrap();
    let mut ledger = ExposureLedger::default();
    ledger.confirm(&receipt);
    assert!(
        ledger.closed_calls().is_empty(),
        "只信任 canonical typed evidence"
    );
}
