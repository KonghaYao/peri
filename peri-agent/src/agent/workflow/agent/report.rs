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
    let contains_delimiter = body.contains(OPEN) || body.contains(CLOSE);
    (!body.is_empty()
        && (!contains_delimiter || serde_json::from_str::<serde_json::Value>(body).is_ok()))
    .then_some(body)
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
    // Compaction changes model visibility, not the committed assignment report.
    // Own entries exclude inherited reports; rewind physically truncates revoked entries.
    if let Some(body) = transcript.entries()[transcript.ancestor_len()..]
        .iter()
        .rev()
        .filter_map(|entry| entry.as_message())
        .filter(|message| matches!(message, BaseMessage::Ai { .. }))
        .find_map(|message| report_body(&message.content()).map(str::to_owned))
    {
        return body;
    }
    let messages: Vec<_> = transcript.entries()[transcript.ancestor_len()..]
        .iter()
        .filter(|entry| !transcript.flags(entry.id()).excluded)
        .filter_map(|entry| entry.as_message())
        .collect();
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
    fn json_delimiters_are_data_but_concatenated_envelopes_are_rejected() {
        let json = r#"{"text":"<workflow-result>","close":"</workflow-result>"}"#;
        let message = format!("{OPEN}{json}{CLOSE}");
        assert_eq!(report_body(&message), Some(json));
        assert_eq!(select_output([message].into_iter()), json);
        assert_eq!(
            report_body(
                "<workflow-result>one</workflow-result><workflow-result>two</workflow-result>"
            ),
            None
        );
    }

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

#[cfg(test)]
mod compact_tests {
    use super::*;
    use crate::session::FrozenContext;

    #[test]
    fn archived_own_report_survives_summary_and_optional_cleanup() {
        for cleanup in [false, true] {
            let session =
                Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
            {
                let transcript = session.transcript();
                let mut transcript = transcript.write();
                let report = transcript.append(BaseMessage::ai(
                    "<workflow-result>{\"status\":\"passed\"}</workflow-result>",
                ));
                // Full compact excludes old entries and adds a Human summary.
                transcript.set_excluded(report, true);
                transcript.append(BaseMessage::human("Compacted conversation summary"));
                if cleanup {
                    transcript.append(BaseMessage::ai("Background process cleaned up."));
                }
            }
            assert_eq!(extract_output(&session), "{\"status\":\"passed\"}");
            session.transcript().write().append(BaseMessage::ai(
                "<workflow-result>Corrected outcome</workflow-result>",
            ));
            assert_eq!(extract_output(&session), "Corrected outcome");
        }
    }

    #[test]
    fn archived_unmarked_message_is_not_a_fallback_report() {
        let session = Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
        {
            let transcript = session.transcript();
            let mut transcript = transcript.write();
            let old = transcript.append(BaseMessage::ai("obsolete unmarked output"));
            transcript.set_excluded(old, true);
            transcript.append(BaseMessage::human("Summary"));
        }
        assert_eq!(extract_output(&session), "");
        session
            .transcript()
            .write()
            .append(BaseMessage::ai("Visible output"));
        assert_eq!(extract_output(&session), "Visible output");
    }
}
