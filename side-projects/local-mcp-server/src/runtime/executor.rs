//! 进程内执行内核：把 MCP 面的 [`ToolRequest`] 直接分派到工具语义实现。
//!
//! 单进程形态（D-003）下没有 worker 进程、没有管道、没有帧编码、没有请求配对、
//! 没有超时兜底，也没有任何形式的通道监督：
//!
//! ```text
//! MCP core → runtime::InProcessExecutor ─┬─ tools::fs::FsRuntime        （五工具）
//!                                        ├─ tools::grep::invoke          （Grep）
//!                                        └─ tasks::TaskRegistry          （Bash：唯一任务表）
//! ```
//!
//! 三条路径都是**同一进程内的普通 Rust 调用**：文件类工具经 capability 根解析
//! （越界与符号链接防护在 capability 层，不在本模块），Bash 经唯一任务注册表，
//! 其进程真身在 [`crate::tasks::bash`]。
//!
//! ## 错误投影（与容器期 broker 侧逐字一致）
//!
//! | 情形 | 结果 |
//! | --- | --- |
//! | 工具名不在七工具内 | [`ToolError::UnknownTool`] |
//! | 参数不是 JSON 对象 | [`ToolError::InvalidRequest`]（源文案） |
//! | 路径越出工作区根 | `Ok(ToolResponse::tool_error(..))`（业务错误，非协议错误） |
//! | 工具语义层报错 | [`ToolError::Internal`]（同容器期"worker 协议级失败"的投影） |
//! | 请求已被取消 | [`ToolError::Internal`]（`Request cancelled.`） |
//!
//! ## 边界
//!
//! 本模块不判定路径授权、不执行工具语义、不持有进程表：它只做分派（宿主路径单表示），
//! 因此每一条语义都能在 `tools/**`、`capability/**` 与 `tasks/**` 的测试里单独验证。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capability::RootDir;
use crate::error::{CapabilityError, ToolError};
use crate::observe::{audit_tool_call, ToolAudit, ToolOutcome};
use crate::output::DEFAULT_ARTIFACT_DIR;
use crate::tasks::log::DirOutputPersist;
use crate::tasks::TaskRegistry;
use crate::tools::bash::BashTool;
use crate::tools::fs::{self, FsCall, FsRuntime};
use crate::tools::grep::{self, GrepContext, SEARCH_TIMEOUT};
use crate::wire::{BoxFuture, ToolExecutor, ToolRequest, ToolResponse};

use super::LOG_SUBDIR;

/// 进程内执行器：唯一的生产 [`ToolExecutor`]。
pub struct InProcessExecutor {
    /// 工作区授权根（宿主工作区根）。文件类工具与 Grep 的路径解析都用它。
    root: Arc<RootDir>,
    /// 五工具 + 截断落盘（capability 判定在 `FsRuntime` 内）。
    fs: Arc<FsRuntime>,
    /// Grep 截断落盘（与 Bash 日志同一私有目录，同样走 capability 校验）。
    grep_persist: Arc<dyn crate::tasks::log::OutputPersist>,
    /// Grep 遍历的兜底超时（生产为源实现的 15s）。
    search_timeout: Duration,
    /// Bash 工具（经唯一任务注册表，不持有进程）。
    bash: BashTool,
}

impl InProcessExecutor {
    /// 以工作区根与任务注册表构造执行器。
    ///
    /// 日志目录取 [`LOG_SUBDIR`]（`<工作区根>/.local-mcp/logs`），必须位于授权根内：
    /// 日志与截断落盘都是本产品自产文件，必须与七工具接受同一套 capability 校验；
    /// 不满足即 fail closed，不降级到无校验写入。
    pub fn new(
        workspace: impl Into<PathBuf>,
        registry: Arc<TaskRegistry>,
    ) -> Result<Self, ToolError> {
        let workspace = workspace.into();
        let log_dir = workspace.join(LOG_SUBDIR);
        let log_subdir = relative_subdir(&workspace, &log_dir)?;
        let fs = FsRuntime::with_artifact_dir(workspace.clone(), DEFAULT_ARTIFACT_DIR).map_err(
            |error| ToolError::Internal {
                message: format!(
                    "cannot open workspace as capability root: {}",
                    error.message()
                ),
            },
        )?;
        // Grep 与五工具使用同一授权根的两个句柄：capability 判定逐组件一致，
        // 但 `RootDir` 持有目录 fd（不能 Clone），因此这里单独打开一次。
        let capability =
            Arc::new(
                RootDir::open(workspace.clone()).map_err(|error| ToolError::Internal {
                    message: format!(
                        "cannot open workspace as capability root: {}",
                        error.message()
                    ),
                })?,
            );
        let grep_persist: Arc<dyn crate::tasks::log::OutputPersist> = Arc::new(
            DirOutputPersist::rooted(Arc::clone(&capability), log_subdir),
        );
        Ok(Self {
            root: capability,
            fs: Arc::new(fs),
            grep_persist,
            search_timeout: SEARCH_TIMEOUT,
            bash: BashTool::new(registry),
        })
    }

