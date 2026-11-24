# peri-browser-gateway

本机浏览器服务的唯一 owner：一个主 loopback listener 同时提供 MCP Streamable HTTP（`/mcp`）、人工监控页面、宿主页与外部宿主的 view session API（`/view/sessions*`）；可选的第二个 loopback listener 提供顶层 App 视图（App HTML + 同文档 host bridge）与 sandbox 诊断页。每个 MCP 会话拥有独立的 `BrowserCore`（端点、页面、活动页、快照），页面写入经唯一 `PageControl` writer；人可在 monitor 里接管同一页面。view session 是独立于 MCP 会话的第二类宿主：宿主（Peri TUI）提交 App HTML，网关渲染顶层文档并推送 screencast 帧、接受真实 CDP 输入、中继 App 的 `tools/call`。

`mcp-packages/peri-browser` 的 stdio 服务与旧二进制已移除；本 crate 是运行时契约的唯一来源，迁移步骤见文末。工具面与 Kitesurf 隔离模型的历史证据见 `docs/verification/` 与 git 历史。

## 快速开始

```bash
# 安装二进制与 kitesurf 引擎
curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-browser.sh | bash

# 更新网关并确保 kitesurf 已安装；已安装的引擎不会升级
peri-browser update

peri-browser start kitesurf   # 引擎 18971，网关 127.0.0.1:7391；健康检查通过后立即返回
peri-browser stop             # 停止该网关；必要时一并停止它拉起的引擎

# 升级 kitesurf 引擎到最新 release（需先 stop，安装器不会停止运行中的进程）
peri-browser update kitesurf
peri-browser start kitesurf
```

`update` 不带目标时只重新运行上面的 browser 安装脚本，继承当前环境变量（`PERI_BROWSER_INSTALL_DIR`、`PERI_BROWSER_SKIP_KITESURF=1` 等）并原样返回脚本退出码；脚本末尾的 `peri-browser install kitesurf` 在引擎已安装时直接返回，所以裸 `update` 不会升级引擎。`update kitesurf` 则无条件重跑 kitesurf 官方安装脚本，由脚本拉取最新 release 并覆盖安装到 `$KITESURF_INSTALL_DIR`（默认 `$HOME/.kitesurf`）。

`update` 不停止正在运行的网关：进程继续使用旧二进制，需 `peri-browser stop` 后再 `peri-browser start` 才会切到新版本。安装器也不会动正在运行的引擎进程，升级引擎前请先 `peri-browser stop`（或自行停止引擎），更新后再 `peri-browser start kitesurf`。

`start kitesurf` 先确保引擎在后台就绪（已监听则复用，否则后台拉起并等待 `/json/version`），再以 `--cdp-url http://127.0.0.1:18971` 后台启动网关；`/healthz` 通过后命令返回 0，终端不再被占用。成功输出包含 MCP 地址、监控地址、pid、日志路径与 `stop` 提示。

本地开发可以直接构建：

```bash
cargo build -p peri-browser-gateway
./target/debug/peri-browser install kitesurf   # 已安装时只打印版本与路径
./target/debug/peri-browser start kitesurf
```

只运行网关（同样后台）：

```bash
./target/debug/peri-browser start --bind 127.0.0.1:7391
./target/debug/peri-browser stop --bind 127.0.0.1:7391
```

前台运行只用于开发、容器或需要阻塞并响应 SIGTERM 的场景：

```bash
./target/debug/peri-browser serve --bind 127.0.0.1:7391
```

- MCP：`http://127.0.0.1:7391/mcp`
- 监控首页：`http://127.0.0.1:7391/`（列出 Live 会话）
- 单会话页：`http://127.0.0.1:7391/s/{token}/`，WS 为 `/s/{token}/ws`
- 顶层 App 视图：`http://127.0.0.1:7392/apps/view`，返回 `--app-html` 文档并在 `<head>` 注入同文档 host bridge（classic script，无 npm 依赖、无构建步骤）；这是 App 的验收路径
- 宿主页（iframe 诊断）：`http://127.0.0.1:7391/apps/host`，用 sandbox iframe 加载 sandbox 页，并复用同一份 bridge
- sandbox 页（诊断）：`http://127.0.0.1:7392/apps/sandbox`；独立 listener，`--app-html` 未设置时返回可见色块占位页，设置后返回该 App HTML 并追加指针 shim。与 App 视图共用 CSP `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'`
- 健康检查：`/healthz`

