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
//! The handler does not override `discover`; besides the seven tools it exposes the git ref
//! resource (`workspace://git/ref`) and the 2026-07-28 subscription surface
//! (`accepted_subscription_filter` + `listen`) that carries git ref changes to the host.
//! Tool failures use shared allowlisted recovery text; request cancellation drops the tool
//! future.
//!
//! **W1 resource surface（2026-09-29）**：经 [`Self::with_resources`] 装配时，handler
//! 合并 `resources` provider 的公开批（skills / agents / instructions）与
//! `skills/list|get` custom requests；未装配时这些方法分别表现为
//! 「未知资源」（`-32602`）与「方法不支持」（`-32601`）——**不**把缺输入伪装成
//! 「技能不存在且已 ready」。生产装配点（`peri-middlewares` dispatch 的 `workspace`
//! arm）自 W4a（2026-09-29）起对**会话装配**调用 `with_resources`，当前只装载
//! meta 面（J6；skill / agent 根与真实关闭位属 W4b）；顶层三路径不调用（资源面未接线）。
//! 宿主侧投递（registry/消费端切换）除外。

use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::Engine as _;
use peri_agent::tools::BaseTool;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CustomRequest, CustomResult, ErrorCode,
        ExtensionCapabilities, Implementation, JsonObject, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, RequestId, Resource, ResourceContents,
        ResourceTemplate, ServerCapabilities, ServerInfo, SubscriptionFilter,
    },
    service::{RequestContext, RoleServer, SubscriptionContext, SubscriptionSink},
    ErrorData as McpError, ServerHandler,
};

use peri_mcp_common::{invoke_tool_call, list_tools_of};

