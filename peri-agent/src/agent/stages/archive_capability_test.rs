use super::*;
use crate::agent::stages::StageContext;
use crate::messages::BaseMessage;
use crate::session::{test_resources::TestSession, FrozenContext, MessageTranscript, Session};
use crate::tools::{BaseTool, ToolContext};
use peri_acp_types::{
    context_archive::ArchiveRoot, projection::MessageProjectionDirective, store::MessageFlags,
};
use std::sync::Arc;

struct RecoveryTool;
#[async_trait::async_trait]
impl BaseTool for RecoveryTool {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "Archive recovery"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!()
    }
}

async fn make_context(inherited: bool) -> (TestSession, StageContext) {
    let bound = TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/archive-capability"),
        FrozenContext::builder().build(),
        Some(bound.thread_id()),
    );
    let summary = BaseMessage::ai("inherited archive summary");
    let id = summary.id();
    let root_id = crate::messages::MessageId::new();
    let transcript = if inherited {
        MessageTranscript::new().with_ancestor(vec![summary])
    } else {
        MessageTranscript::new().with_own_payloads(vec![
            peri_acp_types::store::PersistedPayload::Message(summary),
        ])
    };
    let mut transcript = transcript.with_persistence(bound.resources(), bound.thread_id());
    transcript.set_flags_batch(std::collections::HashMap::from([(
        id,
        MessageFlags {
            projection: Some(MessageProjectionDirective {
                policy_version: 3,
                archive_roots: vec![ArchiveRoot {
                    source_thread_id: "parent".into(),
                    first_anchor: root_id,
                    last_anchor: root_id,
                    committed_tail: root_id,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    )]));
    *session.transcript().write() = transcript;
    let tools = Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::from(
        [(
            "ReadContextArchive".into(),
            Arc::new(RecoveryTool) as Arc<dyn BaseTool>,
        )],
    )));
    let ctx = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_tools(tools)
    .build();
    // 临时数据库/仓库目录由 TestSession 持有，必须陪同比存储探测更久地存活。
    (bound, ctx)
}

/// [回归测试] 只有继承摘要、own append尚未落盘时，当前会话NotFound不等于缺失归档能力。
#[tokio::test]
async fn test_archive_capability_accepts_empty_own_history_with_inherited_roots() {
    let (bound, ctx) = make_context(true).await;
    let result = validate(&ctx, &ctx.runtime.tool_catalog.snapshot()).await;
    drop(bound);
    result.unwrap();
}

/// [回归测试] 已提交own投影的存储变为NotFound时，必须停止而不能携带省略视图继续。
#[tokio::test]
async fn test_archive_capability_rejects_missing_committed_own_archive() {
    let (bound, ctx) = make_context(false).await;
    let error = validate(&ctx, &ctx.runtime.tool_catalog.snapshot())
        .await
        .unwrap_err();
    drop(bound);
    assert!(
        error
            .to_string()
            .contains("context_archive_capability_required"),
        "{error}"
    );
}