### 命令面

| 命令 | 行为 |
| --- | --- |
| `peri-browser [OPTIONS]`、`peri-browser serve [OPTIONS]` | 前台运行网关；裸参数与 `serve` 后参数走同一套解析。 |
| `peri-browser start [OPTIONS]` | 后台运行网关，`OPTIONS` 与 `serve` 相同；`/healthz` 通过后返回 0。 |
| `peri-browser start kitesurf [--port N] [--bind IP:PORT]` | 先确保 `kitesurf serve --port <N> --allow-private-network` 就绪（日志 `$HOME/.kitesurf/serve-<N>.log`），再后台启动网关。 |
| `peri-browser stop [--bind IP:PORT]` | 停止该 bind 上由 `start` 拉起的网关；未运行时打印提示并返回 0。 |
| `peri-browser install kitesurf` | 安装 kitesurf；已安装则打印版本与路径并返回 0。 |
| `peri-browser update` | 重新运行 browser 安装脚本，确保 kitesurf 已安装；已安装的引擎不会升级。继承环境变量并原样返回脚本退出码。 |
| `peri-browser update kitesurf` | 无条件重跑 kitesurf 官方安装脚本，覆盖安装最新 release；不停止运行中的网关或引擎。 |
| `peri-browser --version`、`peri-browser -h\|--help` | 版本与命令帮助。 |

`start` / `stop` 的 `--bind` 默认 `127.0.0.1:7391`，`start kitesurf` 的 `--port` 默认 `18971`；`--bind` 只接受环回地址。端口 `0` 只对 `serve` / 裸调用有效；`start` 与 `start kitesurf` 需要固定端口，传 `0` 会被拒绝。引擎未安装时 `start kitesurf` 失败并提示先运行 `peri-browser install kitesurf`。

### 后台运行、状态与日志

- 状态：`$HOME/.peri/browser/run/gateway-<port>.json`，记录 `pid`、`pid_start`、`bind`、`log`、`engine_port`、`engine_owned` 与 `started_at`；目录权限 0700。
- 日志：`$HOME/.peri/browser/logs/gateway-<port>.log`，追加写；目录权限 0700。
- 后台进程由 `current_exe serve <原样网关参数>` 启动，独立进程组、stdin 关闭，终端退出不影响它。
- `start` 只在该子进程 `/healthz` 返回自身 pid 后才排他发布状态；并发启动只会有一个发布成功，失败方终止自己的子进程且不删除既有状态。
- `start` 时同一端口已有存活网关会失败，并给出 pid 与 `stop` 提示；记录中的 pid 已退出、已被复用（`pid_start` 不同）则清理陈旧状态后继续。地址已有未记录的 `/healthz` 应答时直接拒绝，不接管该进程。
- `stop` 只向 `peri-browser` 进程发 SIGTERM，不用 SIGKILL：pid 存活但不是 `peri-browser`，或 5 秒内未退出时返回 1 并保留状态。
- 只有网关启动时拥有引擎（`start kitesurf` 拉起而非复用），且没有其他 `run/gateway-*.json` 引用同一引擎端口时，`stop` 才追加执行 `kitesurf stop --port N`；复用引擎不归 `stop` 关闭。引擎停止失败时状态保留，返回 1，可重试。

### 安装脚本

`install-browser.sh` 支持 kitesurf 官方安装器覆盖的两个平台（macOS arm64、Linux x86_64），校验发布资产的 SHA-256，安装到 `$HOME/.peri/browser/bin/peri-browser`，随后执行 `peri-browser install kitesurf`。

`peri-browser update` 直接复用该脚本：以 `bash -c 'set -o pipefail; curl -fsSL …/install-browser.sh | bash'` 执行 README 里的同一条命令，失败下载不会被空脚本掩盖；stdin/stdout/stderr 与环境变量都继承，退出码原样返回。`PERI_BROWSER_SKIP_KITESURF=1 peri-browser update` 只更新网关二进制。安装脚本只替换软链，正在运行的网关继续使用旧二进制，更新后需 `peri-browser stop` 再 `peri-browser start`。