    /// 授权根（宿主工作区根）。
    pub fn workspace(&self) -> &Path {
        self.root.base()
    }

    /// 任务注册表（同进程内嵌 API 与测试使用）。
    pub fn registry(&self) -> &Arc<TaskRegistry> {
        self.bash.registry()
    }

    /// 覆盖 Grep 超时（仅测试用；生产保持源实现的 15s）。
    pub fn with_search_timeout(mut self, timeout: Duration) -> Self {
        self.search_timeout = timeout;
        self
    }

    /// 分派一次调用；同时发出审计行（唯一日志出口在 [`crate::observe`]）。
    async fn dispatch(&self, request: ToolRequest) -> Result<ToolResponse, ToolError> {
        let started = Instant::now();
        let audit = |outcome: ToolOutcome, elapsed: std::time::Duration| {
            audit_tool_call(&ToolAudit {
                request_id: &request.context.request_id,
                principal: &request.context.principal,
                client_instance: &request.context.client_instance,
                tool: request.name,
                outcome,
                elapsed,
            });
        };

        let result = match request.name {
            "Read" | "Write" | "Edit" | "Glob" | "folder_operations" => {
                self.dispatch_fs(request.clone())
            }
            "Grep" => self.dispatch_grep(request.clone()).await,
            "Bash" => {
                // `BashTool` 自己完成参数解析、容量、TTL 与结果投影；
                // 它只经注册表访问进程，注册表直接持有任务句柄。
                // 任务日志路径来自根内私有日志目录（`<工作区根>/.local-mcp/logs/…`），
                // 与交付文本同为**宿主路径单表示**，因此逐字外发（D-003 起无需改写）。
                Ok(self.bash.invoke(&request.arguments, &request.context).await)
            }
            other => Err(ToolError::UnknownTool {
                name: other.to_string(),
            }),
        };

        let elapsed = started.elapsed();
        match &result {
            Ok(response) if response.is_error => audit(ToolOutcome::ToolError, elapsed),
            Ok(_) => audit(ToolOutcome::Ok, elapsed),
            Err(_) => audit(ToolOutcome::ProtocolError, elapsed),
        }
        result
    }

    /// 五个文件系统工具：路径翻译 → 同步语义调用 → 交付表示改写。
    fn dispatch_fs(&self, request: ToolRequest) -> Result<ToolResponse, ToolError> {
        let tool = request.name;
        if !fs::is_fs_tool(tool) {
            return Err(ToolError::UnknownTool {
                name: tool.to_string(),
            });
        }
        if !request.arguments.is_object() {
            return Err(ToolError::InvalidRequest {
                message: format!("Tool `{tool}` arguments must be a JSON object."),
            });
        }
        let arguments = request.arguments.clone();
        let path = match self.translate_path(tool, &arguments) {
            Ok(path) => path,
            Err(response) => return Ok(*response),
        };
        let call = FsCall {
            tool: tool.to_string(),
            arguments,
            path,
        };
        // 语义层的协议级失败按容器期 broker 的同一投影返回内部错误（fail closed）。
        let mut response = self.fs.call(&call).map_err(|error| ToolError::Internal {
            message: error.message(),
        })?;
        crate::tools::enforce_delivery_budget(&self.root, &mut response);
        Ok(response)
    }

