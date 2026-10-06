use super::failure_text;
use crate::failure::ToolFailure;

#[test]
fn typed_failure_preserves_detail_and_recovery() {
    let error = ToolFailure::new(
        "Read before retrying",
        "failed /tmp/fixture?token=synthetic",
    );
    let text = failure_text("Write", &error);
    assert!(text.contains("failed /tmp/fixture?token=synthetic"));
    assert!(text.contains("Recovery: Read before retrying"));
}

#[test]
fn arbitrary_io_failure_preserves_diagnostic() {
    let error = std::io::Error::other("https://fixture.invalid/api?key=synthetic Bearer fixture");
    let text = failure_text("Fetch", &error);
    assert!(text.contains(&error.to_string()));
    assert!(text.contains("tool `Fetch` failed to execute"));
}

#[test]
fn failure_preserves_source_chain() {
    #[derive(Debug, thiserror::Error)]
    #[error("request failed")]
    struct RequestError(#[source] std::io::Error);
    let error = RequestError(std::io::Error::other("password=synthetic /home/fixture"));
    let text = failure_text("Request", &error);
    assert!(text.contains("request failed"));
    assert!(text.contains("Caused by: password=synthetic /home/fixture"));
}

#[test]
fn cyclic_source_chain_is_bounded_and_marked() {
    #[derive(Debug)]
    struct CyclicError;
    impl std::fmt::Display for CyclicError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("synthetic cycle")
        }
    }
    impl std::error::Error for CyclicError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(self)
        }
    }
    let text = failure_text("Cycle", &CyclicError);
    assert_eq!(text.matches("Caused by:").count(), 15);
    assert!(text.contains("[Cause chain truncated after 16 levels]"));
}

#[test]
fn failure_text_has_a_utf8_safe_total_byte_budget() {
    let error = ToolFailure::new("retry", "诊断".repeat(10000));
    let text = failure_text("Write", &error);
    assert!(text.len() <= 16 * 1024);
    assert!(text.ends_with("[Diagnostic truncated: exceeds 16 KiB limit]"));
}