`peri-browser update kitesurf` 绕过 `install kitesurf` 的“已安装即返回”短路，直接执行 `scripts/install-kitesurf.sh`（先 `curl` 下载到私有临时目录，再用 `bash` 运行）。运行前打印当前状态（`updating kitesurf <version> ...`，未安装时为 `kitesurf is not installed; installing the latest release`），退出码原样返回。安装器只替换软链、不停止任何进程：网关或引擎在运行时先 `peri-browser stop`（或自行停止引擎），更新后再 `peri-browser start kitesurf` 才会用上新版本。

Peri 侧配置（HTTP MCP，宿主不含任何浏览器协商代码）：

```json
{
  "mcpServers": {
    "peri-browser": {
      "type": "http",
      "url": "http://127.0.0.1:7391/mcp"
    }
  }
}
```

换端口时，`start kitesurf --bind` 与这里的 `url` 必须一致。

## 架构

```mermaid
flowchart TD
  HTTP[axum /mcp + monitor + /apps/host] --> MCP[rmcp Streamable HTTP]
  HTTP --> UI[monitor HTTP / WS]
  HTTP --> HOST[/apps/host iframe 诊断]
  SANDBOX[独立 loopback listener] --> VIEW["/apps/view 顶层 App + bridge<br/>?view=ID 会话文档"]
  SANDBOX --> SBX[/apps/sandbox + CSP]
  HTTP --> VS["/view/sessions* 控制面<br/>token + SSE + input"]
  MCP --> D[SessionDirectory]
  UI --> D
  D --> S[BrowserCore per MCP session]
  S --> P[PageHandle]
  P --> L[PageControl 唯一 writer]
  L --> CDP[CDP page connection]
  S --> V[只读 viewer / 帧]
  VS --> VT[view session：独立 target]
  VT --> VC[writer controller：输入/求值/中继]
  VT --> VV[只读 viewer：screencast]
  VIEW -. 同文档 bridge relay .-> VT
```

- `src/apps.rs`：`/apps/view` 返回 App HTML 并在 `<head>` 注入同文档 host bridge；`/apps/host` 返回内嵌 sandbox iframe 的宿主页并复用同一份 bridge；`/apps/sandbox` 为 iframe 诊断页，文档末尾追加指针 shim。三者由独立 listener 提供，响应带 CSP 与 `nosniff`；`--app-html` 的存在性、大小与 UTF-8 在监听前校验。
- `src/mcp_http.rs`：`GatewaySessions` 实现 rmcp `SessionManager`。`initialize` 成功且记录仍可提交时才发布 `Live`；请求经 `BoundBrowserSession` extension 取得本会话许可。未绑定、伪造标记或 `Closing` 的 `tools/*`、`skills/*`、`resources/*` 一律被拒。
- `src/session.rs`：`Initializing → Live → Closing → Settled`，同步封闭准入，有限等待在途任务结算；关闭超时可见，不伪装成功。
- `src/core/`、`src/page/`、`src/cdp/`：端点与标签、页面元数据/快照/诊断、唯一 attach/lease/串行写、CDP 连接与导航。
- `src/stream.rs` 只用只读 viewer；monitor 不能借帧通道获得写权限。
- `src/view.rs`、`src/view/api.rs`、`src/view/page.rs`：view session 注册表与鉴权、HTTP/SSE 面、每个会话自己的 target + writer controller + viewer；不经 `SessionDirectory`，也不出现在 MCP 会话的页面集合里。

### 顶层 App 视图、同文档 bridge 与 sandbox 诊断页

Kitesurf 0.5.4 的 iframe 运行时无法执行官方 `@modelcontextprotocol/ext-apps`：`App.connect()` 在 iframe 内抛 `TypeError: not a callable function (callee=undefined, in fn 'setRequestHandler')`，esbuild 转译到 es2015 后依旧失败；同一份单文件 HTML 在顶层文档能正常加载 SDK，只因缺少 host 桥而收到 `-32601`。因此 App 的验收路径改成 `/apps/view`：App 作为顶层文档，bridge 与它同文档；`/apps/host` + `/apps/sandbox` 退为 iframe 诊断/占位，页面加载与 App 视图互不影响。

共享 bridge 是 classic `<script>`（无 module、无构建），注入到 App HTML `<head>` 开标签之后，早于 App 的模块脚本执行：

