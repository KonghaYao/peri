//! `workspace` builtin MCP handler wraps six filesystem tools and `BashTool`; it does not define
//! a second tool schema or execution path. `peri-mcp-common` owns tools/list conversion,
//! tools/call mapping, and safe failure projection.
//!
//! Construction freezes one host cwd and gives it to all seven tools. This does not create a
//! capability root: filesystem tools can still address absolute paths outside cwd, and Bash is
//! not a sandbox. Do not infer stronger isolation from cwd binding.
//!
//! `WorkspaceInstanceInput` is session-scoped and passes the session task manager and background
//! completion callback to Bash unchanged. `None` is supported: all seven tools remain visible,
//! while Bash rejects explicit background execution, kills a timed-out foreground process group,
//! and cannot register shell descendants left behind by `command &`.
//!
//! The handler does not override `discover`; besides the seven tools it exposes exactly one
//! resource (`workspace://git/ref`, Git ref 快照) and the 2026-07-28 subscription surface
//! (`accepted_subscription_filter` + `listen`) that carries git ref changes to the host.
//! Tool failures use shared allowlisted recovery text; request cancellation drops the tool
//! future.

use std::{collections::HashMap, sync::Arc, time::Duration};

use peri_agent::tools::BaseTool;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, Implementation, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        RequestId, Resource, ResourceContents, ServerCapabilities, ServerInfo, SubscriptionFilter,
    },
    service::{RequestContext, RoleServer, SubscriptionContext, SubscriptionSink},
    ErrorData as McpError, ServerHandler,
};

use peri_mcp_common::{invoke_tool_call, list_tools_of};

use crate::filesystem::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};
use crate::git_watch::{run_git_sample, GitWatchState, SampleOutcome, GIT_REF_RESOURCE_URI};
use crate::terminal::BashTool;
use crate::WorkspaceInstanceInput;

/// `workspace` 实例的 `ServerInfo` 名字（`Implementation::name`）；实例名仍是注册表里的
/// `"workspace"`（`Implementation::name` 与注册表 key 是两件事，不要合并）。
const WORKSPACE_SERVER_NAME: &str = "peri-workspace-mcp";

/// git ref 资源的展示名（与 URI 一起出现在 `resources/list`）。
const GIT_REF_RESOURCE_NAME: &str = "git-ref";

/// `workspace` 实例的 handler：持有 7 个既有工具（**同一份** `BaseTool` 实现）与一个
/// 构造期冻结的 host cwd。
///
/// cwd 在构造时取一次；`list_tools` 不重算、运行期不可变更（与 pool 的 `execution_cwd`
/// 同为 `OnceLock` 语义，AW3-05：不支持多 cwd）。
pub struct WorkspaceMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    /// 工具面共享的 host cwd（文件工具相对路径解析 + Bash `current_dir`）。
    cwd: String,
    /// git ref 采样状态机（`workspace://git/ref` 资源正文 + 变化判定）。
    git: Arc<GitWatchState>,
    /// 活跃订阅 sink 表（键 = `subscriptions/listen` 请求 id）。
    ///
    /// 空表 = 无订阅者：`call_tool` 成功后的触发点直接返回，**不产生任何 git 调用**。
    /// 通知在 `tokio::spawn` 的采样收口里逐 sink 发送；`SubscriptionClosed` 的 sink
    /// 就地移除（客户端已 drop 订阅）。
    sinks: Arc<parking_lot::Mutex<HashMap<RequestId, SubscriptionSink>>>,
}

