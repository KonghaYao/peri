# MCP Apps Stable stdio relay

**状态**：Implemented（环境 capability、resource relay、Binding lease + canonical HITL tools/call 已接通）
**优先级**：高
**类型**：协议适配 / ACP stdio / MCP capability
**更新日期**：2026-08-27

## 最终决策

```text
PERI_MCP_APPS 环境变量存在
  → 当前进程使用 immutable Apps deployment profile
  → MCP pool prewarm / reconnect / Dynamic MCP initialize 声明
    io.modelcontextprotocol/ui + text/html;profile=mcp-app
  → stdio 装配 MCP Apps relay backend

环境变量不存在
  → 不声明 extension
  → 不装配 relay
  → peri/mcp/* fail closed
```

环境变量**只看是否存在，值不解析**；空串、`0` 等均表示启用。进程内 profile 通过 `OnceLock` 冻结。完全移除 ACP `peri.mcpApps` capability 的解析、协商和回显。

## 范围

### Peri 实现

- 环境 deployment profile。
- MCP initialize/reconnect capability propagation。
- `_meta.ui.resourceUri` canonical/legacy 兼容读取与冲突 fail closed。
- `_meta.ui.visibility` model/app 解释；malformed fail closed。
- `peri/mcp/open`：按 server/tool 建立 connection-owned resource binding。
- `peri/mcp/resource`：读取并返回 `ui://` HTML resource，保留 `text|blob` 与 `_meta`。
- `peri/mcp/app` typed envelope；`tools/call` 经单次消费 Binding lease 进入 canonical `EffectiveToolDispatcher` 与 Permission/HITL。
- ACP EOF 清理该 connection 的 binding 和 pending relay。

### 明确不实现

- Web Host、iframe、sandbox、CSP/Permissions Policy 执行、浏览器 `postMessage`、MCP Apps FE SDK。
- `peri-tui` Apps 能力或渲染。
- ACP MCP Apps capability。
- 任意 MCP proxy。
- 绕过 effective tool view / Permission / HITL 的直接 `Peer::call_tool()`。

## Wire contract

### Open

```json
{
  "method": "peri/mcp/open",
  "params": {
    "envelopeVersion": "1",
    "appsProtocolVersion": "2026-01-26",
    "serverId": "server",
    "toolName": "open_dashboard"
  }
}
```

Peri 校验：server 已连接、tool 存在、visibility 包含 `app`、tool metadata 含合法且无冲突的 `ui://` resource URI。成功返回 opaque `appSessionId`、resource URI 和 MCP core version。

### Resource

`peri/mcp/resource` 必须携带 `appSessionId + serverId + resourceUri`。Peri 校验 connection owner、server generation、resource binding，然后执行 `resources/read`。首版要求：

- URI 精确匹配且为 `ui://`；
- MIME 为 `text/html;profile=mcp-app`；
- `text` / `blob` 恰有一个；
- `_meta` 与未知字段保留。

### App tools/call

DTO 与 request/response id 分层已接通。初始模型 MCP tool invocation 在 `ToolContext` 中取得 canonical dispatcher、session/turn identity 与 cancellation；仅当 tool 同时 app-visible 且绑定合法 `ui://` resource 时签发短期 lease。`peri/mcp/open` 必须携带该 invocation 的 opaque correlation token 与 owner session identity，并单次消费精确匹配 server/tool/resource/generation/session/token 的 lease；同 connection 内并发 open 也不能消费彼此 lease。后续 `tools/call` 通过 lease dispatcher 执行 canonical full tool name。

allowed tool set 是“同 server、同 resource URI、visibility 包含 `app`”与 lease 签发时 canonical effective catalog 的交集。它包含 app-only tool，但 app-only tool 不进入模型 definitions；调用仍进入同一 `EffectiveToolDispatcher`、Permission 与 HITL 路径。

MCP bridge 在专用 `mcp-app:` invocation identity 下将 raw `CallToolResult` 写入无日志 side channel；dispatcher 完成后 relay 取走并构造 Apps JSON-RPC response，完整保留 `content`、`structuredContent`、`_meta`、`isError` 和未知字段，不把 dispatcher 的文本结果伪装成 raw result。

## Binding lease

```text
InitialMcpAppBindingLease {
  agent_session_id,
  turn_generation,
  server_id,
  server_generation,
  resource_uri,
  instantiating_tool,
  opaque_invocation_token,
  app_visible_effective_tool_set,
  effective_dispatcher,
  cancellation,
  expires_at
}
```

租约由 canonical initial invocation 签发，`peri/mcp/open` 按 server/tool/resource/generation/session/opaque token 单次消费并绑定 ACP connection；TTL 为 5 分钟。registry 记录 session 的最新 turn generation，较旧 pending/active lease 均失效；turn/session cancellation、server generation 变化、过期同样使租约无效。pending/active lease 与 raw result side channel 均有 TTL、容量上限和 opportunistic cleanup；raw result invocation identity 含 connection owner，ACP EOF 撤销该 connection 的 active relay lease、清除 connection-owned binding 和未取走 raw result。

server generation 在每次 pool connection commit/失败替换时显式递增；验证只查 server name 对应的 generation，不使用 `Arc` 地址或指针相等推断。stdio 装配点读取一次环境并构造 immutable profile；pool、initial/reconnect 和 Dynamic MCP connector 复用同一 profile，TUI/MPSC 入口固定 disabled 且不读取/传播环境。Apps requests 由 connection-owned task 异步执行；除 `open` 的短临界提交外不跨网络 await 持 connection 锁，EOF cancellation 与 response 发送竞争后每个已接收 request 至多产生一个 terminal response。

## 安全不变量

1. 环境变量不存在时，extension 与 relay 都不存在。
2. Apps profile 在 prewarm 前冻结，初始连接和重连一致。
3. binding 属于 ACP connection，不能跨 connection 使用。
4. server handle replacement 改变 generation，旧 binding 返回 `stale_server_generation`。
5. App visibility 不含 `app` 时拒绝 open。
6. `isError: true` 属正常 `CallToolResult`，不得映射成 transport error。
7. error 层级由 ACP outer error 与 payload JSON-RPC error 的结构位置区分。
8. 不记录 HTML 正文、tool 参数、OAuth token、header、stdio env 或其他 secret。

## 验证

已覆盖：

- env profile presence/absence helper；
- MCP UI extension 构造；
- visibility 与 resource URI canonicalization；
- raw resource/result roundtrip；
- connection-owned binding 与 EOF 清理；
- Apps envelope/version/JSON-RPC result/error 校验；
- `peri-acp` 全量 lib 测试。

最终验收还需：

- fake MCP server 断言环境变量存在时 initial/reconnect initialize extension；
- stdio wire `open → resource` E2E；
- Binding lease 的 HITL approve/deny、cancel、expiry、server generation E2E；
- App 主动调用不重复进入模型事件投影。

## 目标命令

```bash
cargo fmt --check
cargo test -p peri-acp-types --lib mcp_apps
cargo test -p peri-middlewares --lib mcp::apps
cargo test -p peri-middlewares --lib mcp::tool_bridge
cargo test -p peri-acp --lib
cargo check -p peri-acp-types -p peri-middlewares -p peri-acp
cargo clippy -p peri-acp-types -p peri-middlewares -p peri-acp --all-targets -- -D warnings
git diff --check
```