| 位置 | 接口 | 行为 |
| --- | --- | --- |
| 同文档 | `ui/initialize` → result | 返回协商版本（回显 App 版本或 `2026-01-26`）、`hostInfo`、`hostCapabilities`、`hostContext`（含 `containerDimensions`） |
| 同文档 | `ui/notifications/initialized` | 置 `window.__bridge.initialized = true` |
| 同文档 | `window.__bridge.sendToolInput(args)` / `sendToolResult(result)` | 向自身 window 发 `ui/notifications/tool-input`（`{arguments}`）/ `ui/notifications/tool-result`（`CallToolResult`） |
| 同文档 | App 发起的 `tools/call` | 追加到 `window.__toolCalls` 并调用 `window.__onToolCall(params)`；未注入时返回 `-32601` 明确错误，handler 抛错返回 `-32603` |
| 同文档 | `window.__log` / `window.__msgs` | 收发的 method/方向日志、原始 `postMessage` 数据 |
| 宿主页 | `window.__periBridgeTarget` | 唯一差别：返回 iframe 的 `contentWindow`；App 视图不设置它，bridge 直接用自身 window |
| sandbox shim | `peri:pointer` | 仅诊断页：`elementFromPoint(x, y)` 后依次派发 `pointerdown`、`mousedown`、`pointerup`、`mouseup`、`click`，click 只派发一次；App 视图不注入 |

顶层时 `window.parent === window`，App 的 transport 把请求 postMessage 给自身，bridge 用 `window.postMessage(response, "*")` 回复。源窗口无法区分 bridge 与 App，所以只处理带 `method` 的请求/通知，响应（只有 `result`/`error`）一律忽略；bridge 在收到消息前就已定义 `window.__bridge`，`initialized` 标志不会被迟到初始化覆盖。iframe 模式仍用 `event.source === iframe.contentWindow` 校验，不用 origin 字符串（opaque origin 为 `"null"`）。

同文档还有一个竞态：App 自己的 MCP client 也会收到它发往 `window.parent` 的请求并同步回 `-32601 Method not found`。bridge 的监听器注册更早，所以 `ui/initialize` 与 `tools/call` 都必须在**同一次消息分发内**同步回复，否则 App 会先被自答的 `-32601` 结算。`window.__onToolCall` 因此必须同步返回 `CallToolResult`；返回 thenable 时 bridge 仍会回复，但结果通常到得太晚（仅作尽力而为）。生产宿主用双 origin 时没有这条限制。

输入：App 视图不再使用指针 shim。`click` 在顶层文档派发真实 CDP 输入（`mouseMoved` → `mousePressed` → `mouseReleased`），hover 与激活行为由浏览器产生；`hover` 仍只用 `mouseMoved`。

安全取舍：同文档意味着 App 可以改写 bridge。该视图只由独立 loopback listener 提供（`127.0.0.1:7392`），沿用 sandbox CSP（`connect-src 'none'`、无外部资源），不读取也不回发任何凭据，不访问网络；这是验证期取舍，生产宿主应改回双 origin 或引擎级隔离。`--app-html` 在监听前读入内存：超过 5 MiB、读失败或非 UTF-8 时直接退出；文档必须是自带脚本与样式的单文件（CSP 不允许外部资源）。

### 外部宿主的 view session API

外部宿主（Peri TUI 等）可以不经 MCP 会话驱动一个 App：`POST /view/sessions` 提交 App HTML，网关把它注册成一份文档、新建独立页面 target 并渲染，宿主流式收帧、发真实输入、回填 App 的 `tools/call`。

**控制面在主 listener（默认 `127.0.0.1:7391`），文档源仍在 sandbox listener（`127.0.0.1:7392`）。** 理由：创建会话、发输入、中继结果都需要 token 鉴权与生命周期管理，放在唯一有业务路由的 listener 上；7392 只按 session id 返回 HTML（`/apps/view?view=<id>`），不持有会话、不接受宿主命令，保持“无凭据的文档源”这一边界。会话 URL 形如 `http://127.0.0.1:7392/apps/view?view=<uuid>`。

