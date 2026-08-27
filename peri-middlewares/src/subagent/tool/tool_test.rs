use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::{
    agent::{
        events::ExecutorEvent,
        events_v2::ObserveEvent,
        react::{ReactLLM, Reasoning, StreamingContext},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    thread::ThreadStore,
    tools::BaseTool,
};
use tempfile::tempdir;

use super::*;
use crate::claude_agent_parser::ToolsValue;

// Mock LLM: returns final answer directly
struct EchoLLM;

#[async_trait::async_trait]
impl ReactLLM for EchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

fn make_tool(name: &'static str) -> Arc<dyn BaseTool> {
    struct DummyTool(&'static str);

    #[async_trait::async_trait]
    impl BaseTool for DummyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_direct(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(format!("{} result", self.0))
        }
    }

    Arc::new(DummyTool(name))
}

fn make_subagent_tool(parent_tools: Vec<Arc<dyn BaseTool>>) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
}

/// mock LangfuseBridgeLike：记录 forwarder 转发的全部 ObserveEvent
struct RecordingBridge {
    observes: Arc<std::sync::Mutex<Vec<ObserveEvent>>>,
}

impl peri_agent::agent::LangfuseBridgeLike for RecordingBridge {
    fn process_render_event(&self, _ev: &peri_agent::agent::events_v2::RenderEvent) {}

    fn process_observe_event(&self, ev: &ObserveEvent) {
        self.observes.lock().unwrap().push(ev.clone());
    }
}

/// 断言 Start/Stop 恰好一次且字段配对（agent_name / is_background / 父子 id 一致）
fn assert_start_stop_pair(evs: &[ObserveEvent], expected_name: &str, expected_bg: bool) {
    let starts: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
        .collect();
    let stops: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
        .collect();
    assert_eq!(starts.len(), 1, "SubagentStart 必须恰好一次: {:?}", evs);
    assert_eq!(stops.len(), 1, "SubagentStop 必须恰好一次: {:?}", evs);

    let (start_parent, start_child, start_name, start_bg) = match starts[0] {
        ObserveEvent::SubagentStart {
            agent_id,
            child_agent_id,
            agent_name,
            is_background,
            ..
        } => (agent_id, child_agent_id, agent_name, is_background),
        _ => unreachable!(),
    };
    let (stop_parent, stop_child, stop_name, stop_result, stop_err) = match stops[0] {
        ObserveEvent::SubagentStop {
            agent_id,
            child_agent_id,
            agent_name,
            result,
            is_error,
            ..
        } => (agent_id, child_agent_id, agent_name, result, is_error),
        _ => unreachable!(),
    };
    assert_eq!(start_name.as_str(), expected_name, "agent_name 不符");
    assert_eq!(*start_bg, expected_bg, "is_background 不符");
    assert_eq!(
        start_parent, stop_parent,
        "Start/Stop 父 agent_id 必须一致（同一次调用）"
    );
    assert_eq!(
        start_child, stop_child,
        "Start/Stop child_agent_id 必须配对（同一 subagent）"
    );
    assert_eq!(stop_name.as_str(), expected_name, "Stop agent_name 不符");
    assert!(!stop_result.is_empty() || *stop_err, "Stop 必须携带 result");
    assert!(
        uuid::Uuid::parse_str(&start_child.to_string()).is_ok(),
        "child_agent_id 必须是可解析 UUID（= child_thread_id）"
    );
}

fn write_test_agent(dir: &tempfile::TempDir) {
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();
}

fn tool_with_built_ins_disabled(cwd: &str) -> SubAgentTool {
    let state = peri_acp_types::meta_harness::MetaHarnessState {
        built_in_subagents_enabled: false,
        ..Default::default()
    };
    let parent = peri_agent::session::Session::new(
        Arc::from(cwd),
        peri_agent::session::FrozenContext::builder()
            .meta_harness(state)
            .build(),
        None,
    );
    make_subagent_tool(Vec::new()).with_parent_session(parent)
}