    /// `Grep`：路径解析 → capability 解析 → 遍历/截断 → 交付字节预算复核。
    async fn dispatch_grep(&self, request: ToolRequest) -> Result<ToolResponse, ToolError> {
        if request.name != "Grep" {
            return Err(ToolError::UnknownTool {
                name: request.name.to_string(),
            });
        }
        if !request.arguments.is_object() {
            return Err(ToolError::InvalidRequest {
                message: "Tool `Grep` arguments must be a JSON object.".to_string(),
            });
        }
        let mut arguments = request.arguments.clone();
        let path = match self.translate_grep_path(&arguments) {
            Ok(path) => path,
            Err(response) => return Ok(*response),
        };
        // 路由层（本模块）已经把 `path` 解析成授权根内路径；Grep 的参数解析只认
        // `path` 字段，因此这里把它写回 arguments（缺失即工作区根，与源默认一致）。
        if let Some(path) = path {
            if let Some(object) = arguments.as_object_mut() {
                object.insert("path".to_string(), Value::String(path));
            }
        }
        let requested = arguments.get("path").and_then(Value::as_str);
        // 三种写法（缺省 / `path="."` / 显式根路径）走**同一**解析与判定：缺省在
        // `resolve_search_path` 内归一为 `.`，不在调用点分叉出第二条路径。
        let resolved = resolve_search_path(&self.root, requested);
        let cwd = self.root.base().to_path_buf();
        let root_real = self.root.real_base().to_path_buf();
        let persist: Arc<dyn crate::tasks::log::OutputPersist> = Arc::clone(&self.grep_persist);
        let timeout = self.search_timeout;
        let cancel = request.context.cancellation.clone();

        let mut response = match resolved {
            Ok(resolved) => {
                let resolved = match resolved {
                    SearchRoot::Inside(real) => real,
                    // 目标不存在：把路径原样交给 Grep 的源文案，遍历前仍会被
                    // `execute_search` 的 canonical 复核拦下（fail closed）。
                    SearchRoot::Missing(path) => path,
                };
                let resolver = move |_requested: &str| -> Result<PathBuf, CapabilityError> {
                    // capability 已在上面的同步阶段判定；这里只回放同一结果（克隆而非移动）。
                    Ok(resolved.clone())
                };
                let context =
                    GrepContext::new(cwd, root_real, &resolver, persist).with_timeout(timeout);
                grep::invoke(&arguments, &context, Some(&cancel)).await
            }
            Err(error) => {
                // 逃逸的搜索根（符号链接）是工具业务错误，与"不存在"同族。
                return Ok(ToolResponse::tool_error(
                    format!("Error: {}", error.public_message()),
                    crate::wire::StructuredOutput::error("Grep"),
                ));
            }
        };
        // 交付文本按宿主路径单表示逐字外发（Grep 的展示路径相对搜索根，落盘路径是宿主绝对
        // 路径），这里只复核 20000 字节交付预算。
        crate::tools::enforce_delivery_budget(&self.root, &mut response);
        Ok(response)
    }

    /// 解析五种 FS 工具的路径参数：越界是工具业务错误，缺失/类型错误交给语义层。
    ///
    /// 返回 `Err(ToolResponse)` 表示「授权根外路径」这类**工具业务错误**——语义层
    /// 不应被打扰，调用方直接拿到该结果。
    fn translate_path(
        &self,
        tool: &str,
        arguments: &Value,
    ) -> Result<Option<String>, Box<ToolResponse>> {
        let Some(field) = fs::path_argument(tool) else {
            return Ok(None);
        };
        let raw = match arguments.get(field) {
            Some(Value::String(raw)) => raw.clone(),
            // Glob 的 `path` 可选：缺省即工作区根（源实现默认 cwd）。
            None | Some(Value::Null) if field == "path" => {
                return Ok(Some(self.host_root_string()))
            }
            // 缺失或类型错误：交给语义层产出源实现的必需参数文案。
            _ => return Ok(None),
        };
        match self.root.resolve_host_path(&raw) {
            Ok(path) => Ok(Some(path.to_string_lossy().to_string())),
            Err(error) => Err(Box::new(denied_response(tool, &raw, &error))),
        }
    }

    /// `Grep` 的 `path` 只有三种形状：缺省（工作区根）、字符串（解析）、
    /// 类型错误（原实现用空串占位，交给 Grep 的参数校验产出源文案）。
    fn translate_grep_path(&self, arguments: &Value) -> Result<Option<String>, Box<ToolResponse>> {
        let raw = match arguments.get("path") {
            None | Some(Value::Null) => return Ok(Some(self.host_root_string())),
            Some(Value::String(raw)) => raw.clone(),
            // 类型错误交给 Grep 的参数校验产出源实现的文案。
            _ => return Ok(Some(String::new())),
        };
        match self.root.resolve_host_path(&raw) {
            Ok(path) => Ok(Some(path.to_string_lossy().to_string())),
            Err(error) => Err(Box::new(denied_response("Grep", &raw, &error))),
        }
    }

    /// 工作区根的宿主绝对路径（Glob/Grep 的 `path` 缺省值）。
    fn host_root_string(&self) -> String {
        self.root.base().to_string_lossy().to_string()
    }
}

