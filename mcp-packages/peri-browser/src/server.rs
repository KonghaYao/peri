//! MCP surface: tools, SEP-2640 skills and skill resources.
//!
//! Tool names and semantics follow README §7. Failures are tool results with
//! `isError` (never protocol errors), so the model can read the stable code and
//! the hint. The only protocol errors are an unknown tool name and an unknown
//! skill/resource URI.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError,
    GetSkillRequestParams, GetSkillResult, Implementation, ListResourcesResult, ListSkillsResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::core::BrowserCore;
use crate::error::BrowserError;

/// Server identity advertised in `initialize`.
pub const SERVER_NAME: &str = "peri-browser-mcp";
/// Hand-off the model performs after `request_authorization` returns.
const AUTHORIZATION_INSTRUCTION: &str =
    "Call AskUserQuestion with this URL so the user can open it; continue only after the user answers.";

/// MCP handler bound to one browser session.
#[derive(Clone)]
pub struct BrowserMcpServer {
    core: Arc<BrowserCore>,
}

impl BrowserMcpServer {
    #[must_use]
    pub fn new(core: Arc<BrowserCore>) -> Self {
        Self { core }
    }

    #[must_use]
    pub fn core(&self) -> &Arc<BrowserCore> {
        &self.core
    }

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
                "Attach this session to a running Kitesurf instance. Required before any page tool; browser_status is the only state source. Accepts http:// or ws:// loopback URLs with an explicit port and fixes the endpoint for the process lifetime.",
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
        request: &CallToolRequestParams,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.as_ref();
        let core = &self.core;
        let response = match name {
            "browser_status" => success(core.status().await),
            "browser_connect" => {
                let args: ConnectArgs = arguments(request)?;
                match core.connect(&args.url).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "list_pages" => success(core.list_pages().await),
            "select_page" => {
                let args: SelectPageArgs = arguments(request)?;
                match core.select_page(&args.page_id).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "new_page" => {
                let args: NewPageArgs = arguments(request)?;
                match core.new_page(args.url.as_deref()).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "close_page" => {
                let args: PageArgs = arguments(request)?;
                match core.close_page(args.page_id.as_deref()).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "navigate_page" => {
                let args: NavigateArgs = arguments(request)?;
                match self.navigate(&args).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "take_snapshot" => {
                let args: PageArgs = arguments(request)?;
                match self.page(&args).await {
                    Ok(page) => match page.take_snapshot(core).await {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "take_screenshot" => {
                let args: PageArgs = arguments(request)?;
                match self.screenshot(&args).await {
                    Ok(result) => CallToolResponse::Complete(result),
                    Err(error) => failure(error),
                }
            }
            "evaluate_script" => {
                let args: EvaluateArgs = arguments(request)?;
                if args.expression.trim().is_empty() {
                    return Ok(failure(BrowserError::InvalidArgument {
                        name: "expression".to_owned(),
                        reason: "must not be empty".to_owned(),
                    }));
                }
                match self
                    .page(&PageArgs {
                        page_id: args.page_id,
                    })
                    .await
                {
                    Ok(page) => match page.evaluate_script(core, &args.expression).await {
                        Ok(value) => success(json!({"result": value, "pageId": page.id})),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "click" => {
                let args: ElementArgs = arguments(request)?;
                match self.page(&args.page_args()).await {
                    Ok(page) => match page
                        .click(core, args.uid.as_deref(), args.selector.as_deref())
                        .await
                    {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "fill" => {
                let args: FillArgs = arguments(request)?;
                match self.page(&args.element.page_args()).await {
                    Ok(page) => match page
                        .fill(
                            core,
                            args.element.uid.as_deref(),
                            args.element.selector.as_deref(),
                            &args.text,
                        )
                        .await
                    {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "hover" => {
                let args: ElementArgs = arguments(request)?;
                match self.page(&args.page_args()).await {
                    Ok(page) => match page
                        .hover(core, args.uid.as_deref(), args.selector.as_deref())
                        .await
                    {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "press_key" => {
                let args: PressKeyArgs = arguments(request)?;
                match self
                    .page(&PageArgs {
                        page_id: args.page_id,
                    })
                    .await
                {
                    Ok(page) => match page.press_key(core, &args.key).await {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "wait_for" => {
                let args: WaitArgs = arguments(request)?;
                match self
                    .page(&PageArgs {
                        page_id: args.page_id,
                    })
                    .await
                {
                    Ok(page) => match page
                        .wait_for(
                            core,
                            args.text.as_deref(),
                            args.predicate.as_deref(),
                            args.timeout_ms,
                        )
                        .await
                    {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "list_console_messages" => {
                let args: PageArgs = arguments(request)?;
                match self.page(&args).await {
                    Ok(page) => match page.list_console(core).await {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "list_network_requests" => {
                let args: NetworkListArgs = arguments(request)?;
                match self
                    .page(&PageArgs {
                        page_id: args.page_id,
                    })
                    .await
                {
                    Ok(page) => match page.list_network(core, args.filter.as_deref()).await {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "get_network_request" => {
                let args: NetworkRequestArgs = arguments(request)?;
                match self
                    .page(&PageArgs {
                        page_id: args.page_id,
                    })
                    .await
                {
                    Ok(page) => match page.get_network_request(core, &args.request_id).await {
                        Ok(value) => success(value),
                        Err(error) => failure(error),
                    },
                    Err(error) => failure(error),
                }
            }
            "yield_page" => match core.yield_page().await {
                Ok(value) => success(value),
                Err(error) => failure(error),
            },
            "request_human" => {
                let args: RequestHumanArgs = arguments(request)?;
                if args.prompt.trim().is_empty() {
                    return Ok(failure(BrowserError::InvalidArgument {
                        name: "prompt".to_owned(),
                        reason: "must not be empty".to_owned(),
                    }));
                }
                match core.request_human(&args.prompt, args.timeout_ms).await {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                }
            }
            "request_authorization" => {
                let args: AuthorizationArgs = arguments(request)?;
                self.request_authorization(&args)
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown tool: {other}"),
                    None,
                ));
            }
        };
        Ok(response)
    }

    async fn page(&self, args: &PageArgs) -> Result<Arc<crate::page::PageHandle>, BrowserError> {
        self.core.tool_page(args.page_id.as_deref()).await
    }

    async fn navigate(&self, args: &NavigateArgs) -> Result<Value, BrowserError> {
        let page = self
            .page(&PageArgs {
                page_id: args.page_id.clone(),
            })
            .await?;
        let value = page.navigate_page(&self.core, &args.url).await?;
        self.core.sync_pages().await;
        Ok(value)
    }

    async fn screenshot(&self, args: &PageArgs) -> Result<CallToolResult, BrowserError> {
        let page = self.page(args).await?;
        let shot = page.take_screenshot(&self.core).await?;
        let mut result = CallToolResult::structured(json!({
            "pageId": page.id,
            "mimeType": shot.mime_type,
        }));
        result
            .content
            .push(ContentBlock::image(shot.data, shot.mime_type));
        Ok(result)
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
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_skills()
            .build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Browser control through a Kitesurf instance. Start with browser_status; if it \
                 reports connected: false, follow nextSteps and call browser_connect before any \
                 page tool. Use take_snapshot before uid-based interaction, and take a new \
                 snapshot after every navigation. A page starts as yours; the monitor may ask \
                 for it, in which case page tools return page_busy_by_human until you reclaim \
                 it. Skills: skill://peri-browser/browser-automation/SKILL.md and \
                 skill://peri-browser/debugging/SKILL.md.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let response = self.dispatch(&request).await;
        self.core.touch();
        response
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = crate::skills::resources()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let raw = crate::skills::read(&request.uri).map_err(|_| {
            McpError::resource_not_found(format!("no such resource: {}", request.uri), None)
        })?;
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
        _context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        let skills = crate::skills::catalog()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(ListSkillsResult::with_all_items(skills))
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        let skill = crate::skills::skill(&request.uri).map_err(|_| {
            McpError::invalid_params(format!("unknown skill uri: {}", request.uri), None)
        })?;
        Ok(GetSkillResult::new(skill))
    }
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

impl ElementArgs {
    fn page_args(&self) -> PageArgs {
        PageArgs {
            page_id: self.page_id.clone(),
        }
    }
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