**鉴权**：`PERI_BROWSER_VIEW_TOKEN`（或 `--view-token`）配置 bearer token，`/view/sessions*` 的全部方法都要带 `Authorization: Bearer <token>` 或 `X-Peri-View-Token`。缺失 → `401 token_required`；不匹配 → `403 invalid_token`；未配置 → `403 view_api_disabled`（API 默认关闭）。比较是常量时间，token 不回显、不入日志、不写进错误信息、也不进入 App 文档。`/apps/view`（含 `?view=<id>`）不加鉴权：它是浏览器导航的文档源，页面没有凭据、CSP `connect-src 'none'`，加 token 只能把它塞进 URL/history；session id 是 UUIDv4，不可枚举。

**契约**（JSON；错误统一为 `{"error":"<code>","message":"..."}`）：

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| `POST` | `/view/sessions` | 请求体是 App HTML（≤5 MiB、UTF-8）。`201 {sessionId, url, eventsUrl, state:"starting"}`；超限 `413`（body 超过 5 MiB 时 `413 html_too_large`；再大由 axum body limit 直接回 413），非 UTF-8 `400 html_not_utf8`，空体 `400 empty_html`，sandbox 关闭 `409 sandbox_disabled`，并发上限（4）`429 session_limit`。 |
| `GET` | `/view/sessions` | `{sessions:[status…]}`，含失败记录（下次创建时淘汰）。 |
| `GET` | `/view/sessions/{id}` | `{sessionId,url,state,frames,subscribers,pendingRequests,targetId}`；失败时附 `code`/`message`。未知/已移除 `404 session_not_found`。 |
| `DELETE` | `/view/sessions/{id}` | 取消任务、关闭页面 target、移除文档；等页面释放后返回 `{closed:true,sessionId}`，之后同一 id 返回 404。 |
| `GET` | `/view/sessions/{id}/events` | SSE，见下表。 |
| `POST` | `/view/sessions/{id}/click` | `{x,y}`（CSS 视口坐标）= 真实 CDP 点击；`{"action":"move"}` 只派发 `mouseMoved`；`{"action":"scroll","deltaX","deltaY"}` 派发 `mouseWheel`。`200 {clicked:true,x,y}`。会话未就绪 `409 session_starting`。 |
| `POST` | `/view/sessions/{id}/evaluate` | `{expression}` → `{result}`（`Runtime.evaluate`，`awaitPromise`+`returnByValue`+`userGesture`）。页面异常 `400 script_failed`。 |
| `POST` | `/view/sessions/{id}/app-results` | 回填一个 `app-request`：`{seq, result}` 或 `{seq, error:{code,message,data?}}`，二者只能有一个。`200 {delivered:true}`；未知/已回答 `404 unknown_request`。 |
| `POST` | `/view/sessions/{id}/notify` | 宿主主动通知 App：`{method, params}`，例如 `ui/notifications/tool-input` / `tool-result`。 |

SSE 事件（`data` 均为 JSON）：

| 事件 | 载荷 |
| --- | --- |
| `meta` | 订阅时的一次状态快照（同 `GET /view/sessions/{id}`）；若有当前帧，随后立刻补发一帧。 |
| `state` | 状态变化：`state` ∈ `starting`/`ready`/`failed`（附 `code`/`message`）。 |
| `frame` | `{seq, width, height, viewportWidth, viewportHeight, format:"jpeg", data:"<base64 JPEG>"}`；`seq` 在会话内单调递增，慢订阅者只拿最新帧（序号可跳号）。 |
| `app-request` | `{seq, id, method, params}`：App 发起的请求（目前只有 `tools/call`），`seq` 是网关侧关联号。 |
| `closed` | `{reason}`：`deleted`、`gateway_shutdown`、`target_expired`、`controller_closed`、`start_failed`。流随后结束。 |

生命周期语义：`DELETE` 之后 SSE 一定先收到 `closed` 再结束；SSE 断开**不**结束会话，重连会先收到当前帧、再重放所有未回答的 `app-request`（请求在宿主回答前一直保留，最多 64 个，溢出时该请求立刻收到 `-32603`）；对同一 `seq` 重复回填返回 404，不会二次投递。会话在网关退出时取消并释放文档，页面 target 留在 Kitesurf（与 MCP 会话一致）。

帧与输入复用现有 CDP 路径：只读 viewer 上 `Page.startScreencast`（JPEG，`Page.screencastFrameAck`），writer controller 上 `Input.dispatchMouseEvent`（`mouseMoved` → `mousePressed` → `mouseReleased`）与 `Input.dispatchMouseWheel`，均为真实输入，不做 JS 合成。

