//! MCP surface: tools, SEP-2640 skills and skill resources.
//!
//! Tool names and semantics follow README §7. Failures are tool results with
//! `isError` (never protocol errors), so the model can read the stable code and
//! the hint. The only protocol errors are an unknown tool name and an unknown
//! skill/resource URI.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, DiscoverRequestMethod,
    DiscoverResult, ErrorData as McpError, GetSkillRequestParams, GetSkillResult, Implementation,
    InitializeRequestParams, InitializeResult, ListResourcesResult, ListSkillsResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::core::BrowserCore;
use crate::error::BrowserError;
use crate::mcp_http::{BoundBrowserSession, Initializing};

/// Server identity advertised in `initialize`.
pub const SERVER_NAME: &str = "peri-browser-mcp";
/// Hand-off the model performs after `request_authorization` returns.
const AUTHORIZATION_INSTRUCTION: &str =
    "Call AskUserQuestion with this URL so the user can open it; continue only after the user answers.";

/// 浏览器状态只能来自请求的 Live 许可，不从 SDK factory 创建。
#[derive(Clone)]
pub(crate) struct BrowserMcpServer;

impl BrowserMcpServer {
    /// The README §7 tool surface.
    #[must_use]
    pub fn tools() -> Vec<Tool> {
        vec![
            tool(
                "browser_status",
                "Report connection, install and endpoint state plus this session's pages and monitor. Call it first when browser tools fail: while disconnected, follow nextSteps to install kitesurf, start it and connect.",
                json!({"type": "object", "properties": {}}),
            ),
            tool(
                "browser_connect",
                "Attach this session to a running Kitesurf instance. Required before any page tool; browser_status is the only state source. Accepts http:// or ws:// loopback URLs with an explicit port and fixes the endpoint for the MCP session lifetime.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "running Kitesurf endpoint, for example http://127.0.0.1:18971 or ws://127.0.0.1:18971/devtools/browser"}
                    },
                    "required": ["url"]
                }),
            ),
            tool(
                "list_pages",
                "List the page targets this session owns, with url, title, active flag and whether the controller is currently yielded to a human.",
                json!({"type": "object", "properties": {}}),
            ),
            tool(
                "select_page",
                "Switch the active page among the targets this session already owns. Does not change page ownership.",
                json!({
                    "type": "object",
                    "properties": {"pageId": {"type": "string", "description": "id from list_pages"}},
                    "required": ["pageId"]
                }),
            ),
            tool(
                "new_page",
                "Create a new page target through the shared browser endpoint and make it active. The first browser tool call in a session creates a page automatically.",
                json!({
                    "type": "object",
                    "properties": {"url": {"type": "string", "description": "optional URL to load; bare hosts get https://"}}
                }),
            ),
            tool(
                "close_page",
                "Close a page target (the active page by default) through the browser endpoint and forget it. The closed page cannot be recovered.",
                page_schema(),
            ),
            tool(
                "navigate_page",
                "Navigate a page and wait for document.readyState to reach complete (bounded by PERI_BROWSER_PAGE_WAIT_MS). Invalidates uids from earlier snapshots.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "absolute URL or bare host"},
                        "pageId": {"type": "string"}
                    },
                    "required": ["url"]
                }),
            ),
            tool(
                "take_snapshot",
                "Return a text accessibility snapshot of the page. Each actionable node has a uid; click/fill/hover accept only uids from the latest snapshot. Take a fresh snapshot after navigation or when a uid is rejected as stale.",
                page_schema(),
            ),
            tool(
                "take_screenshot",
                "Capture the viewport as a JPEG image (bounded to 1600x1000). Also returns structured metadata; the image is the primary content.",
                page_schema(),
            ),
            tool(
                "evaluate_script",
                "Evaluate a JavaScript expression in the page and return its value. Promises are awaited and values are returned by value. A thrown exception becomes a script_error tool error.",
                json!({
                    "type": "object",
                    "properties": {
                        "expression": {"type": "string"},
                        "pageId": {"type": "string"}
                    },
                    "required": ["expression"]
                }),
            ),
            tool(
                "click",
                "Click the center of an element identified by a snapshot uid or a CSS selector. Waits for it to be visible and scrolls it into view first.",
                json!({
                    "type": "object",
                    "properties": {
                        "uid": {"type": "string", "description": "uid from the latest take_snapshot"},
                        "selector": {"type": "string", "description": "CSS selector, used when uid is absent"},
                        "pageId": {"type": "string"}
                    }
                }),
            ),
            tool(
                "fill",
                "Focus a field, replace its contents with text and fire input/change events. Identified by a snapshot uid or a CSS selector.",
                json!({
                    "type": "object",
                    "properties": {
                        "uid": {"type": "string"},
                        "selector": {"type": "string"},
                        "text": {"type": "string"},
                        "pageId": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            ),
            tool(
                "hover",
                "Move the pointer over the center of an element identified by a snapshot uid or a CSS selector.",
                json!({
                    "type": "object",
                    "properties": {
                        "uid": {"type": "string"},
                        "selector": {"type": "string"},
                        "pageId": {"type": "string"}
                    }
                }),
            ),
            tool(
                "press_key",
                "Press a key or chord on the focused element. Examples: Enter, Tab, Escape, ArrowDown, Control+A, Shift+Tab, F5.",
                json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string"},
                        "pageId": {"type": "string"}
                    },
                    "required": ["key"]
                }),
            ),
            tool(
                "wait_for",
                "Wait until body text contains a string or a JS predicate is truthy (or sleep when neither is given). Returns matched:false when the timeout expires; it is not an error.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "predicate": {"type": "string", "description": "JavaScript expression evaluated as a boolean"},
                        "timeoutMs": {"type": "integer", "minimum": 0, "maximum": 60000},
                        "pageId": {"type": "string"}
                    }
                }),
            ),
            tool(
                "list_console_messages",
                "Return console messages and uncaught exceptions buffered for the page (newest 200). Runtime is enabled lazily on first use, so earlier messages are not replayed.",
                page_schema(),
            ),
            tool(
                "list_network_requests",
                "Return buffered network requests (newest 200) with method, url, status and timing. Optionally filter by URL substring. Network is enabled lazily on first use.",
                json!({
                    "type": "object",
                    "properties": {
                        "filter": {"type": "string", "description": "URL substring"},
                        "pageId": {"type": "string"}
                    }
                }),
            ),
            tool(
                "get_network_request",
                "Return one buffered request plus its response body (bounded to 64 KiB). Use the requestId from list_network_requests.",
                json!({
                    "type": "object",
                    "properties": {
                        "requestId": {"type": "string"},
                        "pageId": {"type": "string"}
                    },
                    "required": ["requestId"]
                }),
            ),
            tool(
                "yield_page",
                "Hand the active page's controller to the monitor on purpose and wait up to PERI_BROWSER_YIELD_TIMEOUT_MS for it to come back. Returns reclaimed:false (status yield_pending) when a human still holds it; the page stays yielded.",
                json!({"type": "object", "properties": {}}),
            ),
            tool(
                "request_human",
                "Ask a human to act in the monitor (log in, solve a captcha, confirm). Blocks until the human finishes, declines, or the timeout expires; returns done with url/title/screenshot, declined, or no_human. Other page tools return waiting_for_human meanwhile.",
                json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "description": "what the human should do"},
                        "timeoutMs": {"type": "integer", "minimum": 0}
                    },
                    "required": ["prompt"]
                }),
            ),
            tool(
                "request_authorization",
                "Return a URL the user must open to authorize or consent. The server validates the URL, returns it, and does not navigate; it works without a browser connection. Pass the URL to AskUserQuestion so the user sees it, and continue only after the user answers.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "http(s) URL the user should open"},
                        "reason": {"type": "string", "description": "why the authorization is needed"}
                    },
                    "required": ["url"]
                }),
            ),
        ]
    }

    async fn dispatch(
        &self,
        core: &Arc<BrowserCore>,
        request: &CallToolRequestParams,
    ) -> Result<CallToolResponse, McpError> {
        // 整个工具调用共用一份写入许可，含 UID 解析到最后一条 CDP 命令。
        macro_rules! page {
            ($id:expr, $operation:ident => $body:expr) => {{
                async {
                    let page = core.tool_page($id).await?;
                    let $operation = core.agent_operation(&page).await?;
                    $body
                }
                .await
            }};
        }
        let value = match request.name.as_ref() {
            "browser_status" => Ok(core.status().await),
            "browser_connect" => core.connect(&arguments::<ConnectArgs>(request)?.url).await,
            "list_pages" => Ok(core.list_pages().await),
            "select_page" => {
                core.select_page(&arguments::<SelectPageArgs>(request)?.page_id)
                    .await
            }
            "new_page" => {
                core.new_page(arguments::<NewPageArgs>(request)?.url.as_deref())
                    .await
            }
            "close_page" => {
                core.close_page(arguments::<PageArgs>(request)?.page_id.as_deref())
                    .await
            }
            "navigate_page" => {
                let args: NavigateArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.navigate_page(&args.url).await)
            }
            "take_snapshot" => {
                let args: PageArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.take_snapshot().await)
            }
            "take_screenshot" => {
                let args: PageArgs = arguments(request)?;
                let shot = page!(args.page_id.as_deref(), op => op.take_screenshot().await.map(|shot| (op.id.clone(), shot)));
                return Ok(match shot {
                    Ok((page_id, shot)) => {
                        let mut result = CallToolResult::structured(
                            json!({"pageId": page_id, "mimeType": shot.mime_type}),
                        );
                        result
                            .content
                            .push(ContentBlock::image(shot.data, shot.mime_type));
                        CallToolResponse::Complete(result)
                    }
                    Err(error) => failure(error),
                });
            }
            "evaluate_script" => {
                let args: EvaluateArgs = arguments(request)?;
                if args.expression.trim().is_empty() {
                    return Ok(failure(BrowserError::InvalidArgument {
                        name: "expression".to_owned(),
                        reason: "must not be empty".to_owned(),
                    }));
                }
                page!(args.page_id.as_deref(), op => op.evaluate_script(&args.expression).await.map(|value| json!({"result": value, "pageId": op.id})))
            }
            "click" => {
                let args: ElementArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.click(args.uid.as_deref(), args.selector.as_deref()).await)
            }
            "fill" => {
                let args: FillArgs = arguments(request)?;
                page!(args.element.page_id.as_deref(), op => op.fill(args.element.uid.as_deref(), args.element.selector.as_deref(), &args.text).await)
            }
            "hover" => {
                let args: ElementArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.hover(args.uid.as_deref(), args.selector.as_deref()).await)
            }
            "press_key" => {
                let args: PressKeyArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.press_key(&args.key).await)
            }
            "wait_for" => {
                let args: WaitArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.wait_for(args.text.as_deref(), args.predicate.as_deref(), args.timeout_ms).await)
            }
            "list_console_messages" => {
                let args: PageArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.list_console().await)
            }
            "list_network_requests" => {
                let args: NetworkListArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.list_network(args.filter.as_deref()).await)
            }
            "get_network_request" => {
                let args: NetworkRequestArgs = arguments(request)?;
                page!(args.page_id.as_deref(), op => op.get_network_request(&args.request_id).await)
            }
            "yield_page" => core.yield_page().await,
            "request_human" => {
                let args: RequestHumanArgs = arguments(request)?;
                if args.prompt.trim().is_empty() {
                    return Ok(failure(BrowserError::InvalidArgument {
                        name: "prompt".to_owned(),
                        reason: "must not be empty".to_owned(),
                    }));
                }
                core.request_human(&args.prompt, args.timeout_ms).await
            }
            "request_authorization" => return Ok(self.request_authorization(&arguments(request)?)),
            _ => return Err(McpError::invalid_params("unknown tool", None)),
        };
        Ok(value.map_or_else(failure, success))
    }

    /// Validates the URL and hands it back with the AskUserQuestion
    /// instruction. The server never opens the URL, never navigates and never
    /// depends on the browser connection; the user's answer is the model's to
    /// collect from its own question tool.
    fn request_authorization(&self, args: &AuthorizationArgs) -> CallToolResponse {
        let parsed = match Url::parse(args.url.trim()) {
            Ok(parsed) => parsed,
            Err(error) => {
                return failure(BrowserError::InvalidArgument {
                    name: "url".to_owned(),
                    reason: error.to_string(),
                })
            }
        };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return failure(BrowserError::InvalidArgument {
                name: "url".to_owned(),
                reason: "must be an absolute http(s) URL with a host".to_owned(),
            });
        }
        let message = match args
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        {
            Some(reason) => reason.to_owned(),
            None => "The user must open this URL to authorize the browser session".to_owned(),
        };
        let url = parsed.as_str().to_owned();
        let mut result = CallToolResult::structured(json!({
            "status": "user_confirmation_required",
            "url": url,
            "message": message,
            "instruction": AUTHORIZATION_INSTRUCTION,
        }));
        result.content.push(ContentBlock::text(format!(
            "Authorization URL (not opened, not navigated): {url}\n{message}\n{AUTHORIZATION_INSTRUCTION}"
        )));
        CallToolResponse::Complete(result)
    }
}

