//! MCP App 文档：顶层 App 视图、iframe 宿主页与 sandbox 诊断页。
//!
//! `/apps/view` 把 App HTML 与 host bridge 放进同一个顶层文档：Kitesurf 0.5.4 的
//! iframe 运行时无法执行 `@modelcontextprotocol/ext-apps`（`setRequestHandler`
//! 报 callee=undefined），顶层文档可以。`/apps/host` + `/apps/sandbox` 保留为
//! iframe 诊断路径，全部由独立 loopback listener 提供，与主网关 listener 的
//! 生命周期互不绑定：sandbox 监听不可用时网关仍返回宿主页。
//!
//! sandbox iframe 是不透明源（`sandbox="allow-scripts"`），host 侧不能读它的
//! DOM，CDP 输入也不进入子 frame。App 视图不走 iframe，真实 CDP 鼠标事件直接
//! 作用于顶层文档；sandbox 指针 shim 只为诊断页保留。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, MethodRouter};
use axum::Router;
use serde::Deserialize;
use tokio::net::TcpListener;

use crate::config::is_bind_ip;

/// 网关监听上的宿主页路径（iframe 诊断）。
pub(crate) const HOST_PATH: &str = "/apps/host";
/// sandbox 监听上的沙箱源页路径；宿主页的 iframe 指向它。
pub(crate) const SANDBOX_PATH: &str = "/apps/sandbox";
/// sandbox 监听上的顶层 App 视图路径：App HTML + 同文档 host bridge。
pub(crate) const VIEW_PATH: &str = "/apps/view";
/// `--app-html` 的读取上限：超过即拒绝启动，避免把整份文档常驻内存。
pub const MAX_APP_HTML_BYTES: usize = 5 * 1024 * 1024;

/// App 视图与 sandbox 页的 CSP：内联脚本与样式、`data:`/`blob:` 图片可用，其余
/// 外部资源与网络请求全部禁止。MCP App 的 bundle 必须自带脚本与样式才能运行。
const SANDBOX_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'";