#### `tools/call` 中继：bridge 在出站处拦截

同文档竞态（见上节）对 HTTP 宿主无解：宿主不可能在 bridge 的同一消息分发内同步回复。会话文档的 bridge 因此在 App transport **出站前**拦截：

- App 构造 transport 时拿到的是 `window.parent`（顶层即自身），`send()` 走 `window.postMessage`；bridge 在 App 模块之前执行，把 `window.postMessage` 换成一个只吞掉“自身出站 JSON-RPC”（`jsonrpc==="2.0" && typeof method==="string"`，请求与通知都算）的包装，改投递到页面队列。App 自己的 MCP client 因此**收不到自己的请求**，不会同步自答 `-32601`；bridge 自己的投递保存了原生 `postMessage`，不受影响。
- `ui/initialize` 仍由 bridge 同步应答；`tools/call` 入队并 `console.debug("peri-view:app-request <n>")` 唤醒网关；其他请求同步回 `-32601`。
- 网关在 writer controller 上监听 `Runtime.consoleAPICalled` 唤醒，调用 `window.__bridge.takeRequests()`（drain 语义）把请求取走，并按 1 秒周期兜底轮询；引擎不接受 `Runtime.enable` 时仍能工作。
- 宿主结果经 `Runtime.evaluate` 调 `window.__bridge.hostReply(<message>)` 投回；消息必须是**纯** JSON-RPC：SDK 的响应 schema 是 `.strict()`，附加字段（例如 `__periHost`）会让 App 直接丢弃整条消息。
- 页面与网关之间没有网络通道（CSP `connect-src 'none'` 原样保留），console 只是唤醒信号，队列由 CDP evaluate 取走。

实测（Kitesurf 0.5.4，`side-projects/mcp-apps` 的 App HTML）：不拦截时 App 的 `tools/call` 被自身 `-32601` 结算成 `ERR MCP error -32601`；拦截出站（`postMessage` 包装，或在 `addEventListener("message")` 上包裹 App 的监听器）后，异步（300 ms）回复即可让 App 显示 `LINK ESTABLISHED`，且窗口内不再出现任何 `-32601` 响应。两种拦截都可行，这里选 `postMessage` 包装：不触碰监听器注册表、无需还原 `removeEventListener`，且请求在源头就不会被投递。把 1 秒兜底轮询临时调到 30 秒后重跑同一 E2E，请求仍在 **27–28 ms** 内到达宿主，证明 console 唤醒路径生效。

非会话文档（`--app-html` 的 `/apps/view`、iframe 宿主页）保持原有行为：不注入 `__periViewSession`，不同步拦截，`tools/call` 仍走同步 `window.__onToolCall`；现有 App 视图验收路径不受影响。

## MCP 面

工具名与参数 schema 保持不变：

| 分类 | 工具 |
| --- | --- |
| 会话 | `browser_status`、`browser_connect`、`yield_page`、`request_human`、`request_authorization` |
| 页面 | `list_pages`、`select_page`、`new_page`、`navigate_page`、`close_page` |
| 读取 | `take_snapshot`、`take_screenshot`、`evaluate_script` |
| 交互 | `click`、`fill`、`hover`、`press_key`、`wait_for` |
| 诊断 | `list_console_messages`、`list_network_requests`、`get_network_request` |

- `browser_status` 是唯一状态来源：端点、安装/监听建议、`nextSteps`、页面与 owner、是否让出、monitor 状态。
- `browser_connect` 是唯一连接入口，只接受环回 HTTP/WS；未连接时其余工具返回 `not_connected`。
- 技能随包暴露：`skills/list`、`skills/get`、`resources/read`（`skill://peri-browser/...`），size 与 digest 来自同一打包字节。
- `discover`（`server/discover`）返回关联 id 的 JSON-RPC `METHOD_NOT_FOUND`，Peri 的 Auto 协商据此回退到 legacy `initialize`，不靠超时或空 404。

## 会话生命周期