impl WorkspaceMcpServer {
    /// 生产构造：按 cwd 实例化 7 个既有工具，并把 session 级输入（若有）转交 `BashTool`。
    ///
    /// 顺序 = 注册表 `WORKSPACE_TOOLS` 的声明顺序（`list_tools_of` 保留本向量顺序）：
    /// 6 个文件工具 Read → Write → Edit → Glob → Grep → folder_operations，`Bash` 作为
    /// 唯一执行类工具排在末位（与 `namespace()` 的 `filesystem` / `execution` 分组一致）。
    /// v4-part-4 W3-C1 后链上已无 middleware 提供面（原 `FilesystemMiddleware::build_tools`
    /// 的顺序参照随之失效）：本段顺序的**唯一**事实源是注册表声明顺序，两处由
    /// `workspace_test.rs` 的声明段用例锁定。
    ///
    /// `input` 的两名成员各自独立生效（`task_manager` 与 `on_bg_complete` 由不同装配面产出，
    /// 本构造不假定它们同时到位）：`None` 时对应字段保持 `BashTool::new` 的缺省 `None`
    /// （退化分支见模块头），其余 6 个工具不受 `input` 影响。
    pub fn new(cwd: impl Into<String>, input: Option<WorkspaceInstanceInput>) -> Self {
        let cwd = cwd.into();
        let mut bash = BashTool::new(cwd.as_str());
        if let Some(input) = input {
            if let Some(task_manager) = input.task_manager {
                bash = bash.with_task_manager(task_manager);
            }
            if let Some(on_bg_complete) = input.on_bg_complete {
                bash = bash.with_on_bg_complete(on_bg_complete);
            }
        }
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(ReadFileTool::new(cwd.as_str())),
            Arc::new(WriteFileTool::new(cwd.as_str())),
            Arc::new(EditFileTool::new(cwd.as_str())),
            Arc::new(GlobFilesTool::new(cwd.as_str())),
            Arc::new(GrepTool::new(cwd.as_str())),
            Arc::new(FolderOperationsTool::new(cwd.as_str())),
            Arc::new(bash),
        ];
        Self {
            tools,
            cwd,
            git: Arc::new(GitWatchState::new()),
            sinks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    /// 覆盖 git 采样节流窗口（**测试 seam**：生产恒为 `GIT_THROTTLE`，60s）。
    ///
    /// 线路级用例需要在同一条链路上观察「commit → 第二次采样 → 通知」，60s 的真实节流
    /// 窗口不可等待；本方法只改状态机的窗口，不改变触发语义（单飞 / 短路 / 失败不推进
    /// 节流都不变）。
    pub fn with_git_throttle(mut self, throttle: Duration) -> Self {
        self.git = Arc::new(GitWatchState::with_timing(
            throttle,
            crate::git_watch::GIT_SAMPLE_TIMEOUT,
        ));
        self
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }

    /// `call_tool` 成功返回后的采样触发点（原宿主 `GitWatchMiddleware::after_tool` 的
    /// 位置；触发门逐字保持：仅成功、`is_error != Some(true)`）。
    ///
    /// 「无订阅者 ⇒ 不采样」在此落地：空 sink 表直接返回，零 git 调用。
    fn spawn_git_sample(&self) {
        if self.sinks.lock().is_empty() {
            return;
        }
        if !self.git.begin_sample() {
            return;
        }

        let git = Arc::clone(&self.git);
        let sinks = Arc::clone(&self.sinks);
        let cwd = self.cwd.clone();
        tokio::spawn(async move {
            let outcome =
                match tokio::time::timeout(git.sample_timeout(), run_git_sample(&cwd)).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        tracing::warn!(
                            target: "git_watch",
                            cwd = %cwd,
                            timeout_ms = git.sample_timeout().as_millis(),
                            "git sample timed out"
                        );
                        // 超时按失败收口：不推进节流、不通知（旧实现同口径）。
                        git.finish_sample(SampleOutcome::Failed);
                        return;
                    }
                };

            let Some(notice) = git.finish_sample(outcome) else {
                return;
            };
            tracing::debug!(target: "git_watch", cwd = %cwd, "git ref 已变化，推送资源更新通知");

            // 快照 sink 表后逐条发送：发送期间新建立的订阅由它自己的 listen 注册，
            // 不要求本条通知补发（下一次变化自然会带上它）。
            let targets: Vec<(RequestId, SubscriptionSink)> = sinks
                .lock()
                .iter()
                .map(|(id, sink)| (id.clone(), sink.clone()))
                .collect();
            for (id, sink) in targets {
                if let Err(error) = sink.notify_resource_updated(GIT_REF_RESOURCE_URI).await {
                    match error {
                        // 客户端已 drop 订阅：就地移除，避免表无限增长。
                        rmcp::service::SubscriptionSendError::SubscriptionClosed => {
                            sinks.lock().remove(&id);
                        }
                        other => {
                            tracing::warn!(target: "git_watch", error = %other, "资源更新通知发送失败");
                        }
                    }
                }
            }
            let _ = notice;
        });
    }
}