/// 宿主页模板：只承载 iframe，`{bridge}` 注入共享 bridge，`{frame}` 注入 iframe。
const HOST_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>peri-browser host page</title>
<style>
  html, body { margin: 0; height: 100%; background: #0b0b0c; }
  #f { position: fixed; inset: 0; width: 100%; height: 100%; border: 0; display: block; }
</style>
<script>
// iframe 模式：bridge 只和这个 window 通信。App 视图不设置该函数，bridge 直接
// 使用自身 window（同文档模式）。
window.__periBridgeTarget = function () {
  var frame = document.getElementById("f");
  return frame ? frame.contentWindow : null;
};
</script>
{bridge}
</head>
<body>
{frame}
</body>
</html>
"#;

/// 最小 MCP Apps host bridge：classic script，无 SDK、无构建。
///
/// 同一份脚本服务两种承载：
/// - 顶层 App 视图（`/apps/view`）：App 与 bridge 同文档，App 的 transport 把
///   请求 `postMessage` 给 `window.parent`，也就是本窗口；bridge 用
///   `window.postMessage(response, "*")` 回复自身。
/// - 宿主诊断页（`/apps/host`）：`__periBridgeTarget` 指向 iframe 的 window。
///
/// 消息过滤：只处理带 `method` 的 JSON-RPC 请求/通知。bridge 自己的响应只带
/// `result`/`error`，因此同文档模式下不会响应自己的响应。
///
/// 信任边界：App 与 bridge 同源同窗口，App 可以改写 bridge。视图只在专用
/// loopback origin 提供，带严格 CSP，不读取、不回发任何凭据，也不访问网络；
/// 生产宿主应改回双 origin 或引擎级隔离。
const BRIDGE_SCRIPT: &str = r#"<script>
(function () {
  "use strict";
  var PROTOCOL_VERSION = "2026-01-26";
  var HOST_INFO = { name: "peri-browser", version: "{version}" };
  var WAKE = "peri-view:app-request";

  window.__log = [];
  window.__msgs = [];
  window.__toolCalls = [];
  window.__onToolCall = undefined;

  // 外部宿主（view session）模式：`/apps/view?view=<id>` 会先注入
  // __periViewSession。宿主在另一进程（HTTP），无法在 bridge 的消息分发内同步
  // 回复，因此 relay 模式改为拦截 App 的出站 JSON-RPC：请求进入宿主队列，不再
  // 投递给本窗口，App 自己的 MCP client 也就收不到自己的请求、不会同步自答
  // -32601；宿主结果随后经 __bridge.hostReply 异步送达。
  var SESSION = window.__periViewSession || null;
  var relay = !!(SESSION && SESSION.id) && appWindow() === window;
  var outbound = [];
  var outboundSeq = 0;
  // relay 模式下 window.postMessage 被替换，bridge 自己的投递必须走原生实现。
  var nativePost = window.postMessage.bind(window);

  // Same-document view: window.parent === window, so the App's transport and
  // this bridge share one window. The iframe host page installs
  // __periBridgeTarget instead; a null target means "no app attached".
  function appWindow() {
    var resolve = window.__periBridgeTarget;
    if (typeof resolve === "function") return resolve();
    return window;
  }

  function record(direction, method, id) {
    window.__log.push({ dir: direction, method: method, id: id === undefined ? null : id });
  }

  function post(message) {
    var win = appWindow();
    if (!win) {
      record("out", "no-target");
      return false;
    }
    if (win === window) {
      nativePost(message, "*");
      return true;
    }
    win.postMessage(message, "*");
    return true;
  }

  function reply(id, method, result) {
    record("out", method, id);
    post({ jsonrpc: "2.0", id: id, result: result });
  }

  function replyError(id, method, code, message) {
    record("out", method + " (error)", id);
    post({ jsonrpc: "2.0", id: id, error: { code: code, message: message } });
  }

  function notify(method, params) {
    record("out", method);
    var message = { jsonrpc: "2.0", method: method };
    if (params !== undefined) message.params = params;
    post(message);
  }

  function hostContext() {
    return {
      theme: "dark",
      displayMode: "inline",
      availableDisplayModes: ["inline"],
      platform: "web",
      locale: navigator.language || "en-US",
      timeZone: "UTC",
      userAgent: HOST_INFO.name + "/" + HOST_INFO.version,
      containerDimensions: { width: window.innerWidth, height: window.innerHeight },
      deviceCapabilities: { hover: true, touch: false }
    };
  }

  // McpUiInitializeResult: protocolVersion / hostInfo / hostCapabilities /
  // hostContext, field names as in ext-apps 1.7.5 spec.types.d.ts.
  function initializeResult(params) {
    var version = params && typeof params.protocolVersion === "string"
      ? params.protocolVersion
      : PROTOCOL_VERSION;
    return {
      protocolVersion: version,
      hostInfo: HOST_INFO,
      hostCapabilities: { serverTools: {}, logging: {}, message: {} },
      hostContext: hostContext()
    };
  }

  function errorText(error) {
    return String((error && error.message) || error);
  }

  function isThenable(value) {
    return value !== null &&
      (typeof value === "object" || typeof value === "function") &&
      typeof value.then === "function";
  }

  function replyResult(id, result) {
    if (result === undefined || result === null) {
      replyError(id, "tools/call", -32603,
        "__onToolCall returned " + String(result) + " instead of a CallToolResult");
      return;
    }
    reply(id, "tools/call", result);
  }

  function handleToolCall(id, params) {
    window.__toolCalls.push(params);
    var handler = window.__onToolCall;
    if (typeof handler !== "function") {
      replyError(id, "tools/call", -32601,
        "host bridge has no __onToolCall handler; install one before the app calls tools/call");
      return;
    }
    // Reply inside this dispatch. In the same-document view the App's own MCP
    // client also sees its outbound request and answers "Method not found"
    // synchronously; a response deferred to a microtask would lose that race.
    var result;
    try {
      result = handler(params);
    } catch (error) {
      replyError(id, "tools/call", -32603, "__onToolCall failed: " + errorText(error));
      return;
    }
    if (isThenable(result)) {
      // Async handlers are best-effort: their answer arrives too late for the
      // App's already-settled request. Keep a synchronous handler for the
      // validation path; a production host needs split origins instead.
      result.then(function (value) {
        replyResult(id, value);
      }, function (error) {
        replyError(id, "tools/call", -32603, "__onToolCall failed: " + errorText(error));
      });
      return;
    }
    replyResult(id, result);
  }

  function onMessage(event) {
    var win = appWindow();
    // Only the App window may talk to this bridge. At top level that window is
    // this window, so the source cannot separate bridge from App; the method
    // check below drops our own responses (they carry result/error only).
    if (!win || event.source !== win) return;
    window.__msgs.push(event.data);
    var data = event.data;
    if (!data || data.jsonrpc !== "2.0" || typeof data.method !== "string") return;
    // relay 模式的出站请求在 postMessage 处已被拦截，不会走到这里；这里只处理
    // 宿主自己的投递（响应/通知）以及 bridge 自身。
    record("in", data.method, data.id);
    if (data.id === undefined || data.id === null) {
      if (data.method === "ui/notifications/initialized") window.__bridge.initialized = true;
      return;
    }
    if (data.method === "ui/initialize") {
      reply(data.id, data.method, initializeResult(data.params));
    } else if (data.method === "tools/call") {
      handleToolCall(data.id, data.params || {});
    } else {
      replyError(data.id, data.method, -32601, "unsupported method " + data.method);
    }
  }

  // 唤醒宿主：console 事件是 CDP Runtime.consoleAPICalled 的推送信号，队列本身
  // 由宿主通过 __bridge.takeRequests 取走，因此消息体不进 console 日志。
  function wakeHost() {
    console.debug(WAKE + " " + outbound.length);
  }

  function handleOutbound(data) {
    window.__msgs.push(data);
    record("in", data.method, data.id);
    if (data.id === undefined || data.id === null) {
      if (data.method === "ui/notifications/initialized") window.__bridge.initialized = true;
      return;
    }
    if (data.method === "ui/initialize") {
      // 握手仍由 bridge 同步完成：宿主在轮询/推送之前就要能拿到请求队列。
      reply(data.id, data.method, initializeResult(data.params));
      return;
    }
    if (data.method === "tools/call") {
      window.__toolCalls.push(data.params || {});
      outboundSeq += 1;
      outbound.push({
        seq: outboundSeq,
        id: data.id,
        method: data.method,
        params: data.params || {}
      });
      wakeHost();
      return;
    }
    replyError(data.id, data.method, -32601, "unsupported method " + data.method);
  }

  window.__bridge = {
    initialized: false,
    relay: relay,
    session: SESSION ? SESSION.id : null,
    sendToolInput: function (args) {
      notify("ui/notifications/tool-input", { arguments: args === undefined ? {} : args });
    },
    sendToolResult: function (result) {
      notify("ui/notifications/tool-result", result);
    },
    // 宿主 drain：返回并清空 App 发起的请求。返回空数组时宿主无需任何处理。
    takeRequests: function () {
      var taken = outbound;
      outbound = [];
      return taken;
    },
    // 宿主回填结果。消息必须是纯 JSON-RPC：SDK 的响应 schema 是 strict，
    // 附加字段会让 App 丢弃整条消息。
    hostReply: function (message) {
      if (!message || message.jsonrpc !== "2.0") return false;
      post(message);
      return true;
    },
    hostNotify: function (method, params) {
      notify(method, params);
      return true;
    },
    // sandbox 诊断页专用：把坐标转成 peri:pointer 消息，由 shim 在文档内派发
    // 合成事件。顶层 App 视图用真实 CDP 输入，不需要这条路径。
    click: function (x, y) {
      x = Number(x);
      y = Number(y);
      if (!Number.isFinite(x) || !Number.isFinite(y)) {
        throw new Error("__bridge.click(x, y) needs finite coordinates");
      }
      record("out", "peri:pointer");
      post({ type: "peri:pointer", x: x, y: y });
    }
  };

  if (relay) {
    // 只拦截出站 JSON-RPC（method 为字符串，含请求与通知）：不投递给本窗口，
    // 因此 App 自己的 MCP client 收不到自己的请求；宿主 -> App 的响应由
    // bridge 用原生 postMessage 投递。
    window.postMessage = function (message, targetOrigin, transfer) {
      if (message && typeof message === "object" && message.jsonrpc === "2.0" &&
          typeof message.method === "string") {
        handleOutbound(message);
        return;
      }
      return nativePost(message, targetOrigin, transfer);
    };
  }

  window.addEventListener("message", onMessage);
})();
</script>
"#;

/// sandbox 占位页；`--app-html` 未设置时使用。
const PLACEHOLDER_SANDBOX_HTML: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>peri-browser sandbox</title>
<body style="margin:0;background:#0b0b0c">
<div style="width:160px;height:96px;background:#4ade80"></div>
<script>
parent.postMessage("sandbox-ready", "*");
</script>
</body>
</html>
"#;

/// 注入到 sandbox 诊断页末尾的指针 shim。
///
/// 追加在整个文档之后而不是插进 `<body>`：字符串里的 `</body>` 可能落在脚本
/// 字面量中，追加不会破坏已有内容，浏览器照样把尾部节点放进 body。顶层 App
/// 视图不注入该 shim，真实 CDP 输入由 `Input.dispatchMouseEvent` 提供。
const POINTER_SHIM: &str = r#"<script>
(function () {
  "use strict";
  function targetAt(x, y) {
    return document.elementFromPoint(x, y) || document.body || document.documentElement;
  }
  function fire(element, constructor, type, x, y, buttons) {
    var init = {
      bubbles: true, cancelable: true, composed: true, view: window,
      clientX: x, clientY: y, screenX: x, screenY: y,
      button: 0, buttons: buttons, detail: type === "click" ? 1 : 0,
      pointerId: 1, pointerType: "mouse", isPrimary: true,
      pressure: buttons ? 0.5 : 0
    };
    var event;
    try { event = new constructor(type, init); } catch (error) { return; }
    element.dispatchEvent(event);
  }
  function press(x, y) {
    var element = targetAt(x, y);
    var Pointer = typeof PointerEvent === "function" ? PointerEvent : MouseEvent;
    fire(element, Pointer, "pointerdown", x, y, 1);
    fire(element, MouseEvent, "mousedown", x, y, 1);
    fire(element, Pointer, "pointerup", x, y, 0);
    fire(element, MouseEvent, "mouseup", x, y, 0);
    // dispatchEvent 不产生激活行为，所以 click 显式补一次；down/up 本身不会
    // 再合成第二个 click。
    fire(element, MouseEvent, "click", x, y, 0);
  }
  window.addEventListener("message", function (event) {
    if (event.source !== window.parent) return;
    var data = event.data;
    if (!data || data.type !== "peri:pointer") return;
    var x = Number(data.x);
    var y = Number(data.y);
    if (!Number.isFinite(x) || !Number.isFinite(y)) return;
    press(x, y);
  });
  parent.postMessage({ type: "peri:sandbox-ready" }, "*");
})();
</script>
"#;

/// bridge 脚本，版本号来自 [`crate::version`]。
fn bridge_script() -> String {
    BRIDGE_SCRIPT.replace("{version}", crate::version())
}

/// 会话文档的前置配置：让 bridge 进入 relay（外部宿主）模式。
///
/// 只放会话 id，不放 token 或宿主地址：文档在无鉴权的 loopback 文档源上，
/// bridge 也不访问网络。
fn session_script(session: &str) -> String {
    format!(
        "<script>window.__periViewSession = {{ id: {} }};</script>",
        serde_json::to_string(session).expect("session id serializes")
    )
}

/// 把 bridge 注入 App 文档。
///
/// 优先插在 `<head ...>` 开标签之后（App 的模块脚本在 head 内，classic
/// bridge 会在它之前执行）；没有 head 时退回 `<html ...>`、`<!doctype ...>`，
/// 最后才整体前置。只做切片拼接，不改动 App 原有字节。`session` 非空时先注入
/// 会话配置，bridge 据此启用宿主中继。
fn inject_bridge(html: &str, session: Option<&str>) -> String {
    let mut bridge = String::new();
    if let Some(session) = session {
        bridge.push_str(&session_script(session));
    }
    bridge.push_str(&bridge_script());
    let lower = html.to_ascii_lowercase();
    let offset = tag_end_after(&lower, "<head")
        .or_else(|| tag_end_after(&lower, "<html"))
        .or_else(|| tag_end_after(&lower, "<!doctype"))
        .unwrap_or(0);
    let mut document = String::with_capacity(html.len() + bridge.len());
    document.push_str(&html[..offset]);
    document.push_str(&bridge);
    document.push_str(&html[offset..]);
    document
}

fn tag_end_after(lower: &str, needle: &str) -> Option<usize> {
    let start = lower.find(needle)?;
    let end = lower[start..].find('>')? + start + 1;
    Some(end)
}

/// 一个 sandbox listener 的文档：顶层 App 视图、iframe sandbox 源页和宿主页。
pub struct AppDocument {
    /// App HTML 原文（`--app-html`）或占位页。
    raw: Arc<str>,
    /// 顶层视图：原文 + 注入 head 的 bridge。
    view: Arc<str>,
    placeholder: bool,
}

impl std::fmt::Debug for AppDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppDocument")
            .field("bytes", &self.raw.len())
            .field("placeholder", &self.placeholder)
            .finish()
    }
}