| 事件 | 契约 |
| --- | --- |
| `initialize` | 成功后分配独立 `BrowserCore`、域 UUID 与 monitor token；端点与页面为空，必须显式 `browser_connect`。 |
| 单个 HTTP/SSE 断开 | 仍 Live 的 `Mcp-Session-Id` 可继续使用；GET + `Last-Event-ID` 在 SDK 缓存窗口内可恢复。 |
| `DELETE` / 传输 EOF / 过期 | 幂等关闭：先封闭准入，再结算请求、handoff、viewer、页面连接；旧 ID 之后返回 404。 |
| 空闲过期 | 单一 SDK keep-alive 时钟，默认不小于 360 秒，并覆盖最长业务预算；ping 与 15 秒页面 idle 不续期。 |
| 网关重启 | 旧 ID/token 与页面不继承；新会话为空，target 留在 Kitesurf。 |

不承诺跨进程恢复或 exactly-once；已发给 CDP 的写不回滚。

## 人工接管与监控

- Agent 在途复合操作期间 monitor 显示 pending；过期输入不执行。
- `request_human` 阻塞当前调用，返回人的结果或 `no_human`；人持有 writer 期间 agent 写返回 busy。
- `yield_page`、`request_human`、静置让出与 monitor `Release` 都经同一 `PageControl`；任何时刻只有一个 writable 页面连接。
- 外部 DevTools 抢占由引擎返回 WebSocket `409`；网关识别为 `AnotherController` 并保持页面与 viewer 可用。

## 权限与暴露边界

- 监听地址只接受 `127.0.0.1` 或 `[::1]`；非环回绑定在启动时拒绝。MCP、monitor 与 `/apps/*` 没有 bearer/token 鉴权层，本机回环就是边界；`/view/sessions*` 是唯一例外：必须带 `PERI_BROWSER_VIEW_TOKEN`（未配置时 API 关闭）。
- view session 的页面 target 不在任何 MCP 会话的页面集合里；宿主只能操作自己创建的会话。`evaluate` 与 `click` 和“宿主自己提交 App HTML”同属一个信任域：页面在 loopback 文档源上、无凭据、CSP 禁止网络。
- Host/Origin 校验：`Host` 必须匹配监听端口与环回地址；非 `/mcp` 路由要求同源 `Origin`。
- monitor token = `base64url(sha256(secret ++ sessionId))[..22]`，只对当前 Live 记录有效；未知或过期 token 返回 404。token 出现在 monitor URL 中，不要把该端口转发到共享网络。
- `RUST_LOG` 只调整网关自身诊断级别；依赖的 HTTP/WS/CDP 原始请求与 header 不进入日志。

## 配置

以下参数只属于网关（`serve` / 裸调用 / `start`）；`start kitesurf` 的 `--port` / `--bind` 见上文命令面。CLI 参数优先于同名环境变量；超时值范围 `1..=86400000` 毫秒，非法值拒绝启动。

| 参数 | 环境变量 | 默认 | 说明 |
| --- | --- | --- | --- |
| `--bind IP:PORT` | `PERI_BROWSER_MONITOR_BIND` | `127.0.0.1:7391` | 主 listener；端口 `0` 交给 OS，但仅限 `serve` / 裸调用，`start` 与 `start kitesurf` 拒绝端口 `0`。 |
| `--sandbox-bind IP:PORT` | `PERI_BROWSER_SANDBOX_BIND` | `127.0.0.1:7392` | 顶层 App 视图与 sandbox 诊断页的 listener；只接受环回非零端口，`off`/`none` 关闭。关闭或绑定失败时宿主页仍可用，只是不嵌入 iframe，App 视图也不再提供。 |
| `--app-html PATH` | `PERI_BROWSER_APP_HTML` | 无（占位页） | `/apps/view` 与 `/apps/sandbox` 返回的真实 MCP App HTML；监听前读入内存，超过 5 MiB、读失败或非 UTF-8 直接退出。sandbox listener 被关闭时忽略并告警。 |
| `--view-token VALUE` | `PERI_BROWSER_VIEW_TOKEN` | 无（API 关闭） | `/view/sessions*` 的 bearer token；推荐环境变量，不进日志、不回显。空字符串拒绝启动。 |
| `--cdp-url URL` | `KITESURF_CDP_URL` | `http://127.0.0.1:18971` | `browser_status` 的建议地址；不自动连接、不预绑定。 |
| `--secret VALUE` | `PERI_BROWSER_GATEWAY_SECRET` | 每进程随机 | monitor token 派生输入；推荐环境变量，不留 secret 文件。 |
| `--idle-yield-ms MS` | `PERI_BROWSER_IDLE_YIELD_MS` | 15000 | Agent 静置让出。 |
| `--yield-timeout-ms MS` | `PERI_BROWSER_YIELD_TIMEOUT_MS` | 60000 | `yield_page` 绝对预算。 |
| `--request-human-timeout-ms MS` | `PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS` | 300000 | `request_human` 服务上限。 |

