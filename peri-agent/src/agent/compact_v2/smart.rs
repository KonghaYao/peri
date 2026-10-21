//! Smart selection uses the same Micro policy and durable commit boundary.
use crate::agent::compact_v2::config::CompactConfig;
use crate::error::AgentResult;
use crate::session::transcript::MessageTranscript;

/// Without current-run exposure facts the convenience entry conservatively keeps history.
pub async fn smart_compact(
    transcript: &mut MessageTranscript,
    config: &CompactConfig,
) -> AgentResult<(usize, u64)> {
    let plan = super::planner::plan_micro(transcript, config, true);
    let affected = super::micro::apply_micro_plan(transcript, &plan).await?;
    Ok((affected, plan.estimated_tokens_saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::BaseMessage;

    #[tokio::test]
    async fn test_smart_without_exposure_cannot_infer_consumption_from_age() {
        let mut transcript = MessageTranscript::new();
        for i in 0..100 {
            transcript.append(BaseMessage::human(format!("task {i}")));
            transcript.append(BaseMessage::tool_result(
                format!("call-{i}"),
                "old".repeat(1000),
            ));
        }
        let revision = transcript.view_revision();
        assert_eq!(
            smart_compact(&mut transcript, &CompactConfig::default())
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(transcript.view_revision(), revision);
    }
}