impl ServerHandler for WorkspaceMcpServer {
    // 不覆写 `discover`（§10 R2：覆写会让 `tools/list` 被会话层拒绝）。

    /// 能力位（D-5 的机制前提）：`resources.subscribe = true` 是 SDK 受理
    /// `subscriptions/listen` 并按 filter 放行 `notifications/resources/updated` 的必要
    /// 条件（SDK 把 handler filter 与 capabilities 求交，未声明时 filter 收窄为空）。
    ///
    /// **不复用** `peri_mcp_common::server_info`：它的固定形状是 tools-only。
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new(
            WORKSPACE_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(list_tools_of(self.tools()))
    }

    /// 单条资源：`workspace://git/ref`（正文 = 最近一次采样结论，见 `git_watch`）。
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![Resource::new(
            GIT_REF_RESOURCE_URI,
            GIT_REF_RESOURCE_NAME,
        )
        .with_mime_type("text/plain")]))
    }

    /// 读取资源正文；未命中的 URI 按协议回 `-32602`（`invalid_params`）。
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != GIT_REF_RESOURCE_URI {
            return Err(McpError::invalid_params(
                format!("unknown resource: {}", request.uri),
                None,
            ));
        }
        Ok(ReadResourceResponse::Complete(
            rmcp::model::ReadResourceResult::new(vec![ResourceContents::text(
                self.git.resource_text(),
                GIT_REF_RESOURCE_URI,
            )]),
        ))
    }

    /// 只接受 git ref 资源 URI（与请求 filter 求交；其余类别不声明 ⇒ 请求它们不会被 ack）。
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let candidate = SubscriptionFilter::builder()
            .resource_subscriptions([GIT_REF_RESOURCE_URI])
            .build();
        Some(requested.intersection(&candidate))
    }

    /// 订阅生命周期：注册 sink → 等待取消（客户端 drop 订阅 / 连接关闭）→ 注销。
    ///
    /// ack 由 SDK 在本方法被调用**之前**发出（`SubscriptionContext::establish`），
    /// 因此这里只做注册与收口。
    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        let id = context.sink().id().clone();
        self.sinks.lock().insert(id.clone(), context.sink().clone());
        context.cancelled().await;
        self.sinks.lock().remove(&id);
        Ok(())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // 传实例冻结的 host cwd（不走 `web` / `cron` / `lsp` 的空串形态）。**注意**：本参
        // 只落进 `ToolContext`，而本波 7 个工具都忽略 `ToolContext`（`invoke` 的 `_ctx`），
        // 真正生效的 cwd 绑定点是 [`Self::new`] 注入各工具的 `cwd` 字段——不得据本行推断
        // 「cwd 由 `tools/call` 决定」（反例实验证据见 `workspace_test.rs` 模块头）。
        let response = tokio::select! {
            biased;
            _ = context.ct.cancelled() => Ok(CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("Tool execution cancelled.")]))),
            result = invoke_tool_call(self.tools(), &self.cwd, &request) => result,
        };
        // D-1 触发语义：仅**成功**返回后触发（`is_error != Some(true)`，旧 after_tool 门
        // 逐字）；`spawn` 不阻塞响应，采样在后台收口。
        if matches!(&response, Ok(CallToolResponse::Complete(result)) if result.is_error != Some(true))
        {
            self.spawn_git_sample();
        }
        response
    }
}