impl ServerHandler for BrowserMcpServer {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        if context.extensions.get::<Initializing>().is_none() {
            return Err(McpError::invalid_request(
                "initialize requires a new session",
                None,
            ));
        }
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, McpError> {
        Err(McpError::method_not_found::<DiscoverRequestMethod>())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tools().into_iter().find(|tool| tool.name == name)
    }

    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_skills()
            .build();
        ServerConfig::new(capabilities)
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
            .with_server_info(Implementation::new(SERVER_NAME, crate::version()))
            .with_instructions(
                "Browser control through a Kitesurf instance. Start with browser_status; if it \
                 reports connected: false, follow nextSteps and call browser_connect before any \
                 page tool. Use take_snapshot before uid-based interaction, and take a new \
                 snapshot after navigation or a writer change. Use list_pages to find opaque \
                 page IDs. If a person holds the writer, wait for them to release it before \
                 retrying. Skills: skill://peri-browser/browser-automation/SKILL.md and \
                 skill://peri-browser/debugging/SKILL.md.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let _permit = admit(&context)?;
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let (core, _permit) = admit(&context)?;
        let budget = match request.name.as_ref() {
            "browser_status" | "browser_connect" => {
                core.config.connect_timeout + std::time::Duration::from_secs(1)
            }
            "yield_page" | "request_human" => {
                // 这两个操作内部使用同一个绝对时限，并返回协议规定的业务结果。
                tokio::select! {
                    biased;
                    _ = core.stop.cancelled() => return Ok(failure(BrowserError::Closing)),
                    _ = context.ct.cancelled() => return Err(McpError::invalid_request("request cancelled", None)),
                    result = self.dispatch(&core, &request) => return result,
                }
            }
            "new_page" | "close_page" => core.config.structural_budget(),
            _ => core.config.page_budget(),
        };
        tokio::select! {
            biased;
            _ = core.stop.cancelled() => Ok(failure(BrowserError::Closing)),
            _ = context.ct.cancelled() => Err(McpError::invalid_request("request cancelled", None)),
            result = tokio::time::timeout(budget, self.dispatch(&core, &request)) =>
                result.unwrap_or_else(|_| Ok(failure(BrowserError::Timeout("browser tool".to_owned())))),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let _permit = admit(&context)?;
        let resources = crate::skills::resources()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let _permit = admit(&context)?;
        let raw = crate::skills::read(&request.uri)
            .map_err(|_| McpError::resource_not_found("no such resource", None))?;
        let contents = raw
            .content()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![contents],
        )))
    }

    async fn list_skills(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        let _permit = admit(&context)?;
        let skills = crate::skills::catalog()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ListSkillsResult::with_all_items(skills))
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        let _permit = admit(&context)?;
        let skill = crate::skills::skill(&request.uri)
            .map_err(|_| McpError::invalid_params("unknown skill uri", None))?;
        Ok(GetSkillResult::new(skill))
    }
}