/// 授权根外路径的工具业务错误（`denied=outside_workspace`，结构化字段可复核）。
fn denied_response(tool: &str, requested: &str, error: &CapabilityError) -> ToolResponse {
    ToolResponse::tool_error(
        format!("Error: {}", error.public_message()),
        crate::wire::StructuredOutput::error(tool)
            .with_extra("path", serde_json::json!(requested))
            .with_extra("denied", serde_json::json!("outside_workspace")),
    )
}

impl ToolExecutor for InProcessExecutor {
    fn execute<'a>(
        &'a self,
        request: ToolRequest,
    ) -> BoxFuture<'a, Result<ToolResponse, ToolError>> {
        Box::pin(async move {
            // 已取消的请求不再进入语义层：这是进程内唯一的"取消检查点"
            // （同步工具调用没有可中断的等待点，`Bash` 的真实停止走注册表）。
            if request.context.cancellation.is_cancelled() {
                return Err(ToolError::Internal {
                    message: "Request cancelled.".to_string(),
                });
            }
            self.dispatch(request).await
        })
    }
}

/// 计算日志目录相对授权根的子路径（本产品自产文件必须在授权根内）。
///
/// 只接受**严格位于根内**的子目录（相等或越界都拒绝）：生产上日志目录恒为
/// `<workspace>/.local-mcp/logs`，一旦有人把它指到根外，说明接线被改坏，
/// 此时 fail closed 比"悄悄按无校验路径写"更安全。
fn relative_subdir(workspace: &Path, log_dir: &Path) -> Result<String, ToolError> {
    let reject = || ToolError::Internal {
        message: format!(
            "log dir {} must be a subdirectory of the capability root {}",
            log_dir.display(),
            workspace.display()
        ),
    };
    let relative = log_dir.strip_prefix(workspace).map_err(|_| reject())?;
    let mut components: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                components.push(part.to_string_lossy().to_string())
            }
            // 空路径（== 根）、`..`、绝对路径分量一律拒绝。
            _ => return Err(reject()),
        }
    }
    if components.is_empty() {
        return Err(reject());
    }
    Ok(components.join("/"))
}

/// Grep 搜索根的解析结果。
///
/// 两种结果都**不再**是"用来判定的那个词法字符串"：生产路径上交给遍历器的是
/// [`SearchRoot::Inside`] 的 canonical 真实路径（GAP-034 的修复判据之一）。
enum SearchRoot {
    /// 目标存在，且真实路径位于**启动时锚定**的授权根内：这一份真实路径就是遍历对象。
    Inside(PathBuf),
    /// 目标不存在：只用于产出源文案（"Search path does not exist"），不会被遍历
    /// （`execute_search` 会先 canonicalize，失败即 fail closed）。
    Missing(PathBuf),
}

/// 把 Grep 的 `path` 参数（缺省归一为 `.`）解析为**根内**宿主绝对路径。
///
/// 判定分两步（与 `FsRuntime::call` 同源，另加真实路径复核）：
/// 1. 词法：调用方路径按 [`RootDir::resolve_host_path`] 归一并判定授权根（相对路径相对根，
///    绝对路径必须逐组件以根为前缀，前缀碰撞拒绝）；
/// 2. 真实路径：`canonicalize` 后的搜索根必须仍在**启动时锚定的根真实路径**
///    （[`RootDir::real_base`]）内，且返回的是该真实路径——遍历器拿到的与判定的是同一实体。
///    基准**禁止**每次请求重新 canonicalize：根路径被换成指向根外的符号链接时，
///    "当前根"与搜索根会指向同一个逃逸目标，包含判定恒真（GAP-034 构造 1）。
///
/// 缺省 `path`、`path="."`、显式根路径与其它相对/绝对写法都走本函数这一条路径。
fn resolve_search_path(
    root: &RootDir,
    requested: Option<&str>,
) -> Result<SearchRoot, CapabilityError> {
    let requested = requested.unwrap_or(".");
    let joined = root.resolve_host_path(requested)?;
    let anchored = root.real_base();
    match std::fs::canonicalize(&joined) {
        Ok(real) => {
            if real == anchored || real.starts_with(anchored) {
                Ok(SearchRoot::Inside(real))
            } else {
                Err(CapabilityError::OutsideRoot {
                    requested: requested.to_string(),
                    root: root.base().to_path_buf(),
                })
            }
        }
        // 目标不存在：交给 Grep 的源文案（"Search path does not exist"）。
        Err(_) => Ok(SearchRoot::Missing(joined)),
    }
}