其余会话/页面/安装相关变量（`PERI_BROWSER_OWNER`、`PERI_BROWSER_SESSION_NAME`、`KITESURF_INSTALL_DIR`、`PERI_BROWSER_PROVISION_LOCK*`、`PERI_BROWSER_CONNECT_TIMEOUT_MS`、`PERI_BROWSER_PAGE_WAIT_MS`、`PERI_BROWSER_RECLAIM_WAIT_MS`、`PERI_BROWSER_LOCK_STALE_MS`）见 [环境变量表](../../docs/standards/environment-variables.md#browser-gateway)。旧 `PERI_BROWSER_GATEWAY_SOCK`、`PERI_BROWSER_HTTP_ADDR` 与自动拉起网关不再生效。

`start kitesurf` 的 `--bind` / `--port` 是独立进程的启动参数，不读取 `PERI_BROWSER_MONITOR_BIND`；它固定用 `--cdp-url http://127.0.0.1:<port>` 启动后台网关。`start kitesurf` 拉起的引擎默认带 `--allow-private-network`；设 `PERI_BROWSER_ALLOW_PRIVATE_NETWORK=0`（或 `false`/`off`/`no`）可关闭。

## 引导（onboarding）

`browser_status` 在无引擎时也能启动：返回安装检测、建议端点、监听状态与可执行的 `nextSteps`（安装 → 启动 → `browser_connect`）。MCP 会话本身不运行安装器、不拉起 Kitesurf；安装与启动只经 CLI 的 `install kitesurf` / `start kitesurf` 显式执行。网关启动时仍不探测或绑定端点，推荐的 `--cdp-url` 只是提示，实际连接由会话显式建立。

## 从 peri-browser 迁移

1. 删除 MCP 配置里的 stdio `peri-browser` 条目，改为上面的 `type: http` + `url`。
2. 删除 `PERI_BROWSER_GATEWAY_SOCK`、`PERI_BROWSER_HTTP_ADDR` 等旧开关；网关不再通过 UDS 注册或转发。
3. 旧 `peri-browser` 二进制与 crate 已移除；需要同时保留新旧两套时，旧版请从历史提交自行构建，本仓库不再提供兼容层。
4. 同一个 Kitesurf 引擎可服务多个独立 MCP 会话；每个会话各自 `browser_connect`，不会共享页面或活动页。

## 验证

```bash
./scripts/cargo-rmcp-patched.sh test --offline --locked -p peri-browser-gateway
./scripts/cargo-rmcp-patched.sh clippy --offline --locked -p peri-browser-gateway --all-targets -- -D warnings
cargo fmt -p peri-browser-gateway -- --check
bash scripts/check-layer-imports.sh
```

view session 契约由 `src/view_test.rs` 覆盖：token（401/403/未配置）、HTML 上限与编码、5 MiB 边界、会话创建→ready、SSE `meta`/`frame`/`app-request`/`closed`、真实输入（`mouseMoved`→`mousePressed`→`mouseReleased` 与坐标）、结果回填后的请求出队、`evaluate`、DELETE 后 404，以及应用文档的 `?view=` 路由与 relay bridge 注入（假引擎按脚本应答 CDP，不需要浏览器）。

`tests/mcp_http.rs` 用真实 listener 与 Peri 等价 Auto 客户端覆盖协商、绑定、隔离、handoff、shutdown 与 onboarding；`tests/contracts/daemon.rs` 用隔离 `HOME` 与临时端口覆盖 `start`/`stop` 的返回时机、端口释放、状态文件与幂等；fixture 以第二 writer 的 HTTP 409 和共享 targetId 模拟真实引擎边界。本次验收证据与未运行项见 [验收记录](../../docs/verification/peri-browser-gateway-mcp.md)。

## 非目标

远程访问与鉴权、跨进程状态恢复、多 profile、现代 stateless 工具调用、旧 UDS/stdio 兼容，以及依赖宿主侧 MCP 适配层的浏览器逻辑。