impl AppDocument {
    /// Placeholder used when `--app-html` is absent.
    #[must_use]
    pub fn placeholder() -> Self {
        Self::build(PLACEHOLDER_SANDBOX_HTML, true)
    }

    /// Wraps inline App HTML and injects the host bridge.
    #[must_use]
    pub fn from_app_html(html: &str) -> Self {
        Self::build(html, false)
    }

    fn build(html: &str, placeholder: bool) -> Self {
        Self {
            raw: Arc::from(html),
            view: Arc::from(inject_bridge(html, None)),
            placeholder,
        }
    }

    /// Reads `--app-html` into memory, rejecting missing, oversized and
    /// non-UTF-8 documents.
    ///
    /// # Errors
    ///
    /// [`AppHtmlError`] when the file cannot be read, exceeds
    /// [`MAX_APP_HTML_BYTES`] or is not UTF-8.
    pub fn load(path: &Path) -> Result<Self, AppHtmlError> {
        let file = std::fs::File::open(path).map_err(|source| AppHtmlError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_APP_HTML_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| AppHtmlError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        if bytes.len() > MAX_APP_HTML_BYTES {
            return Err(AppHtmlError::TooLarge {
                path: path.to_path_buf(),
                limit: MAX_APP_HTML_BYTES,
            });
        }
        let html = String::from_utf8(bytes).map_err(|_| AppHtmlError::NotUtf8 {
            path: path.to_path_buf(),
        })?;
        Ok(Self::from_app_html(&html))
    }

    /// 顶层 App 视图：bridge 与 App 同文档。
    pub(crate) fn view_body(&self) -> &str {
        &self.view
    }

    /// sandbox 诊断页：原文（其后追加指针 shim）。
    pub(crate) fn sandbox_body(&self) -> Cow<'_, str> {
        if self.placeholder {
            Cow::Borrowed(&self.raw)
        } else {
            Cow::Owned(format!("{}\n{}", self.raw, POINTER_SHIM))
        }
    }
}