fn admit(
    context: &RequestContext<RoleServer>,
) -> Result<
    (
        Arc<BrowserCore>,
        tokio_util::task::task_tracker::TaskTrackerToken,
    ),
    McpError,
> {
    context
        .extensions
        .get::<BoundBrowserSession>()
        .and_then(|bound| bound.0.admit())
        .filter(|_| !context.ct.is_cancelled())
        .ok_or_else(|| McpError::invalid_request("a live browser session is required", None))
}

// -- argument types ---------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectArgs {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageArgs {
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectPageArgs {
    page_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewPageArgs {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NavigateArgs {
    url: String,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvaluateArgs {
    expression: String,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ElementArgs {
    uid: Option<String>,
    selector: Option<String>,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FillArgs {
    #[serde(flatten)]
    element: ElementArgs,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PressKeyArgs {
    key: String,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitArgs {
    text: Option<String>,
    predicate: Option<String>,
    timeout_ms: Option<u64>,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkListArgs {
    filter: Option<String>,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkRequestArgs {
    request_id: String,
    page_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestHumanArgs {
    prompt: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationArgs {
    url: String,
    reason: Option<String>,
}

fn arguments<T: for<'de> Deserialize<'de>>(request: &CallToolRequestParams) -> Result<T, McpError> {
    let value = Value::Object(request.arguments.clone().unwrap_or_default());
    serde_json::from_value(value).map_err(|error| {
        McpError::invalid_params(
            format!("invalid arguments for {}: {error}", request.name),
            None,
        )
    })
}

fn page_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"pageId": {"type": "string", "description": "active page when omitted"}}
    })
}

fn tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let object = schema.as_object().cloned().unwrap_or_default();
    Tool::new(name, description, object)
}

fn success(value: Value) -> CallToolResponse {
    CallToolResponse::Complete(CallToolResult::structured(value))
}

fn failure(error: BrowserError) -> CallToolResponse {
    let payload = json!({
        "code": error.code(),
        "error": error.to_string(),
        "hint": error.hint(),
    });
    CallToolResponse::Complete(CallToolResult::structured_error(payload))
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
