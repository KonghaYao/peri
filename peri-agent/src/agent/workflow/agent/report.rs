//! An explicit assignment report survives later cleanup-only assistant messages.

use std::sync::Arc;

use crate::messages::BaseMessage;
use crate::session::Session;

const OPEN: &str = "<workflow-result>";
const CLOSE: &str = "</workflow-result>";

pub(super) fn assignment_prompt(prompt: &str) -> String {
    format!(
        "{prompt}\n\nWorkflow handoff protocol: publish the final assignment report as an entire \
         assistant message enclosed in {OPEN} and {CLOSE}. Put only the requested result inside \
         the block; if a JSON schema was requested, put valid JSON there without a Markdown fence. \
         Include the actual outcome, evidence, artifact references, blockers, and outstanding \
         resource handles when applicable. Complete required work before publishing it. \
         Keep cleanup-only notes outside the block. If new evidence changes the outcome, publish \
         a new complete block; an unmarked follow-up does not replace the committed report. \
         Do not claim verification or resource settlement without evidence."
    )
}

fn report_body(text: &str) -> Option<&str> {
    let body = text.trim().strip_prefix(OPEN)?.strip_suffix(CLOSE)?.trim();
    // A whole-message envelope avoids treating a quoted example as a handoff.
    (!body.is_empty() && !body.contains(OPEN) && !body.contains(CLOSE)).then_some(body)
}

fn select_output<T: AsRef<str>>(messages_newest_first: impl Iterator<Item = T>) -> String {
    let mut fallback = None;
    for text in messages_newest_first {
        let text = text.as_ref().trim();
        if text.is_empty() {
            continue;
        }
        fallback.get_or_insert_with(|| text.to_string());
        if let Some(body) = report_body(text) {
            return body.to_string();
        }
    }
    fallback.unwrap_or_default()
}

pub(super) fn extract_output(session: &Arc<Session>) -> String {
    let transcript = session.transcript();
    let transcript = transcript.read();
    let messages = transcript.visible_messages();
    select_output(
        messages
            .iter()
            .rev()
            .filter(|message| matches!(message, BaseMessage::Ai { .. }))
            .map(|message| message.content()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_does_not_replace_the_formal_report() {
        let messages = [
            "Background process cleaned up.",
            "<workflow-result>Fixed the timeout; regression passed.</workflow-result>",
        ];
        assert_eq!(
            select_output(messages.into_iter()),
            "Fixed the timeout; regression passed."
        );
    }

    #[test]
    fn latest_complete_report_can_correct_the_outcome() {
        let messages = [
            "<workflow-result>Blocked: the new verification failed.</workflow-result>",
            "<workflow-result>Implementation complete.</workflow-result>",
        ];
        assert_eq!(
            select_output(messages.into_iter()),
            "Blocked: the new verification failed."
        );
    }

    #[test]
    fn examples_and_legacy_output_keep_the_existing_fallback() {
        let messages = [
            "done",
            "Example: <workflow-result>not a report</workflow-result>",
        ];
        assert_eq!(select_output(messages.into_iter()), "done");
        assert_eq!(
            select_output(["", "legacy final result"].into_iter()),
            "legacy final result"
        );
        assert_eq!(select_output(std::iter::empty::<&str>()), "");
    }

    #[test]
    fn structured_payload_is_not_wrapped_or_reinterpreted() {
        assert_eq!(
            select_output(
                ["<workflow-result> {\"status\":\"blocked\"} </workflow-result>"].into_iter()
            ),
            "{\"status\":\"blocked\"}"
        );
        assert!(report_body("<workflow-result> </workflow-result>").is_none());
        assert!(report_body(
            "<workflow-result>first</workflow-result><workflow-result>second</workflow-result>"
        )
        .is_none());
    }
}