/// 独立 sandbox listener 的文档集合：`--app-html` 回退文档，以及各 view
/// session 自己注册的文档。
///
/// 会话文档由控制面（7391）在创建会话时写入、关闭时移除；文档源（7392）
/// 只负责按 `?view=<id>` 查表，不持有会话生命周期。
pub struct AppDocuments {
    fallback: AppDocument,
    views: Mutex<BTreeMap<String, Arc<str>>>,
}

impl std::fmt::Debug for AppDocuments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppDocuments")
            .field("fallback", &self.fallback)
            .field("views", &self.views.lock().map(|v| v.len()).unwrap_or(0))
            .finish()
    }
}

impl AppDocuments {
    /// Wraps the `--app-html` document (or placeholder) as the default view.
    #[must_use]
    pub fn new(fallback: AppDocument) -> Self {
        Self {
            fallback,
            views: Mutex::new(BTreeMap::new()),
        }
    }

    /// Registers the view body for one session, injecting the relay bridge.
    pub(crate) fn insert_view(&self, session: &str, html: &str) {
        let body: Arc<str> = Arc::from(inject_bridge(html, Some(session)));
        self.views
            .lock()
            .expect("view documents lock")
            .insert(session.to_owned(), body);
    }

    /// Forgets a session document; the HTML must not outlive its session.
    pub(crate) fn remove_view(&self, session: &str) {
        self.views
            .lock()
            .expect("view documents lock")
            .remove(session);
    }

