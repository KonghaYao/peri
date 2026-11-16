//! Fixture-local archive adapter and a real successful request before pressure is raised.
//! MCP transport is exercised in peri-middlewares; this test owns failure/commit boundaries.
use peri_acp_types::context_archive::{ArchiveError, ContextArchiveReadPort};

use peri_agent::agent::stages::ActInput;
use peri_agent::agent::stages::ReasonInput;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::agent::stages::StageContext;
use peri_agent::context_archive::ContextArchiveReader;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ContextRetention;
use peri_agent::tools::ToolContext;
use std::collections::BTreeMap;
use std::sync::Arc;

struct ArchiveTool;

#[async_trait::async_trait]
impl BaseTool for ArchiveTool {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "Read canonical historical content through a session-bound archive reference."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object", "required":["archive_ref"], "properties": {
            "archive_ref":{"type":"string"}, "cursor":{"type":"string"},
            "budget_tokens":{"type":"integer"}
        }, "additionalProperties":false})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let reader = ContextArchiveReader::new(
            ctx.session_resources.ok_or(ArchiveError::Unsupported)?,
            ctx.session_id.ok_or(ArchiveError::Forbidden)?,
        );
        Ok(serde_json::to_string(
            &reader.read(serde_json::from_value(input)?).await?,
        )?)
    }
}

pub(super) fn tools() -> SharedToolMap {
    Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "ReadContextArchive".into(),
        Arc::new(ArchiveTool) as Arc<dyn BaseTool>,
    )])))
}

pub(super) async fn expose_history(ctx: &StageContext) {
    let response = peri_agent::agent::stages::reason::run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect("the run reason fixture result is required for this assertion");
    assert_eq!(response.messages_snapshot.len(), 25);
    {
        let transcript = ctx.session.transcript.read();
        assert!(transcript
            .entries()
            .iter()
            .all(|entry| !transcript.is_truncated(entry.id())));
    }
    let outcome = peri_agent::agent::stages::act::run_act(ActInput {
        context: ctx.clone(),
        reasoning: response.reasoning,
        exposure: response.exposure,
        catalog: response.catalog,
        receipt: response.receipt,
    })
    .await
    .expect("the run act fixture result is required for this assertion");
    assert_eq!(outcome.final_answer.as_deref(), Some("done"));
}
