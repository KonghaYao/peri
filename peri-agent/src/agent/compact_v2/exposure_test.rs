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
    };
    ledger.confirm(&receipt);
    assert_eq!(ledger.messages.len(), MAX_EXPOSED_MESSAGES);
    assert!(
        ExposureLedger::default().messages.is_empty(),
        "冷恢复不恢复执行证据"
    );
}