use crate::filesystem::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};
use crate::git_watch::{run_git_sample, GitWatchState, SampleOutcome, GIT_REF_RESOURCE_URI};
use crate::resources::{
    ResourceBody, ResourceError, WorkspaceResourceProvider, WorkspaceResourcesInput,
};
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
    /// W1 资源 provider（`with_resources` 装配；`None` = 资源面未接线）。
    ///
    /// `None` 是**未支持**而不是「空目录集」：`skills/*` 返回 `-32601`、
    /// 新 scheme 的 `resources/read` 返回 `-32602`（见模块头）。
    resources: Option<Arc<WorkspaceResourceProvider>>,
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
            resources: None,
        }
    }

    /// 装配 W1 资源面（独立于 `WorkspaceInstanceInput` 的 session 级 Bash 输入）。
    ///
    /// 输入由宿主装配构造（根列表 / plugin 标签 / `disable_bundled` 关闭位 / 预算）；
    /// 不调用本方法时资源面保持未接线（模块头语义）。生产装配点自 W4a 起对会话装配
    /// 调用它（经 `BuiltinInstanceContext::workspace_resources`，见 `peri-middlewares`
    /// dispatch 的 `workspace` arm）。
    pub fn with_resources(mut self, input: WorkspaceResourcesInput) -> Self {
        self.resources = Some(Arc::new(WorkspaceResourceProvider::new(
            self.cwd.clone(),
            input,
        )));
        self
    }

    /// 资源 provider 句柄（`spawn_blocking` 需要 move 进阻塞线程）。
    fn resource_provider(&self) -> Option<Arc<WorkspaceResourceProvider>> {
        self.resources.clone()
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
    ///
    /// **W4b（plan §5.1）：技能能力位与资源面同生共死。** 资源 provider 装配时在
    /// 本地组合 `resources` + SEP-2640 skills extension（`skills/list` / `skills/get`
    /// custom requests 由同一 provider 提供）；未装配 provider 的实例（`with_resources`
    /// 未被调用，例如顶层三路径 / 未接线测试）**不声明**该扩展——「声明即实现」，
    /// 客户端不会对未支持的方法盲调，也不会把「未支持」误认成「空技能集」
    /// （`peri-acp-types::skills::SKILLS_EXTENSION_ID` 是两侧共用的键）。
    fn get_info(&self) -> ServerInfo {
        let mut caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .build();
        if self.resources.is_some() {
            let mut extensions = ExtensionCapabilities::new();
            extensions.insert(
                peri_acp_types::skills::SKILLS_EXTENSION_ID.to_string(),
                JsonObject::new(),
            );
            // 未装配 provider 时不写 `extensions` 字段（`Some(空表)` 会序列化成
            // `"extensions": {}`，与「不声明」在语义上不同，但多一份噪声）。
            caps.extensions = Some(extensions);
        }
        ServerInfo::new(caps).with_server_info(Implementation::new(
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

    /// 资源发现批：git ref 单条（恒在） + 资源 provider 的公开批（装配时）。
    ///
    /// provider 的扫描在 `spawn_blocking` 中执行（RUST-ASYNC-001：扫描/读取是阻塞
    /// I/O，不直接堵塞 async runtime）。未装配时行为与本波之前一致（仅 git ref）。
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut items =
            vec![Resource::new(GIT_REF_RESOURCE_URI, GIT_REF_RESOURCE_NAME)
                .with_mime_type("text/plain")];
        if let Some(provider) = self.resource_provider() {
            let scanned = tokio::task::spawn_blocking(move || provider.list_resources())
                .await
                .map_err(|_| McpError::internal_error("resource scan failed", None))?;
            items.extend(scanned);
        }
        Ok(ListResourcesResult::with_all_items(items))
    }

    /// 资源模板（skills / agents 的可读 URI 形状；只描述范围，不构成读取授权）。
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let templates: Vec<ResourceTemplate> = match self.resource_provider() {
            Some(provider) => {
                tokio::task::spawn_blocking(move || provider.list_resource_templates())
                    .await
                    .map_err(|_| McpError::internal_error("resource scan failed", None))?
            }
            None => Vec::new(),
        };
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    /// 读取资源正文：
    /// - git ref：既有单条路径（未命中仍按协议回 `-32602`）；
    /// - 其余 URI：资源 provider 按 scheme 分派（未装配 = 未知资源 `-32602`）。
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri == GIT_REF_RESOURCE_URI {
            return Ok(ReadResourceResponse::Complete(
                rmcp::model::ReadResourceResult::new(vec![ResourceContents::text(
                    self.git.resource_text(),
                    GIT_REF_RESOURCE_URI,
                )]),
            ));
        }
        let Some(provider) = self.resource_provider() else {
            return Err(McpError::invalid_params(
                format!("unknown resource: {}", request.uri),
                None,
            ));
        };
        let uri = request.uri.clone();
        let payload = tokio::task::spawn_blocking(move || provider.read(&uri))
            .await
            .map_err(|_| McpError::internal_error("resource read failed", None))?
            .map_err(map_resource_read_error)?;
        let contents = project_resource(payload);
        Ok(ReadResourceResponse::Complete(
            rmcp::model::ReadResourceResult::new(vec![contents]),
        ))
    }

    /// `skills/list` / `skills/get`（MCPP custom requests；未装配 → `-32601`）。
    ///
    /// 未知 method 与未装配都按 method_not_found 收口：这是「该实例不提供该方法」
    /// 的诚实表达，与「技能不存在」（`-32602`）区分。
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        let method = request.method.clone();
        let supported = matches!(method.as_str(), "skills/list" | "skills/get");
        let provider = if supported {
            self.resource_provider()
        } else {
            None
        };
        let Some(provider) = provider else {
            return Err(McpError::new(ErrorCode::METHOD_NOT_FOUND, method, None));
        };

        match method.as_str() {
            "skills/list" => {
                let cursor = request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("cursor"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let response = tokio::task::spawn_blocking(move || provider.skills_list(cursor))
                    .await
                    .map_err(|_| McpError::internal_error("skill scan failed", None))?
                    .map_err(map_skills_error)?;
                let value = serde_json::to_value(&response).map_err(|_| {
                    McpError::internal_error("skill response encoding failed", None)
                })?;
                Ok(CustomResult::new(value))
            }
            _ => {
                let uri = request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("uri"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        McpError::invalid_params("skills/get requires a uri parameter", None)
                    })?;
                let response = tokio::task::spawn_blocking(move || provider.skills_get(&uri))
                    .await
                    .map_err(|_| McpError::internal_error("skill scan failed", None))?
                    .map_err(map_skills_error)?;
                let value = serde_json::to_value(&response).map_err(|_| {
                    McpError::internal_error("skill response encoding failed", None)
                })?;
                Ok(CustomResult::new(value))
            }
        }
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

// ─── 资源面投影（W1）─────────────────────────────────────────────────────────

/// `resources/read` 的错误映射（新资源面；文案不含绝对路径）。
///
/// - `InvalidUri` / `Denied`：URI 非法 / 未知 scheme / 越界 → `-32602`（未知资源）；
/// - `NotFound`：合法 URI 但目标不存在或未公开 → `-32002`（MCP core 的
///   resource not found）；
/// - `Budget` / `Io`：服务器侧拒绝或失败 → `-32603`。
fn map_resource_read_error(error: ResourceError) -> McpError {
    match error {
        ResourceError::InvalidUri | ResourceError::Denied => {
            McpError::invalid_params("unknown resource", None)
        }
        ResourceError::NotFound => McpError::resource_not_found("resource not found", None),
        ResourceError::Budget | ResourceError::Io => {
            McpError::internal_error("resource unavailable", None)
        }
    }
}

/// `skills/*` 的错误映射（MCPP 约定：未知 `skills/get` URI 一律 `-32602`）。
fn map_skills_error(error: ResourceError) -> McpError {
    match error {
        ResourceError::InvalidUri | ResourceError::NotFound | ResourceError::Denied => {
            McpError::invalid_params("unknown skill", None)
        }
        ResourceError::Budget | ResourceError::Io => {
            McpError::internal_error("skill unavailable", None)
        }
    }
}

/// 读取产物 → `ResourceContents`（text/blob + mime + `_meta`）。
///
/// `_meta` 的 digest 以 payload 的 `digest` 覆盖写（两者同源；即使 provider 内部
/// 组装与投影之间被改动，投影面仍以本次读取的 digest 为准）。
fn project_resource(payload: crate::resources::ResourcePayload) -> ResourceContents {
    use peri_acp_types::workspace_resources::META_KEY_DIGEST;
    let crate::resources::ResourcePayload {
        uri,
        mime,
        digest,
        meta,
        body,
    } = payload;
    let mut meta_map = meta.0;
    meta_map.insert(
        META_KEY_DIGEST.to_string(),
        serde_json::Value::String(digest),
    );
    let mut contents = match body {
        ResourceBody::Text(text) => ResourceContents::text(text, uri).with_mime_type(mime),
        ResourceBody::Blob(bytes) => {
            ResourceContents::blob(base64::engine::general_purpose::STANDARD.encode(bytes), uri)
                .with_mime_type(mime)
        }
    };
    let projected_meta = MetaObject(meta_map);
    match &mut contents {
        ResourceContents::TextResourceContents { meta: slot, .. } => *slot = Some(projected_meta),
        ResourceContents::BlobResourceContents { meta: slot, .. } => *slot = Some(projected_meta),
        // `ResourceContents` 是 non_exhaustive：未来新增变体时保持编译（不投影 meta）。
        _ => {}
    }
    contents
}
