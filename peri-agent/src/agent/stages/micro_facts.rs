//! Snapshot only the current run's consumption and the published tool policy.
use super::StageContext;
use crate::agent::compact_v2::planner::{ArchivePlanningScope, MicroPlanningFacts};
use crate::error::AgentResult;
use crate::session::{async_result_mailbox::ProtectedResults, MessageTranscript};
use peri_acp_types::context_archive::ArchiveMetadataQuery;
use peri_acp_types::session_resources::SessionResourceErrorKind;

pub(super) async fn snapshot(
    ctx: &StageContext,
    transcript: &MessageTranscript,
    protected: &ProtectedResults,
) -> AgentResult<MicroPlanningFacts> {
    let (exposed_message_ids, closed_call_ids) = {
        let ledger = ctx.compact.exposure.lock();
        (ledger.exposed_ids(transcript)?, ledger.closed_calls())
    };
    let tools: Vec<_> = ctx.runtime.tools.read().values().cloned().collect();
    let direct: Vec<_> = tools
        .iter()
        .filter(|tool| tool.is_direct() && tool.visible_to_model())
        .map(|tool| tool.as_ref())
        .collect();
    let fixed_view_tokens = ctx.runtime.llm.estimate_request_tokens(&[], &direct);
    let mut facts = MicroPlanningFacts {
        exposed_message_ids,
        closed_call_ids,
        fixed_view_tokens,
        protected_message_ids: protected
            .items
            .iter()
            .map(|item| item.delivery_id)
            .collect(),
        tool_retention: tools
            .iter()
            .map(|tool| (tool.name().to_lowercase(), tool.context_retention()))
            .collect(),
        ..Default::default()
    };
    if !tools.iter().any(|tool| {
        tool.context_retention() == crate::tools::ContextRetention::ArchiveRead
            && tool.visible_to_model()
    }) {
        return Ok(facts);
    }
    let Some((resources, thread_id, _)) = transcript.idempotent_reminder_port() else {
        return Ok(facts);
    };
    // This bounded capability probe performs no writes. The commit path subsequently verifies
    // canonical bytes after its persistence barrier, including new messages not yet durable here.
    match resources
        .read_archive_metadata(
            &thread_id,
            &ArchiveMetadataQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .await
    {
        Ok(page) => {
            facts.archive_scope = Some(ArchivePlanningScope {
                thread_id,
                committed_tail: page.committed_tail,
            })
        }
        Err(error)
            if matches!(
                error.kind(),
                SessionResourceErrorKind::Unsupported | SessionResourceErrorKind::NotFound
            ) =>
        {
            tracing::debug!(%error, "micro archive recovery unavailable; preserving candidates");
        }
        Err(error) => {
            return Err(anyhow::anyhow!("archive capability probe failed: {error}").into())
        }
    }
    Ok(facts)
}