    /// `None` selects the `--app-html` view; `Some(id)` must name a live session.
    fn view_body(&self, session: Option<&str>) -> Option<Arc<str>> {
        match session {
            None => Some(Arc::from(self.fallback.view_body())),
            Some(id) => self
                .views
                .lock()
                .expect("view documents lock")
                .get(id)
                .cloned(),
        }
    }
}

/// `--app-html` / `PERI_BROWSER_APP_HTML` failures; all of them stop the
/// gateway before it starts listening.
#[derive(Debug, thiserror::Error)]
pub enum AppHtmlError {
    #[error("cannot read app html {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("app html {path} is larger than {limit} bytes")]
    TooLarge { path: PathBuf, limit: usize },
    #[error("app html {path} is not valid UTF-8")]
    NotUtf8 { path: PathBuf },
}

/// 网关路由里的宿主页；`sandbox_url` 是 iframe 的完整地址。
pub(crate) fn host_router<S>(sandbox_url: Option<String>) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(move || {
        let sandbox_url = sandbox_url.clone();
        async move { host_page(sandbox_url.as_deref()) }
    })
}

/// 独立 sandbox listener：`/apps/view` 提供顶层 App，`/apps/sandbox` 提供
/// iframe 诊断页；只接受 loopback 地址。
///
/// `/apps/view` 不带查询参数时返回 `--app-html` 文档；`?view=<id>` 返回该
/// view session 注册的文档（不匹配即 404，不回退到回退文档）。
pub async fn serve(
    listener: TcpListener,
    documents: Arc<AppDocuments>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let bound = listener.local_addr()?;
    anyhow::ensure!(
        is_bind_ip(bound.ip()),
        "sandbox requires a loopback listener"
    );
    let app = Router::new()
        .route(VIEW_PATH, get(view_page))
        .route(SANDBOX_PATH, get(sandbox_page))
        .with_state(documents);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

fn host_page(sandbox_url: Option<&str>) -> Response {
    let frame = sandbox_url.map_or_else(
        || "<p style=\"color:#9fe;font:14px monospace\">sandbox is disabled</p>".to_owned(),
        |url| {
            format!(
                r#"<iframe id="f" src="{url}" sandbox="allow-scripts" width="480" height="320"></iframe>"#
            )
        },
    );
    page_response(
        HOST_TEMPLATE
            .replace("{bridge}", &bridge_script())
            .replace("{frame}", &frame),
        None,
    )
}

async fn view_page(
    State(documents): State<Arc<AppDocuments>>,
    Query(query): Query<ViewQuery>,
) -> Response {
    match documents.view_body(query.view.as_deref()) {
        Some(body) => page_response(body.to_string(), Some(SANDBOX_CSP)),
        None => (
            StatusCode::NOT_FOUND,
            [("content-type", "text/plain; charset=utf-8")],
            "unknown view session",
        )
            .into_response(),
    }
}

/// `/apps/view` 的查询参数；`view` 是 view session id。
#[derive(Debug, Default, Deserialize)]
struct ViewQuery {
    view: Option<String>,
}

async fn sandbox_page(State(documents): State<Arc<AppDocuments>>) -> Response {
    page_response(
        documents.fallback.sandbox_body().into_owned(),
        Some(SANDBOX_CSP),
    )
}

fn page_response(body: String, csp: Option<&'static str>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    if let Some(csp) = csp {
        headers.insert(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static(csp),
        );
    }
    (headers, body).into_response()
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