#[test]
fn built_in_policy_rejects_new_built_in_definition() {
    let tool = tool_with_built_ins_disabled("/nonexistent");
    let error = tool.load_agent_def("coder", "/nonexistent").unwrap_err();
    assert!(error.contains("cannot find agent definition 'coder'"));
}

#[test]
fn built_in_policy_keeps_project_override_callable() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("coder.md"),
        "---\nname: project-coder\ndescription: Project override\n---\n\nProject agent.\n",
    )
    .unwrap();
    let cwd = dir.path().to_str().unwrap();
    let agent = tool_with_built_ins_disabled(cwd)
        .load_agent_def("coder", cwd)
        .unwrap();
    assert_eq!(agent.frontmatter.name, "project-coder");
}

#[test]
fn built_in_policy_keeps_plugin_definition_callable() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugin-agents");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin-reviewer.md"),
        "---\nname: plugin-reviewer\ndescription: Plugin agent\n---\n\nReview.\n",
    )
    .unwrap();
    let tool = tool_with_built_ins_disabled(dir.path().to_str().unwrap())
        .with_plugin_agent_dirs(Arc::new(vec![plugin_dir]));
    let agent = tool
        .load_agent_def("plugin-reviewer", dir.path().to_str().unwrap())
        .unwrap();
    assert_eq!(agent.frontmatter.name, "plugin-reviewer");
}

#[test]
fn plugin_definition_loader_rejects_traversal_agent_id() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugin-agents");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        dir.path().join("outside.md"),
        "---\nname: outside\ndescription: Must not load\n---\n\nOutside.\n",
    )
    .unwrap();
    let tool = make_subagent_tool(Vec::new()).with_plugin_agent_dirs(Arc::new(vec![plugin_dir]));
    let error = tool
        .load_agent_def("../outside", dir.path().to_str().unwrap())
        .unwrap_err();
    assert!(error.contains("invalid agent definition ID"));
}

/// 构造 FilesystemThreadStore（写盘即时刷新，无需 flush）
fn make_fs_store(dir: &tempfile::TempDir) -> Arc<peri_agent::thread::FilesystemThreadStore> {
    Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    ))
}

/// 预置可恢复 thread：创建（title 决定工具集恢复路径）+ 写消息 + 置非 active。
/// FilesystemThreadStore 写盘即时落库（append 后 load_messages 立即可见）。
async fn preset_resumable_thread(
    store: &Arc<peri_agent::thread::FilesystemThreadStore>,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    let id = id.to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new("/tmp/work");
    meta.id = id.clone();
    meta.title = Some(title.to_string());
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    meta.hidden = true;
    store.create_thread(meta).await.unwrap();
    if !msgs.is_empty() {
        store.append_messages(&id, &msgs).await.unwrap();
    }
    store.update_thread_status(&id, "done").await.unwrap();
}

// 本文件经 mod.rs 的 `#[path = "tool_test.rs"]` 挂载；此路径加载方式下，
// rustc 不会为聚合根派生 `tool_test/` 子目录，子模块需显式 `#[path]` 指向。
#[path = "tool_test/bg_register_cancel_test.rs"]
mod bg_register_cancel_test;
#[path = "tool_test/dynamic_mcp_subagent_test.rs"]
mod dynamic_mcp_subagent_test;
#[path = "tool_test/events_contract_test.rs"]
mod events_contract_test;
#[path = "tool_test/fork_test.rs"]
mod fork_test;
#[path = "tool_test/integration_v2_test.rs"]
mod integration_v2_test;
#[path = "tool_test/invoke_test.rs"]
mod invoke_test;
#[path = "tool_test/middleware_chain_test.rs"]
mod middleware_chain_test;
#[path = "tool_test/model_tier_test.rs"]
mod model_tier_test;
#[path = "tool_test/resume_integration_test.rs"]
mod resume_integration_test;
#[path = "tool_test/resume_test.rs"]
mod resume_test;
#[path = "tool_test/tool_filter_test.rs"]
mod tool_filter_test;
