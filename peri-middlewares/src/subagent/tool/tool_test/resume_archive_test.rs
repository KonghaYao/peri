//! Permission drift must stop a restored projection before any model request.
use super::Arc;
use super::BaseMessage;
use super::BaseTool;
use super::SessionFixture;
use super::SubAgentTool;
use super::ThreadMeta;
use peri_acp_types::context_archive::{ArchiveReadRequest, ContextArchiveReadPort};
use peri_agent::agent::compact_v2::micro;
use peri_agent::agent::compact_v2::planner;
use peri_agent::agent::compact_v2::planner::ArchivePlanningScope;
use peri_agent::agent::compact_v2::planner::ContextPressure;
use peri_agent::agent::compact_v2::planner::MicroPlanningFacts;
use peri_agent::agent::compact_v2::projection::ProviderCapabilities;
use peri_agent::agent::compact_v2::CompactConfig;
use peri_agent::session::MessageTranscript;
use peri_agent::tools::ContextRetention;

struct RecoveryTool;
#[async_trait::async_trait]
impl BaseTool for RecoveryTool {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "Read archived context"
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
        _: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("权限验证不能执行工具，原文读取由真实Reader单独验证")
    }
}

#[derive(Clone)]
struct ArchiveResumeModel(Arc<std::sync::atomic::AtomicUsize>);
impl ArchiveResumeModel {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support;
        let tools = test_support::defined_tools(&request);
        assert!(
            tools.iter().any(|t| t.name() == "ReadContextArchive"),
            "恢复后的最终请求必须包含回查工具"
        );
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        test_support::text_events("archive-resume-complete")
    }
}
crate::subagent::test_support::fixture_model_impl!(ArchiveResumeModel);

async fn permission_drift(retention: ContextRetention) {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    let definition = agents.join("archived-child.md");
    let write_definition = |archive: bool| {
        std::fs::write(&definition, format!("---\nname: archived-child\ndescription: Archive permission lifecycle\ntools:\n  - Read\n{}---\nResume existing context.\n", if archive { "  - ReadContextArchive\n" } else { "" })).unwrap();
    };
    write_definition(true);
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let parent = peri_agent::session::Session::new(
        Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let capture = super::install_parent_host(&store, &parent);
    let child = uuid::Uuid::now_v7().to_string();
    super::preset_resumable_thread(&store, &child, "archived-child", Some(&parent_id), vec![])
        .await;
    let mut transcript = MessageTranscript::new().with_persistence(store.facade(), child.clone());
    transcript.append(BaseMessage::human("保留审计原文"));
    for i in 0..6 {
        let call = format!("read-{i}");
        transcript.append(BaseMessage::ai_with_tool_calls(
            "",
            vec![peri_agent::messages::ToolCallRequest::new(
                &call,
                "Read",
                serde_json::json!({"path":"old.rs"}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(
            &call,
            format!("old-{i}:{}", "historical content".repeat(2000)),
        ));
    }
    let mut facts = MicroPlanningFacts {
        archive_scope: Some(ArchivePlanningScope {
            thread_id: child.clone(),
            committed_tail: transcript.entries().last().unwrap().id(),
        }),
        ..Default::default()
    };
    facts.tool_retention.insert("read".into(), retention);
    for entry in transcript.entries() {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            facts
                .closed_call_ids
                .extend(message.tool_calls().iter().map(|call| call.id.clone()));
        }
    }
    let plan = planner::plan_micro_with_facts(
        &transcript,
        &CompactConfig {
            micro_compact_stale_steps: 0,
            ..Default::default()
        },
        &ContextPressure {
            estimated_tokens: 6000,
            context_window: 8000,
            output_reserve: 1000,
            predicted_tool_growth: 0,
            safety_buffer: 0,
            cache_hit_rate: 0.0,
        },
        &facts,
        &ProviderCapabilities::openai(),
    )
    .unwrap();
    assert!(!plan.catalogs.is_empty(), "测试必须实际提交归档投影");
    let reference = plan.catalogs[0].member_ref(0).unwrap();
    assert!(
        micro::apply_micro_plan(&mut transcript, &plan)
            .await
            .unwrap()
            > 0
    );
    transcript.shutdown_persistence();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let tool = SubAgentTool::new(
        Arc::new(vec![super::make_tool("Read"), Arc::new(RecoveryTool)]),
        None,
        Arc::new(move |_| {
            crate::subagent::test_support::fixture_source(
                Arc::new(ArchiveResumeModel(counter.clone())),
                "fixture-scripted",
            )
        }),
        cwd.clone(),
    )
    .with_session_resources(store.facade())
    .with_parent_thread_id(parent_id)
    .with_parent_session(parent);
    write_definition(false);
    let tool = super::with_agent_face(tool, dir.path()).await;
    let receipt = tool
        .invoke(
            serde_json::json!({"resume_thread_id":child,"cwd":cwd}),
            super::preset_child_ctx(&child, "."),
        )
        .await
        .expect("v5 resume 恒后台：权限漂移在子执行中失败");
    assert!(receipt.contains("Background task"), "{receipt}");
    let failure = capture.wait().await;
    assert!(!failure.success, "权限漂移必须落为失败终态");
    let safe_failure = failure
        .subagent_failure
        .as_ref()
        .expect("typed child failure must survive background settlement");
    let failure_text = format!(
        "{} {}",
        safe_failure.message(),
        safe_failure.causes().join(" ")
    );
    assert!(
        failure_text.contains("context_archive_capability_required"),
        "权限漂移必须明确报错: {failure_text}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "不可发出缺少回查能力的模型请求"
    );
    write_definition(true);
    let tool = super::with_agent_face(tool, dir.path()).await;
    let success = capture
        .invoke_and_wait(
            &tool,
            serde_json::json!({"resume_thread_id":child,"cwd":cwd}),
            super::preset_child_ctx(&child, "."),
        )
        .await
        .unwrap();
    assert!(success.contains("archive-resume-complete"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let reader = peri_agent::context_archive::ContextArchiveReader::new(store.facade(), child);
    let page = reader
        .read(ArchiveReadRequest {
            archive_ref: reference,
            cursor: None,
            budget_tokens: None,
        })
        .await
        .unwrap();
    assert!(!page.entries[0].data.is_empty(), "恢复权限后旧引用仍可用");
}

/// [回归测试] 定义收紧权限时，已有preview也必须在低于75%的Resume请求前停止。
#[tokio::test]
async fn test_resume_archive_preview_permission_removed_then_restored() {
    permission_drift(ContextRetention::PreviewOnly).await;
}

/// [回归测试] 整组省略的历史不得在回查工具被过滤后继续发送给模型。
#[tokio::test]
async fn test_resume_archive_whole_omission_permission_removed_then_restored() {
    permission_drift(ContextRetention::Recomputable).await;
}
