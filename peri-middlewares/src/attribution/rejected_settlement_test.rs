//! 回归：被拒绝的 Write/Edit 不得把旧文件全文留在归因中间件。
//!
//! 拒绝发生在审批阶段：`before_tool` 已暂存旧正文，但调用不会执行，也不会触发
//! `after_tool`。暂存必须按被拒调用结算，同批其它调用仍以各自快照计算贡献。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};

use peri_agent::agent::react::Reasoning;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::stages::tool_dispatch;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::agent::stages::StageContext;
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::middleware::MiddlewareChain;
use peri_agent::session::tool_catalog::SessionToolCatalog;
use peri_agent::session::FrozenContext;
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::workspace_io::WorkspaceFileReader;
use crate::workspace_io::WorkspaceReadError;
use crate::ExecuteExtraToolResolver;
use crate::GitAttributionMiddleware;

/// 虚拟工作区：路径的文件名 → 正文。Reader 与 Write 工具共享同一份事实。
type VirtualFs = Arc<Mutex<HashMap<String, String>>>;

fn fs_key(path: &Path) -> String {
    path.file_name()
        .expect("test paths always carry a file name")
        .to_string_lossy()
        .into_owned()
}

struct VirtualFsReader(VirtualFs);

#[async_trait]
impl WorkspaceFileReader for VirtualFsReader {
    async fn read_text(&self, path: &Path) -> Result<String, WorkspaceReadError> {
        self.0
            .lock()
            .get(&fs_key(path))
            .cloned()
            .ok_or(WorkspaceReadError::ReadFailed)
    }
}

struct VirtualWriteTool(VirtualFs);

#[async_trait]
impl BaseTool for VirtualWriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "write into the virtual workspace"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["file_path", "content"]
        })
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let path = input["file_path"]
            .as_str()
            .expect("Write requires file_path");
        let content = input["content"].as_str().expect("Write requires content");
        self.0
            .lock()
            .insert(fs_key(Path::new(path)), content.to_string());
        Ok("written".to_string())
    }
}

/// 模拟审批中间件：只拒绝指定调用，其余放行。
struct RejectMatchingCall(&'static str);

#[async_trait]
impl Middleware for RejectMatchingCall {
    fn name(&self) -> &str {
        "RejectMatchingCall"
    }

    async fn before_tool(
        &self,
        _state: &mut dyn hook_state::BeforeToolState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        if tool_call.id == self.0 {
            return Err(AgentError::ToolRejected {
                tool: tool_call.name.clone(),
                reason: "rejected by regression fixture".to_string(),
                cause: peri_acp_types::error::ToolRejectionCause::UserRejected,
            });
        }
        Ok(tool_call.clone())
    }
}

fn make_context(
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
    chain: MiddlewareChain,
) -> StageContext {
    let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let shared: SharedToolMap = Arc::new(RwLock::new(tools));
    let catalog = Arc::new(
        SessionToolCatalog::try_new(shared.read().clone(), None)
            .expect("test fixture catalog must be constructible"),
    );
    StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_tools(shared)
        .with_tool_catalog(catalog)
        .with_tool_invocation_resolver(Arc::new(ExecuteExtraToolResolver::default()))
        .with_middleware_chain(Arc::new(chain))
        .build()
}

fn pending_stats(pending: &std::sync::Mutex<HashMap<String, String>>) -> (usize, usize) {
    let pending = pending.lock().unwrap();
    (pending.len(), pending.values().map(String::len).sum())
}

#[tokio::test]
async fn rejected_write_settles_only_its_staged_old_content() {
    let old_content = "o".repeat(4096);
    let fs: VirtualFs = Arc::new(Mutex::new(HashMap::from([
        ("rejected.txt".to_string(), old_content.clone()),
        ("accepted.txt".to_string(), "old".to_string()),
    ])));

    let attribution =
        GitAttributionMiddleware::new("test-model", Arc::new(VirtualFsReader(Arc::clone(&fs))));
    let pending = Arc::clone(&attribution.pending_old_content);
    let contributions = Arc::clone(&attribution.state);

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(attribution));
    chain.add(Box::new(RejectMatchingCall("rejected")));

    let write: Arc<dyn BaseTool> = Arc::new(VirtualWriteTool(Arc::clone(&fs)));
    let context = make_context(BTreeMap::from([("Write".to_string(), write)]), chain);

    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new(
                "rejected",
                "Write",
                json!({"file_path": "rejected.txt", "content": "after"}),
            ),
            ToolCall::new(
                "accepted",
                "Write",
                json!({"file_path": "accepted.txt", "content": "new content"}),
            ),
        ],
    );

    let outcome = tool_dispatch::dispatch_tools(
        &context,
        &reasoning,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
    )
    .await
    .expect("dispatch must settle the rejection as a tool error");

    assert_eq!(outcome.results.len(), 2);
    let result_by_id: HashMap<_, _> = outcome
        .results
        .iter()
        .map(|(_, result)| (result.tool_call_id.as_str(), result))
        .collect();
    assert!(result_by_id["rejected"].is_error);
    assert!(!result_by_id["accepted"].is_error);

    assert_eq!(
        pending_stats(&pending),
        (0, 0),
        "被拒调用不得留下暂存条目/字节"
    );

    let contributions = contributions.lock().unwrap();
    assert!(
        !contributions.contributions.contains_key("rejected.txt"),
        "被拒调用不得计入贡献"
    );
    assert_eq!(
        contributions.contributions["accepted.txt"].claude_chars, 11,
        "同批在执行调用必须用自身旧快照计算贡献"
    );
}
