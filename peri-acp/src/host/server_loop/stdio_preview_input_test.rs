use super::dispatch_error;
use peri_agent::session::user_input_mailbox::UserInputQueueError;

#[test]
fn confirmed_withdrawal_reports_store_failure_as_retryable_internal_error() {
    let error = dispatch_error(UserInputQueueError::Store("mq down".into()));
    assert_eq!(error.code, -32603);
    assert!(error.message.contains("mq down"));
    let invalid = dispatch_error(UserInputQueueError::InvalidIdentity);
    assert_eq!(invalid.code, -32602);
}
